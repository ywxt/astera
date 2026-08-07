use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    mem,
};

use astera_core::OutputId;
use smithay::{
    backend::renderer::buffer_dimensions,
    reexports::{
        wayland_protocols::ext::session_lock::v1::server::{
            ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
            ext_session_lock_v1::{self, ExtSessionLockV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, Resource,
            protocol::{wl_output::WlOutput, wl_surface::WlSurface},
        },
    },
    utils::IsAlive,
    wayland::session_lock::{
        LockSurface as SmithayLockSurface, SessionLockHandler, SessionLockManagerState,
        SessionLockState, SessionLocker,
    },
    wayland::{
        compositor::{self, BufferAssignment, SurfaceAttributes},
        viewporter::ViewportCachedState,
    },
};

use super::Astera;

#[derive(Default)]
pub(super) enum SessionState {
    #[default]
    Unlocked,
    Locking {
        confirmation: SessionLocker,
        owner: ExtSessionLockV1,
        pending: BTreeSet<OutputId>,
        generation: u64,
    },
    Locked {
        owner: ExtSessionLockV1,
    },
}

impl Astera {
    /// Nested winit cannot observe real host presentation feedback, so advertising a security
    /// protocol there would promise a guarantee that backend cannot establish.
    pub(crate) fn disable_session_lock_advertisement(&self) {
        self.session_lock_advertised
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn session_is_locked(&self) -> bool {
        !matches!(self.session_state, SessionState::Unlocked)
    }

    pub(crate) fn locking_generation(&self) -> Option<u64> {
        match self.session_state {
            SessionState::Locking { generation, .. } => Some(generation),
            _ => None,
        }
    }

    pub fn lock_frame_presented(&mut self, output: OutputId, generation: Option<u64>) {
        let SessionState::Locking {
            pending,
            generation: expected,
            ..
        } = &mut self.session_state
        else {
            return;
        };
        if generation != Some(*expected) {
            return;
        }
        pending.remove(&output);
        if !pending.is_empty() {
            return;
        }
        let SessionState::Locking {
            confirmation,
            owner,
            ..
        } = mem::take(&mut self.session_state)
        else {
            unreachable!()
        };
        confirmation.lock();
        self.session_state = SessionState::Locked { owner };
        tracing::info!("session lock secured on every output");
    }

    pub(super) fn lock_surface_for_output(&self, output: OutputId) -> Option<&AsteraLockSurface> {
        self.lock_surfaces
            .get(&output)
            .filter(|surface| surface.alive())
    }

    fn lock_owner_is(&self, lock: &ExtSessionLockV1) -> bool {
        match &self.session_state {
            SessionState::Unlocked => false,
            SessionState::Locking { owner, .. } | SessionState::Locked { owner } => owner == lock,
        }
    }

    fn unlock_owner(&mut self, lock: &ExtSessionLockV1) {
        if !self.lock_owner_is(lock) || !matches!(self.session_state, SessionState::Locked { .. }) {
            lock.post_error(
                ext_session_lock_v1::Error::InvalidUnlock,
                "only the confirmed lock owner may unlock the session",
            );
            return;
        }
        self.cancel_touch_sequences();
        self.session_state = SessionState::Unlocked;
        self.lock_surfaces.clear();
        self.mark_render_dirty();
        self.sync_keyboard_focus();
        // Re-hit-test immediately so axis/relative input cannot target the destroyed lock surface.
        self.handle_pointer_motion(self.pointer_location, 0);
        tracing::info!("session unlocked");
    }

    fn cancel_pending_lock(&mut self, lock: &ExtSessionLockV1) {
        if !matches!(self.session_state, SessionState::Locking { ref owner, .. } if owner == lock) {
            return;
        }
        self.cancel_touch_sequences();
        self.session_state = SessionState::Unlocked;
        self.lock_surfaces.clear();
        self.mark_render_dirty();
        self.sync_keyboard_focus();
        self.handle_pointer_motion(self.pointer_location, 0);
        tracing::info!("pending session lock cancelled by its owner");
    }

    fn create_lock_surface(
        &mut self,
        lock: &ExtSessionLockV1,
        id: smithay::reexports::wayland_server::New<ExtSessionLockSurfaceV1>,
        wl_surface: WlSurface,
        wl_output: WlOutput,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if !self.lock_owner_is(lock) {
            lock.post_error(
                ext_session_lock_v1::Error::InvalidUnlock,
                "a rejected lock cannot create lock surfaces",
            );
            return;
        }
        let already_constructed = compositor::with_states(&wl_surface, |states| {
            let mut attributes = states.cached_state.get::<SurfaceAttributes>();
            matches!(
                attributes.pending().buffer,
                Some(BufferAssignment::NewBuffer(_))
            ) || matches!(
                attributes.current().buffer,
                Some(BufferAssignment::NewBuffer(_))
            )
        });
        if already_constructed {
            lock.post_error(
                ext_session_lock_v1::Error::AlreadyConstructed,
                "lock surface already has a buffer",
            );
            return;
        }
        let Some(protocol_output) = smithay::output::Output::from_resource(&wl_output) else {
            lock.post_error(
                ext_session_lock_v1::Error::DuplicateOutput,
                "requested output is no longer available",
            );
            return;
        };
        let Some(output) = self
            .output_runtime
            .iter()
            .find_map(|(id, runtime)| (runtime.wayland == protocol_output).then_some(*id))
        else {
            lock.post_error(
                ext_session_lock_v1::Error::DuplicateOutput,
                "requested output is not managed by this compositor",
            );
            return;
        };
        if compositor::give_role(&wl_surface, "ext_session_lock_surface_v1").is_err() {
            lock.post_error(
                ext_session_lock_v1::Error::Role,
                "surface already has a role",
            );
            return;
        };
        if self.lock_surfaces.contains_key(&output) {
            lock.post_error(
                ext_session_lock_v1::Error::DuplicateOutput,
                "output already has a lock surface",
            );
            return;
        }
        let protocol = data_init.init(
            id,
            LockSurfaceData {
                wl_surface: wl_surface.clone(),
            },
        );
        let mut surface = AsteraLockSurface {
            protocol,
            wl_surface,
            pending_configures: VecDeque::new(),
            acked_size: None,
        };
        surface.configure(output, self);
        self.lock_surfaces.insert(output, surface);
        self.mark_render_dirty();
        self.sync_keyboard_focus();
    }

    pub(super) fn session_output_connected(&mut self, output: OutputId) {
        if let SessionState::Locking { pending, .. } = &mut self.session_state {
            pending.insert(output);
        }
    }

    pub(super) fn session_output_disconnected(&mut self, output: OutputId) {
        self.lock_surfaces.remove(&output);
        let generation = self.locking_generation();
        self.lock_frame_presented(output, generation);
    }

    fn secure_input_for_lock(&mut self) {
        self.key_repeat.cancel_repeats();
        self.drag = None;
        let serial = self.next_serial();
        let keyboard = self.keyboard.clone();
        keyboard.unset_grab(self);
        let pointer = self.pointer.clone();
        pointer.unset_grab(self, serial, 0);
        // Touch focus is fixed at the first contact, so terminate any desktop sequence before the
        // lock client becomes the only eligible input recipient.
        self.cancel_touch_sequences();
        // Re-hit-test immediately so axis/button events cannot use a stale desktop focus.
        self.handle_pointer_motion(self.pointer_location, 0);
    }

    pub(super) fn validate_lock_surface_commit(&self, wl_surface: &WlSurface) {
        let Some(surface) = self
            .lock_surfaces
            .values()
            .find(|surface| surface.wl_surface == *wl_surface)
        else {
            return;
        };
        let Some(expected) = surface.acked_size else {
            surface.protocol.post_error(
                ext_session_lock_surface_v1::Error::CommitBeforeFirstAck,
                "lock surface committed before acknowledging configure",
            );
            return;
        };
        compositor::with_states(wl_surface, |states| {
            let mut attributes = states.cached_state.get::<SurfaceAttributes>();
            let attributes = attributes.current();
            let Some(BufferAssignment::NewBuffer(buffer)) = attributes.buffer.as_ref() else {
                surface.protocol.post_error(
                    ext_session_lock_surface_v1::Error::NullBuffer,
                    "lock surfaces must always have a buffer",
                );
                return;
            };
            let Some(buffer_size) = buffer_dimensions(buffer) else {
                return;
            };
            let logical = if let Some(destination) = states
                .cached_state
                .get::<ViewportCachedState>()
                .current()
                .dst
            {
                (destination.w as u32, destination.h as u32)
            } else {
                let size = buffer_size
                    .to_logical(attributes.buffer_scale, attributes.buffer_transform.into());
                (size.w as u32, size.h as u32)
            };
            if logical != expected {
                surface.protocol.post_error(
                    ext_session_lock_surface_v1::Error::DimensionsMismatch,
                    format!(
                        "expected {}x{}, got {}x{}",
                        expected.0, expected.1, logical.0, logical.1
                    ),
                );
            }
        });
    }
}

impl SessionLockHandler for Astera {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_manager
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        let existing_alive = match &self.session_state {
            SessionState::Unlocked => false,
            SessionState::Locking { owner, .. } | SessionState::Locked { owner } => {
                owner.is_alive()
            }
        };
        if existing_alive {
            return;
        }

        // A crashed locker never reveals the desktop, but its dead surfaces must not prevent a
        // replacement locker from covering the outputs.
        self.lock_surfaces.clear();
        let owner = confirmation.ext_session_lock().clone();
        self.next_lock_generation = self.next_lock_generation.wrapping_add(1).max(1);
        let generation = self.next_lock_generation;
        let pending = self
            .output_runtime
            .keys()
            .filter(|output| self.output_power_modes.get(output).copied().unwrap_or(true))
            .copied()
            .collect::<BTreeSet<_>>();
        if pending.is_empty() {
            confirmation.lock();
            self.session_state = SessionState::Locked { owner };
        } else {
            self.session_state = SessionState::Locking {
                confirmation,
                owner,
                pending,
                generation,
            };
            self.mark_render_dirty();
        }
        self.secure_input_for_lock();
        self.sync_keyboard_focus();
        tracing::info!("session lock requested; desktop input is blocked");
    }

    fn unlock(&mut self) {}

    fn new_surface(&mut self, _surface: SmithayLockSurface, _output: WlOutput) {}
}

pub(super) struct AsteraLockSurface {
    protocol: ExtSessionLockSurfaceV1,
    wl_surface: WlSurface,
    pending_configures: VecDeque<(u32, (u32, u32))>,
    acked_size: Option<(u32, u32)>,
}

impl AsteraLockSurface {
    pub(super) fn alive(&self) -> bool {
        self.protocol.is_alive() && self.wl_surface.alive()
    }

    pub(super) fn wl_surface(&self) -> &WlSurface {
        &self.wl_surface
    }

    fn configure(&mut self, output: OutputId, state: &mut Astera) {
        let size = state.desktop.outputs[&output].output.logical_size;
        let width = u32::try_from(size.width).unwrap_or(u32::MAX);
        let height = u32::try_from(size.height).unwrap_or(u32::MAX);
        let serial = state.next_serial();
        let serial = u32::from(serial);
        self.pending_configures.push_back((serial, (width, height)));
        self.protocol.configure(serial, width, height);
    }
}

impl Astera {
    pub(super) fn configure_lock_surface(&mut self, output: OutputId) {
        let Some(mut surface) = self.lock_surfaces.remove(&output) else {
            return;
        };
        surface.configure(output, self);
        self.lock_surfaces.insert(output, surface);
    }
}

pub(super) struct LockSurfaceData {
    wl_surface: WlSurface,
}

impl Dispatch<ExtSessionLockV1, SessionLockState> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        lock: &ExtSessionLockV1,
        request: ext_session_lock_v1::Request,
        _data: &SessionLockState,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_session_lock_v1::Request::GetLockSurface {
                id,
                surface,
                output,
            } => {
                state.create_lock_surface(lock, id, surface, output, data_init);
            }
            ext_session_lock_v1::Request::UnlockAndDestroy => state.unlock_owner(lock),
            ext_session_lock_v1::Request::Destroy
                if matches!(state.session_state, SessionState::Locking { .. }) =>
            {
                state.cancel_pending_lock(lock);
            }
            ext_session_lock_v1::Request::Destroy if state.lock_owner_is(lock) => {
                lock.post_error(
                    ext_session_lock_v1::Error::InvalidDestroy,
                    "active session lock must use unlock_and_destroy",
                );
            }
            ext_session_lock_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, LockSurfaceData> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        protocol: &ExtSessionLockSurfaceV1,
        request: ext_session_lock_surface_v1::Request,
        data: &LockSurfaceData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_session_lock_surface_v1::Request::AckConfigure { serial } => {
                let Some(surface) = state
                    .lock_surfaces
                    .values_mut()
                    .find(|surface| surface.protocol == *protocol)
                else {
                    return;
                };
                let Some(index) = surface
                    .pending_configures
                    .iter()
                    .position(|(pending, _)| *pending == serial)
                else {
                    protocol.post_error(
                        ext_session_lock_surface_v1::Error::InvalidSerial,
                        "unknown configure serial",
                    );
                    return;
                };
                let size = surface.pending_configures[index].1;
                surface.pending_configures.drain(..=index);
                surface.acked_size = Some(size);
            }
            ext_session_lock_surface_v1::Request::Destroy => {
                state
                    .lock_surfaces
                    .retain(|_, surface| surface.protocol != *protocol);
                state.mark_render_dirty();
                state.sync_keyboard_focus();
                state.handle_pointer_motion(state.pointer_location, 0);
            }
            _ => unreachable!(),
        }
        let _ = &data.wl_surface;
    }
}

pub(super) type LockSurfaces = BTreeMap<OutputId, AsteraLockSurface>;
