use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use astera_config::Config;
use astera_ipc::Command;
use smithay::{
    backend::{
        egl::EGLDevice,
        renderer::{
            Color32F, ImportDma,
            damage::OutputDamageTracker,
            element::{
                Kind, memory::MemoryRenderBufferRenderElement, render_elements,
                surface::WaylandSurfaceRenderElement,
            },
            gles::GlesRenderer,
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::PopupManager,
    reexports::{calloop::EventLoop, wayland_server::Display},
    utils::{Physical, Point, Transform},
};

use crate::{
    backend::{
        render::{complete_frame_callbacks, surface_tree_snapshot},
        sources::{ReadableFdSource, WaylandSocketSource},
    },
    ipc_server::IpcServer,
    state::{Astera, ClientState, InputServiceSupervisor, cursor::CursorRenderSource},
};

render_elements! {
    WinitRenderElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
}

const MAX_EVENTS_PER_BATCH: usize = 256;
const MAX_QUEUED_SOURCE_EVENTS: usize = 4096;
const SOURCE_WARN_AFTER: Duration = Duration::from_millis(8);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const FRAME_RETRY_INITIAL: Duration = Duration::from_millis(16);
const FRAME_RETRY_MAX: Duration = Duration::from_secs(1);

enum RuntimeEvent {
    Winit(WinitEvent),
    WaylandClient(std::os::unix::net::UnixStream),
    WaylandReady,
    IpcReady,
    ConfigChanged,
    InputServiceExited(crate::state::InputServiceExit),
}

struct WinitLoop {
    display: Display<Astera>,
    state: Astera,
    ipc: IpcServer,
    backend: WinitGraphicsBackend<GlesRenderer>,
    events: VecDeque<RuntimeEvent>,
    damage_tracker: OutputDamageTracker,
    tracked_size: smithay::utils::Size<i32, Physical>,
    started: Instant,
    present_ready: bool,
    exiting: bool,
    shutdown_deadline: Option<Instant>,
    frame_retry_deadline: Option<Instant>,
    frame_retry_delay: Duration,
    host_configured: bool,
    has_presented: bool,
    scene_dirty: bool,
    input_service: Option<InputServiceSupervisor>,
}

impl WinitLoop {
    fn enqueue(&mut self, event: RuntimeEvent) {
        let duplicate = self.events.iter().any(|queued| {
            matches!(
                (&event, queued),
                (
                    RuntimeEvent::Winit(WinitEvent::Redraw),
                    RuntimeEvent::Winit(WinitEvent::Redraw)
                ) | (RuntimeEvent::WaylandReady, RuntimeEvent::WaylandReady)
                    | (RuntimeEvent::IpcReady, RuntimeEvent::IpcReady)
                    | (RuntimeEvent::ConfigChanged, RuntimeEvent::ConfigChanged)
            )
        });
        if duplicate {
            return;
        }
        if self.events.len() >= MAX_QUEUED_SOURCE_EVENTS {
            // Preserve lifecycle/readiness events by sacrificing the oldest
            // high-rate motion sample. Press/release and touch lifecycle events
            // must never be discarded because that can leave compositor state stuck.
            if let Some(index) = self.events.iter().position(is_lossy_motion) {
                self.events.remove(index);
            } else if is_lossy_motion(&event) {
                tracing::warn!("runtime source queue is full; dropping a lossy source event");
                return;
            } else {
                // The bound is deliberately soft for discrete input. Preserving a
                // release/up event is more important than strict memory accounting;
                // the 256-event reducer budget drains this exceptional backlog.
                tracing::warn!("runtime source queue overflowed with lifecycle events");
            }
        }
        self.events.push_back(event);
    }

    fn process_events(&mut self) -> Result<()> {
        let now = Instant::now();
        let render_generation = self.state.render_generation();
        let mut scene_changed = self
            .state
            .next_visual_timer_deadline()
            .is_some_and(|deadline| deadline <= now);
        if self
            .frame_retry_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            self.frame_retry_deadline = None;
            scene_changed = true;
        }
        for _ in 0..MAX_EVENTS_PER_BATCH {
            let Some(event) = self.events.pop_front() else {
                break;
            };
            scene_changed |= self.reduce_event(event)?;
        }

        self.ipc.expire(now);
        self.state.poll_config();
        self.state.process_key_repeats();
        self.state.process_idle_timers();
        self.state.remove_dead_windows();
        // Import replies are protocol progress, not visual damage. Process them even when the
        // host suppresses redraws for an occluded or hidden nested window.
        if self.state.has_pending_dmabuf_imports() {
            self.state.validate_dmabuf_imports(self.backend.renderer());
        }
        let size = self.backend.window_size();
        self.state
            .update_output_size(i64::from(size.w), i64::from(size.h));
        if size != self.tracked_size {
            tracing::trace!(?size, tracked = ?self.tracked_size, "nested size observation changed");
            self.tracked_size = size;
            self.reset_damage_tracker();
            scene_changed = true;
        }
        self.ipc.finish_tick(&mut self.state);
        let generation_changed = self.state.render_generation() != render_generation;
        scene_changed |= generation_changed;
        self.scene_dirty |= scene_changed;
        // Protocol replies, especially the initial xdg configure, are independent of visual
        // damage and must leave the compositor in this transaction.
        if let Err(error) = self.display.flush_clients() {
            tracing::warn!(%error, "could not flush Wayland clients");
        }

        if self.host_configured
            && self.scene_dirty
            && !self.present_ready
            && self.shutdown_deadline.is_none()
        {
            // request_redraw is coalesced by winit itself. We intentionally do
            // not retain a local latch: a host that suppressed a redraw while
            // hidden must be able to accept a later request after restoration.
            self.backend.window().request_redraw();
        }
        if self.present_ready {
            self.present_ready = false;
            tracing::trace!("starting nested frame");
            if let Err(error) = self.render_and_present() {
                // A transient EGL/window error must not take down IPC and all
                // clients. The next state change retries from a full frame.
                tracing::warn!(%error, "nested frame submission failed");
                self.reset_damage_tracker();
                self.has_presented = false;
                self.frame_retry_deadline = Some(Instant::now() + self.frame_retry_delay);
                self.frame_retry_delay = (self.frame_retry_delay * 2).min(FRAME_RETRY_MAX);
            } else {
                tracing::trace!("nested frame completed");
                self.scene_dirty = false;
                self.frame_retry_deadline = None;
                self.frame_retry_delay = FRAME_RETRY_INITIAL;
            }
        }
        Ok(())
    }

    fn reduce_event(&mut self, event: RuntimeEvent) -> Result<bool> {
        match event {
            RuntimeEvent::Winit(WinitEvent::Input(event)) => {
                self.state.process_input(event);
            }
            RuntimeEvent::Winit(WinitEvent::Resized { size, .. }) => {
                // The host EGL surface is invalid before its first non-zero
                // configure, and buffer age resets whenever its size changes.
                let was_configured = self.host_configured;
                let size_changed = size != self.tracked_size;
                self.host_configured = size.w > 0 && size.h > 0;
                if size_changed || self.host_configured != was_configured {
                    self.has_presented = false;
                    self.present_ready = false;
                    return Ok(self.host_configured);
                }
            }
            RuntimeEvent::Winit(WinitEvent::Redraw) => {
                self.present_ready |= self.host_configured && self.scene_dirty;
            }
            RuntimeEvent::Winit(WinitEvent::CloseRequested) => self.exiting = true,
            RuntimeEvent::Winit(WinitEvent::Focus(true)) => {
                // Smithay 0.7 does not expose Occluded; focus restoration is a
                // conservative full-repaint signal.
                self.reset_damage_tracker();
                return Ok(true);
            }
            RuntimeEvent::Winit(WinitEvent::Focus(false)) => {}
            RuntimeEvent::WaylandClient(stream) => {
                if let Err(error) = self
                    .display
                    .handle()
                    .insert_client(stream, Arc::new(ClientState::default()))
                {
                    tracing::warn!(%error, "could not insert Wayland client");
                } else {
                    self.dispatch_wayland_clients()?;
                }
            }
            RuntimeEvent::WaylandReady => self.dispatch_wayland_clients()?,
            RuntimeEvent::IpcReady => self.ipc.dispatch(&mut self.state),
            RuntimeEvent::ConfigChanged => self.state.notify_config_changed(),
            RuntimeEvent::InputServiceExited(exit) => {
                if let Some(service) = &mut self.input_service {
                    service.exited(exit);
                }
            }
        }
        Ok(false)
    }

    fn reset_damage_tracker(&mut self) {
        self.damage_tracker =
            OutputDamageTracker::new(self.tracked_size, 1.0, Transform::Flipped180);
    }

    fn dispatch_wayland_clients(&mut self) -> Result<()> {
        let started = Instant::now();
        self.display.dispatch_clients(&mut self.state)?;
        let elapsed = started.elapsed();
        if elapsed >= SOURCE_WARN_AFTER {
            // wayland-server 0.31 exposes an all-clients dispatch operation but
            // no preemptible budget. Keep the readiness callback lightweight and
            // make pathological client batches visible to operators.
            tracing::warn!(?elapsed, "Wayland client dispatch exceeded source budget");
        }
        Ok(())
    }

    fn render_and_present(&mut self) -> Result<()> {
        // EGL buffer age is only defined after this surface has completed a
        // swap. Querying it on the first frame produces EGL_BAD_SURFACE on
        // some Wayland/Mesa combinations.
        let buffer_age = if self.has_presented {
            self.backend.buffer_age().unwrap_or(0)
        } else {
            0
        };
        let roots = self.state.render_roots();
        let mut frame_callbacks = Vec::new();
        let (damage, render_states) = {
            tracing::trace!("binding nested framebuffer");
            let (renderer, mut framebuffer) = self.backend.bind()?;
            tracing::trace!("nested framebuffer bound");
            self.state.validate_dmabuf_imports(renderer);
            let mut elements: Vec<WinitRenderElement> = roots
                .iter()
                .flat_map(|(surface, location, scale)| {
                    let mut elements = Vec::new();
                    for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
                        let geometry = popup.geometry();
                        let offset: Point<i32, Physical> = (
                            ((popup_offset.x - geometry.loc.x) as f64 * scale).round() as i32,
                            ((popup_offset.y - geometry.loc.y) as f64 * scale).round() as i32,
                        )
                            .into();
                        let popup_elements = surface_tree_snapshot(
                            renderer,
                            popup.wl_surface(),
                            *location + offset,
                            *scale,
                            1.0,
                            Kind::Unspecified,
                            &mut frame_callbacks,
                        );
                        elements.extend(popup_elements.into_iter().map(Into::into));
                    }
                    let root_elements = surface_tree_snapshot(
                        renderer,
                        surface,
                        *location,
                        *scale,
                        1.0,
                        Kind::Unspecified,
                        &mut frame_callbacks,
                    );
                    elements.extend(root_elements.into_iter().map(Into::into));
                    elements
                })
                .collect();
            if let Some((icon, location, scale)) =
                self.state.dnd_icon_render_source(astera_core::OutputId(0))
            {
                let icon_elements = surface_tree_snapshot(
                    renderer,
                    &icon,
                    location,
                    scale,
                    1.0,
                    Kind::Cursor,
                    &mut frame_callbacks,
                );
                elements.splice(0..0, icon_elements.into_iter().map(Into::into));
            }
            if let Some(cursor) = self.state.cursor_render_source(astera_core::OutputId(0)) {
                match cursor {
                    CursorRenderSource::Surface {
                        surface,
                        location,
                        scale,
                    } => {
                        let cursor_elements = surface_tree_snapshot(
                            renderer,
                            &surface,
                            location,
                            scale,
                            1.0,
                            Kind::Cursor,
                            &mut frame_callbacks,
                        );
                        elements.splice(0..0, cursor_elements.into_iter().map(Into::into));
                    }
                    CursorRenderSource::Memory {
                        buffer,
                        location,
                        size,
                        source_size,
                    } => {
                        let element = MemoryRenderBufferRenderElement::from_buffer(
                            renderer,
                            location,
                            &buffer,
                            None,
                            Some(smithay::utils::Rectangle::from_size(source_size.to_f64())),
                            Some(size),
                            Kind::Cursor,
                        )?;
                        elements.insert(0, element.into());
                    }
                }
            }
            tracing::trace!(
                elements = elements.len(),
                "nested render elements collected"
            );
            let clear = if self.state.session_is_locked() {
                Color32F::new(0.0, 0.0, 0.0, 1.0)
            } else {
                Color32F::new(0.025, 0.035, 0.06, 1.0)
            };
            let result = self.damage_tracker.render_output(
                renderer,
                &mut framebuffer,
                buffer_age,
                &elements,
                clear,
            )?;
            (result.damage.cloned(), result.states)
        };

        // A frame callback without visual damage still needs a real host presentation
        // opportunity. Swap even when the damage tracker returns None.
        tracing::trace!(has_damage = damage.is_some(), "submitting nested frame");
        self.backend.submit(damage.as_deref())?;
        self.state
            .update_primary_scanout_output(astera_core::OutputId(0), &roots, &render_states);
        tracing::trace!("nested frame submitted");
        self.has_presented = true;
        let frame_time = self.started.elapsed().as_millis() as u32;
        complete_frame_callbacks(&frame_callbacks, frame_time);
        self.display.flush_clients()?;
        Ok(())
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.ipc.next_timeout(),
            self.state.next_timer_deadline(),
            self.shutdown_deadline,
            self.frame_retry_deadline,
            (!self.state.session_is_locked())
                .then(|| {
                    self.input_service
                        .as_ref()
                        .and_then(InputServiceSupervisor::next_deadline)
                })
                .flatten(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn begin_shutdown(&mut self) {
        if self.shutdown_deadline.is_none() {
            self.shutdown_deadline = Some(Instant::now() + SHUTDOWN_GRACE);
            tracing::info!("draining IPC replies before nested compositor shutdown");
        }
    }

    fn shutdown_complete(&self) -> bool {
        self.shutdown_deadline
            .is_some_and(|deadline| !self.ipc.has_pending_output() || Instant::now() >= deadline)
    }
}

fn is_lossy_motion(event: &RuntimeEvent) -> bool {
    matches!(
        event,
        RuntimeEvent::Winit(WinitEvent::Input(
            smithay::backend::input::InputEvent::PointerMotion { .. }
                | smithay::backend::input::InputEvent::PointerMotionAbsolute { .. }
        ))
    )
}

pub fn run(config: Config, config_path: std::path::PathBuf) -> Result<()> {
    let mut event_loop: EventLoop<WinitLoop> = EventLoop::try_new()?;
    let display: Display<Astera> = Display::new()?;
    let input_service = config.input_service.clone();
    let mut state = Astera::new(&display.handle(), config);
    state.disable_session_lock_advertisement();
    state.watch_config(config_path);
    tracing::debug!(state = ?state.execute_command(Command::GetState), "initial desktop state");
    let listener = WaylandSocketSource::bind_auto("astera", 1..32)?;
    let socket_name = listener
        .socket_name()
        .context("Wayland listening socket has no name")?
        .to_string_lossy()
        .into_owned();
    let (mut backend, winit_events) =
        winit::init::<GlesRenderer>().map_err(|error| anyhow!(error.to_string()))?;
    // Astera composites the same cursor path as the native backend. Leaving winit's host cursor
    // visible would produce two cursors with unrelated shapes and hotspots inside the window.
    backend.window().set_cursor_visible(false);
    let (mut input_service, input_service_exits) = match input_service {
        Some(argv) => {
            let (service, exits) = InputServiceSupervisor::new(argv);
            (Some(service), Some(exits))
        }
        None => (None, None),
    };
    if let Some(service) = &mut input_service {
        service.start(display.handle())?;
    }
    // Capability discovery does not require binding the not-yet-configured host
    // EGL surface; doing so here can produce EGL_BAD_SURFACE on Wayland hosts.
    let renderer = backend.renderer();
    let formats = renderer.dmabuf_formats();
    let main_device = EGLDevice::device_for_display(renderer.egl_context().display())
        .ok()
        .and_then(|device| device.try_get_render_node().ok().flatten())
        .map(|node| node.dev_id());
    state.enable_dmabuf(main_device, formats.clone());
    if let Some(main_device) = main_device {
        state.register_output_dmabuf_feedback(astera_core::OutputId(0), main_device, formats);
    }
    let ipc = IpcServer::bind(&socket_name)?;
    let tracked_size = backend.window_size();
    let damage_tracker = OutputDamageTracker::new(tracked_size, 1.0, Transform::Flipped180);

    event_loop
        .handle()
        .insert_source(winit_events, |event, _, runtime| {
            runtime.enqueue(RuntimeEvent::Winit(event));
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    if let Some(exits) = input_service_exits {
        event_loop
            .handle()
            .insert_source(exits, |event, _, runtime| {
                if let smithay::reexports::calloop::channel::Event::Msg(exit) = event {
                    runtime.enqueue(RuntimeEvent::InputServiceExited(exit));
                }
            })
            .map_err(|error| anyhow!(error.to_string()))?;
    }
    event_loop
        .handle()
        .insert_source(listener, |stream, _, runtime| {
            runtime.enqueue(RuntimeEvent::WaylandClient(stream));
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(ReadableFdSource::duplicate(&display)?, |(), _, runtime| {
            runtime.enqueue(RuntimeEvent::WaylandReady);
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(ipc.event_source(), |(), _, runtime| {
            runtime.enqueue(RuntimeEvent::IpcReady);
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    if let Some(config_fd) = state.config_watch_fd()? {
        event_loop
            .handle()
            .insert_source(ReadableFdSource::new(config_fd), |(), _, runtime| {
                runtime.enqueue(RuntimeEvent::ConfigChanged);
            })
            .map_err(|error| anyhow!(error.to_string()))?;
    }

    let mut runtime = WinitLoop {
        display,
        state,
        ipc,
        backend,
        events: VecDeque::new(),
        damage_tracker,
        tracked_size,
        started: Instant::now(),
        present_ready: false,
        exiting: false,
        shutdown_deadline: None,
        frame_retry_deadline: None,
        frame_retry_delay: FRAME_RETRY_INITIAL,
        host_configured: false,
        has_presented: false,
        scene_dirty: false,
        input_service,
    };

    tracing::info!(wayland_display = %socket_name, "nested compositor is ready");
    println!("WAYLAND_DISPLAY={socket_name}");

    while !runtime.exiting && !runtime.shutdown_complete() {
        let timeout = if runtime.events.is_empty() {
            runtime
                .next_deadline()
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
        } else {
            Some(Duration::ZERO)
        };
        event_loop.dispatch(timeout, &mut runtime)?;
        runtime.process_events()?;
        let locked = runtime.state.session_is_locked();
        if let Some(service) = &mut runtime.input_service {
            service.poll(runtime.display.handle(), locked);
        }
        if runtime.state.should_quit() {
            runtime.begin_shutdown();
        }
    }
    tracing::info!("nested compositor is shutting down");
    Ok(())
}
