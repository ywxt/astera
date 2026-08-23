use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    ops::{Deref, DerefMut},
    os::fd::OwnedFd,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

mod activation;
mod clock;
mod config_watcher;
pub(crate) mod cursor;
mod event_hub;
mod geometry;
mod idle;
mod input_method;
mod key_repeat;
mod model;
mod output;
mod output_power;
mod pointer_constraints;
mod popup_touch;
mod process;
pub(crate) use process::{InputServiceExit, InputServiceSupervisor};
mod public_state;
mod scene;
mod session_lock;
#[cfg(test)]
mod snapshot;
mod tablet_input;
mod touch;

use activation::ActivationTracker;
use clock::{Clock, SystemClock};
use config_watcher::ConfigWatcher;
use event_hub::EventHub;
use geometry::{
    layer_rank, mode_layer, output_transform, physical_point, point_inside, saturating_i32,
};
use idle::{IdleEvent, IdleRuntime};
use key_repeat::KeyRepeatState;
use model::{
    ActivePointerGesture, DragKind, DragSource, DragState, MappedInputMethodPopup, MappedLayer,
    MappedWindow, OutputRuntime, ProtocolState, ResizeEdges,
};
use output_power::OutputPowerGlobalData;
use session_lock::{LockSurfaces, SessionState};

use anyhow::{Result as AnyResult, anyhow};
use astera_config::{
    Action, BindingKey, Config, Modifiers as BindingModifiers,
    WorkspaceSelector as BindingWorkspaceSelector,
};
use astera_core::{
    Desktop, Output, OutputId, OutputTransform, Point, RestorePlacement, Size, WindowId,
    WindowMode, WindowTransaction, WorkspaceId, WorkspaceTransaction,
};
use astera_ipc::{Command, ErrorCode, OutputSelector, Response, Success, WorkspaceSelector};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::{
    backend::{
        allocator::{Format, dmabuf::Dmabuf},
        input::{
            AbsolutePositionEvent, Axis, ButtonState as BackendButtonState, Device, Event,
            GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent, GestureSwipeUpdateEvent,
            InputBackend, InputEvent, KeyState, KeyboardKeyEvent, MouseButton, PointerAxisEvent,
            PointerButtonEvent, PointerMotionEvent, TouchEvent,
        },
        renderer::{
            ImportDma,
            utils::{on_commit_buffer_handler, with_renderer_surface_state},
        },
    },
    delegate_alpha_modifier, delegate_compositor, delegate_cursor_shape, delegate_dmabuf,
    delegate_foreign_toplevel_list, delegate_fractional_scale, delegate_idle_inhibit,
    delegate_keyboard_shortcuts_inhibit, delegate_layer_shell, delegate_pointer_constraints,
    delegate_pointer_gestures, delegate_relative_pointer, delegate_seat, delegate_shm,
    delegate_single_pixel_buffer, delegate_tablet_manager, delegate_viewporter,
    delegate_xdg_activation, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, WindowSurfaceType,
        find_popup_root_surface, layer_map_for_output, utils::under_from_surface_tree,
    },
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, ModifiersState},
        pointer::{
            AxisFrame, ButtonEvent, Focus as PointerFocusMode, GestureHoldBeginEvent,
            GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
            GesturePinchUpdateEvent as SmithayGesturePinchUpdateEvent, GestureSwipeBeginEvent,
            GestureSwipeEndEvent, GestureSwipeUpdateEvent as SmithayGestureSwipeUpdateEvent,
            MotionEvent, PointerTarget,
        },
        touch::{DownEvent, MotionEvent as SmithayTouchMotionEvent, UpEvent},
    },
    output::{Mode, Output as SmithayOutput, PhysicalProperties, Scale, Subpixel},
    reexports::wayland_server::{
        Client, DisplayHandle, Resource,
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::{wl_buffer, wl_output::WlOutput, wl_seat, wl_surface::WlSurface},
    },
    utils::{
        IsAlive, Logical, Physical, Point as SmithayPoint, Rectangle as SmithayRectangle, Serial,
    },
    wayland::{
        alpha_modifier::AlphaModifierState,
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, TraversalAction,
            with_states, with_surface_tree_downward,
        },
        cursor_shape::CursorShapeManagerState,
        dmabuf::{
            DmabufFeedback, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
            SurfaceDmabufFeedbackState,
        },
        foreign_toplevel_list::{ForeignToplevelListHandler, ForeignToplevelListState},
        fractional_scale::{
            FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale,
        },
        idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState},
        input_method::{
            InputMethodHandler, InputMethodManagerState, PopupSurface as InputMethodPopupSurface,
        },
        keyboard_shortcuts_inhibit::{
            KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState,
            KeyboardShortcutsInhibitor, KeyboardShortcutsInhibitorSeat,
        },
        output::{OutputHandler, OutputManagerState},
        pointer_constraints::PointerConstraintsState,
        pointer_gestures::PointerGesturesState,
        relative_pointer::RelativePointerManagerState,
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
                set_data_device_focus,
            },
            primary_selection::{
                PrimarySelectionHandler, PrimarySelectionState, set_primary_focus,
            },
        },
        session_lock::SessionLockManagerState,
        shell::wlr_layer::{
            KeyboardInteractivity, Layer, LayerSurface, LayerSurfaceCachedState,
            WlrLayerShellHandler, WlrLayerShellState,
        },
        shell::xdg::{
            PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler,
            XdgShellState, XdgToplevelSurfaceData,
            decoration::{XdgDecorationHandler, XdgDecorationState},
        },
        shm::{ShmHandler, ShmState},
        single_pixel_buffer::SinglePixelBufferState,
        tablet_manager::{TabletManagerState, TabletSeatHandler},
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        virtual_keyboard::VirtualKeyboardManagerState,
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
    },
};

const DEFAULT_WINDOW_SIZE: Size = Size::new(800, 600);

pub struct Astera {
    protocol: ProtocolState,
    output_runtime: BTreeMap<OutputId, OutputRuntime>,
    xdg_outputs: BTreeMap<OutputId, Vec<smithay::reexports::wayland_protocols::xdg::xdg_output::zv1::server::zxdg_output_v1::ZxdgOutputV1>>,
    desktop: Desktop,
    active_output: OutputId,
    windows: Vec<MappedWindow>,
    layers: Vec<MappedLayer>,
    on_demand_layer_focus: Option<u64>,
    input_method_popups: Vec<MappedInputMethodPopup>,
    input_method_claimed: bool,
    input_method_client: Option<Client>,
    input_method_manager_resource: Option<smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::ZwpInputMethodManagerV2>,
    input_method_resource: Option<smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2>,
    virtual_keyboard_clients: Vec<(
        Client,
        smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        usize,
    )>,
    next_window_id: u64,
    next_layer_id: u64,
    pointer_location: SmithayPoint<f64, smithay::utils::Logical>,
    active_pointer_gesture: Option<ActivePointerGesture>,
    cursor_image_status: smithay::input::pointer::CursorImageStatus,
    dnd_icon: Option<WlSurface>,
    dnd_touch_icon: Option<(
        OutputId,
        smithay::backend::input::TouchSlot,
        SmithayPoint<f64, smithay::utils::Logical>,
    )>,
    named_cursors: HashMap<(smithay::input::pointer::CursorIcon, u32), cursor::NamedCursor>,
    active_tablet_cursor: Option<smithay::backend::input::TabletToolDescriptor>,
    pointer_focus_origin: Option<(
        WlSurface,
        SmithayPoint<f64, smithay::utils::Logical>,
        f64,
    )>,
    drag: Option<DragState>,
    key_repeat: KeyRepeatState,
    active_shortcut_inhibitor: Option<KeyboardShortcutsInhibitor>,
    touch_device_outputs: HashMap<String, OutputId>,
    output_aliases: HashMap<String, OutputId>,
    ambiguous_output_aliases: HashSet<String>,
    touch_slots: HashMap<(String, i32), (OutputId, smithay::backend::input::TouchSlot)>,
    next_touch_slot: u32,
    tablets: HashMap<String, (smithay::wayland::tablet_manager::TabletDescriptor, smithay::wayland::tablet_manager::TabletHandle)>,
    tablet_tools: HashMap<
        smithay::backend::input::TabletToolDescriptor,
        tablet_input::TabletToolRuntime,
    >,
    clock: Arc<dyn Clock>,
    config: Config,
    config_source: Option<String>,
    config_generation: u64,
    config_failed: bool,
    config_error: Option<String>,
    config_watcher: Option<ConfigWatcher>,
    event_hub: EventHub,
    public_dirty: bool,
    // Backends compare this generation across one event batch. Protocol
    // readiness alone must not schedule a frame: only a mutation which can
    // change the composed scene advances it.
    render_generation: u64,
    #[cfg(test)]
    public_snapshot_builds: u64,
    should_quit: bool,
    output_configuration_supported: bool,
    pending_dmabufs: Vec<(Dmabuf, ImportNotifier)>,
    dmabuf_enabled: bool,
    dmabuf_global: Option<DmabufGlobal>,
    dmabuf_default_device: Option<u64>,
    dmabuf_default_formats: Vec<Format>,
    dmabuf_devices: BTreeMap<u64, Vec<Format>>,
    dmabuf_output_devices: BTreeMap<OutputId, u64>,
    dmabuf_output_feedback: BTreeMap<OutputId, DmabufFeedback>,
    dmabuf_feedback_surfaces: HashSet<WlSurface>,
    serial: u32,
    session_lock_manager: SessionLockManagerState,
    session_lock_advertised: Arc<AtomicBool>,
    session_state: SessionState,
    lock_surfaces: LockSurfaces,
    next_lock_generation: u64,
    output_power_advertised: Arc<AtomicBool>,
    output_power_controls: BTreeMap<OutputId, Vec<smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::zwlr_output_power_v1::ZwlrOutputPowerV1>>,
    output_power_modes: BTreeMap<OutputId, bool>,
    pending_output_power: VecDeque<(OutputId, bool)>,
    _relative_pointer_state: RelativePointerManagerState,
    _pointer_constraints_state: PointerConstraintsState,
    idle_runtime: IdleRuntime,
    idle_notifications: BTreeMap<u64, smithay::reexports::wayland_protocols::ext::idle_notify::v1::server::ext_idle_notification_v1::ExtIdleNotificationV1>,
    next_idle_notification: u64,
    activation_tracker: ActivationTracker,
    last_selection_serial: Option<Serial>,
    last_primary_selection_serial: Option<Serial>,
}

impl Deref for Astera {
    type Target = ProtocolState;

    fn deref(&self) -> &Self::Target {
        &self.protocol
    }
}

impl DerefMut for Astera {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.protocol
    }
}

impl Astera {
    pub fn new(display: &DisplayHandle, config: Config) -> Self {
        Self::new_with_clock(display, config, Arc::new(SystemClock))
    }

    fn new_with_clock(display: &DisplayHandle, config: Config, clock: Arc<dyn Clock>) -> Self {
        let compositor_state = CompositorState::new::<Self>(display);
        display.create_global::<Self, smithay::reexports::wayland_protocols::ext::idle_notify::v1::server::ext_idle_notifier_v1::ExtIdleNotifierV1, _>(2, ());
        let xdg_shell_state = XdgShellState::new_with_capabilities::<Self>(
            display,
            [
                xdg_toplevel::WmCapabilities::Fullscreen,
                xdg_toplevel::WmCapabilities::Maximize,
                xdg_toplevel::WmCapabilities::Minimize,
            ],
        );
        let xdg_decoration_state = XdgDecorationState::new::<Self>(display);
        let xdg_activation_state = XdgActivationState::new::<Self>(display);
        let layer_shell_state = WlrLayerShellState::new::<Self>(display);
        let fractional_scale_state = FractionalScaleManagerState::new::<Self>(display);
        let viewporter_state = ViewporterState::new::<Self>(display);
        let idle_inhibit_state = IdleInhibitManagerState::new::<Self>(display);
        let keyboard_shortcuts_inhibit_state = KeyboardShortcutsInhibitState::new::<Self>(display);
        let pointer_gestures_state = PointerGesturesState::new::<Self>(display);
        let tablet_manager_state = TabletManagerState::new::<Self>(display);
        let cursor_shape_state = CursorShapeManagerState::new::<Self>(display);
        let text_input_state = TextInputManagerState::new::<Self>(display);
        // Privileged input globals are visible only on the private WAYLAND_SOCKET connection
        // created by the compositor's supervised input-service process.
        let input_method_state = InputMethodManagerState::new::<Self, _>(display, |client| {
            client
                .get_data::<ClientState>()
                .is_some_and(|state| state.trusted_input)
        });
        let virtual_keyboard_state =
            VirtualKeyboardManagerState::new::<Self, _>(display, |client| {
                client
                    .get_data::<ClientState>()
                    .is_some_and(|state| state.trusted_input)
            });
        let session_lock_advertised = Arc::new(AtomicBool::new(true));
        let advertised = session_lock_advertised.clone();
        let session_lock_manager = SessionLockManagerState::new::<Self, _>(display, move |_| {
            advertised.load(Ordering::Relaxed)
        });
        let output_power_advertised = Arc::new(AtomicBool::new(false));
        display.create_global::<
            Self,
            smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1,
            _,
        >(
            1,
            OutputPowerGlobalData {
                visible: output_power_advertised.clone(),
            },
        );
        let relative_pointer_state = RelativePointerManagerState::new::<Self>(display);
        let pointer_constraints_state = PointerConstraintsState::new::<Self>(display);
        let output_manager_state = OutputManagerState::new();
        display.create_global::<Self, smithay::reexports::wayland_protocols::xdg::xdg_output::zv1::server::zxdg_output_manager_v1::ZxdgOutputManagerV1, _>(3, ());
        let wayland_output = SmithayOutput::new(
            "ASTERA-NESTED-1".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Astera".into(),
                model: "Nested Output".into(),
            },
        );
        let output_global = wayland_output.create_global::<Self>(display);
        let initial_mode = Mode {
            size: (1280, 720).into(),
            refresh: 60_000,
        };
        wayland_output.change_current_state(
            Some(initial_mode),
            Some(smithay::utils::Transform::Normal),
            Some(Scale::Fractional(1.0)),
            Some((0, 0).into()),
        );
        wayland_output.set_preferred(initial_mode);
        let shm_state = ShmState::new::<Self>(display, Vec::new());
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(display, "astera-seat");
        let keyboard = seat
            .add_keyboard(
                Default::default(),
                config.key_repeat.delay_ms as i32,
                config.key_repeat.rate as i32,
            )
            .expect("default keyboard map must compile");
        let pointer = seat.add_pointer();
        let touch = touch::add_touch(&mut seat);
        let data_device_state = DataDeviceState::new::<Self>(display);
        let primary_selection_state = PrimarySelectionState::new::<Self>(display);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(display);
        let alpha_modifier_state = AlphaModifierState::new::<Self>(display);
        let foreign_toplevel_list_state = ForeignToplevelListState::new::<Self>(display);

        let active_output = OutputId(0);
        let mut desktop = Desktop::new(config.gap);
        desktop
            .connect_output(Output::new(
                active_output,
                "nested-output",
                Size::new(1280, 720),
            ))
            .expect("initial output is valid");
        desktop
            .workspace_mut(desktop.active_workspace_id(active_output).unwrap())
            .unwrap()
            .camera
            .policy = config.camera;

        Self {
            protocol: ProtocolState {
                display: display.clone(),
                compositor_state,
                xdg_shell_state,
                _xdg_decoration_state: xdg_decoration_state,
                xdg_activation_state,
                layer_shell_state,
                _fractional_scale_state: fractional_scale_state,
                _viewporter_state: viewporter_state,
                _idle_inhibit_state: idle_inhibit_state,
                keyboard_shortcuts_inhibit_state,
                _pointer_gestures_state: pointer_gestures_state,
                _tablet_manager_state: tablet_manager_state,
                _cursor_shape_state: cursor_shape_state,
                _text_input_state: text_input_state,
                _input_method_state: input_method_state,
                _virtual_keyboard_state: virtual_keyboard_state,
                _output_manager_state: output_manager_state,
                shm_state,
                seat_state,
                data_device_state,
                primary_selection_state,
                _single_pixel_buffer_state: single_pixel_buffer_state,
                _alpha_modifier_state: alpha_modifier_state,
                foreign_toplevel_list_state,
                dmabuf_state: DmabufState::new(),
                popup_manager: PopupManager::default(),
                seat,
                keyboard,
                pointer,
                touch,
                idle_inhibitors: std::collections::HashMap::new(),
            },
            output_runtime: BTreeMap::from([(
                active_output,
                OutputRuntime {
                    wayland: wayland_output,
                    global: output_global,
                    entered_surfaces: HashSet::new(),
                    presented_surfaces: HashSet::new(),
                    location: Point::ORIGIN,
                },
            )]),
            xdg_outputs: BTreeMap::new(),
            desktop,
            active_output,
            windows: Vec::new(),
            layers: Vec::new(),
            on_demand_layer_focus: None,
            input_method_popups: Vec::new(),
            input_method_claimed: false,
            input_method_client: None,
            input_method_manager_resource: None,
            input_method_resource: None,
            virtual_keyboard_clients: Vec::new(),
            next_window_id: 1,
            next_layer_id: 1,
            pointer_location: (0.0, 0.0).into(),
            active_pointer_gesture: None,
            cursor_image_status: smithay::input::pointer::CursorImageStatus::Named(
                smithay::input::pointer::CursorIcon::Default,
            ),
            dnd_icon: None,
            dnd_touch_icon: None,
            named_cursors: HashMap::from([(
                (smithay::input::pointer::CursorIcon::Default, 120),
                cursor::load_named_cursor(smithay::input::pointer::CursorIcon::Default, 120),
            )]),
            active_tablet_cursor: None,
            pointer_focus_origin: None,
            drag: None,
            key_repeat: KeyRepeatState::default(),
            active_shortcut_inhibitor: None,
            touch_device_outputs: HashMap::new(),
            output_aliases: HashMap::new(),
            ambiguous_output_aliases: HashSet::new(),
            touch_slots: HashMap::new(),
            next_touch_slot: 0,
            tablets: HashMap::new(),
            tablet_tools: HashMap::new(),
            clock,
            config,
            config_source: None,
            config_generation: 0,
            config_failed: false,
            config_error: None,
            config_watcher: None,
            event_hub: EventHub::default(),
            // The first publish establishes the event hub's authoritative baseline.
            public_dirty: true,
            render_generation: 0,
            #[cfg(test)]
            public_snapshot_builds: 0,
            should_quit: false,
            output_configuration_supported: true,
            pending_dmabufs: Vec::new(),
            dmabuf_enabled: false,
            dmabuf_global: None,
            dmabuf_default_device: None,
            dmabuf_default_formats: Vec::new(),
            dmabuf_devices: BTreeMap::new(),
            dmabuf_output_devices: BTreeMap::new(),
            dmabuf_output_feedback: BTreeMap::new(),
            dmabuf_feedback_surfaces: HashSet::new(),
            serial: 1,
            session_lock_manager,
            session_lock_advertised,
            session_state: SessionState::default(),
            lock_surfaces: LockSurfaces::new(),
            next_lock_generation: 0,
            output_power_advertised,
            output_power_controls: BTreeMap::new(),
            output_power_modes: BTreeMap::new(),
            pending_output_power: VecDeque::new(),
            _relative_pointer_state: relative_pointer_state,
            _pointer_constraints_state: pointer_constraints_state,
            idle_runtime: IdleRuntime::default(),
            idle_notifications: BTreeMap::new(),
            next_idle_notification: 1,
            activation_tracker: ActivationTracker::default(),
            last_selection_serial: None,
            last_primary_selection_serial: None,
        }
    }

    pub fn render_generation(&self) -> u64 {
        self.render_generation
    }

    pub(super) fn mark_render_dirty(&mut self) {
        self.render_generation = self.render_generation.wrapping_add(1);
    }

    #[allow(dead_code)] // Used by the native backend's hotplug path.
    pub fn connect_output(&mut self, output: Output) -> Result<(), astera_core::DesktopError> {
        self.desktop.connect_output(output.clone())?;
        let wayland = SmithayOutput::new(
            output.stable_key.clone(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Astera".into(),
                model: output.stable_key.clone(),
            },
        );
        let global = wayland.create_global::<Self>(&self.display);
        let mode = Mode {
            size: (
                saturating_i32(output.physical_size.width),
                saturating_i32(output.physical_size.height),
            )
                .into(),
            refresh: 60_000,
        };
        wayland.change_current_state(
            Some(mode),
            Some(output_transform(output.transform)),
            Some(Scale::Fractional(output.native_scale.0 as f64 / 120.0)),
            Some((0, 0).into()),
        );
        wayland.set_preferred(mode);
        self.output_runtime.insert(
            output.id,
            OutputRuntime {
                wayland,
                global,
                entered_surfaces: HashSet::new(),
                presented_surfaces: HashSet::new(),
                location: Point::ORIGIN,
            },
        );
        self.session_output_connected(output.id);
        self.output_power_connected(output.id);
        if self.desktop.outputs.len() == 1 {
            self.active_output = output.id;
            for layer in &mut self.layers {
                if !self.desktop.outputs.contains_key(&layer.output) {
                    layer.output = output.id;
                }
            }
        }
        self.reflow_outputs();
        self.map_buffered_toplevels();
        self.refresh_visible_scales();
        let workspace = self.desktop.active_workspace_id(output.id);
        tracing::info!(
            output = ?output.id,
            stable_key = %output.stable_key,
            ?workspace,
            outputs = self.desktop.outputs.len(),
            "output connected"
        );
        self.mark_public_dirty();
        Ok(())
    }

    #[allow(dead_code)] // Used by the native backend's hotplug path.
    pub fn disconnect_output(&mut self, output: OutputId) -> Result<(), astera_core::DesktopError> {
        self.cancel_surface_bound_input();
        let event = self.desktop.disconnect_output(output)?;
        if let Some(runtime) = self.output_runtime.get(&output) {
            for layer in self.layers.iter().filter(|layer| layer.output == output) {
                layer_map_for_output(&runtime.wayland).unmap_layer(&layer.surface);
                layer.surface.layer_surface().send_close();
            }
        }
        let runtime = self
            .output_runtime
            .remove(&output)
            .expect("desktop output has a Wayland runtime");
        self.xdg_outputs.remove(&output);
        self.dmabuf_output_devices.remove(&output);
        self.dmabuf_output_feedback.remove(&output);
        let empty = smithay::backend::renderer::element::RenderElementStates::default();
        for surface in &runtime.presented_surfaces {
            with_states(surface, |states| {
                smithay::desktop::utils::update_surface_primary_scanout_output(
                    surface,
                    &runtime.wayland,
                    states,
                    &empty,
                    smithay::backend::renderer::element::default_primary_scanout_output_compare,
                );
            });
        }
        for surface in runtime.entered_surfaces {
            runtime.wayland.leave(&surface);
        }
        self.display.disable_global::<Self>(runtime.global);
        self.layers.retain(|mapped| mapped.output != output);
        self.session_output_disconnected(output);
        self.output_power_disconnected(output);
        self.output_aliases.retain(|_, mapped| *mapped != output);
        self.touch_device_outputs
            .retain(|_, mapped| *mapped != output);
        if self.active_output == output
            && let Some(next) = self.desktop.outputs.keys().next().copied()
        {
            self.active_output = next;
        }
        self.reflow_outputs();
        self.configure_fullscreen_windows();
        tracing::info!(?output, ?event, "output disconnected");
        self.refresh_requested_dmabuf_feedbacks();
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
        self.handle_pointer_motion(self.pointer_location, 0);
        self.mark_public_dirty();
        Ok(())
    }

    pub fn process_input<B: InputBackend>(&mut self, event: InputEvent<B>) {
        let is_activity = matches!(
            &event,
            InputEvent::Keyboard { .. }
                | InputEvent::PointerMotionAbsolute { .. }
                | InputEvent::PointerMotion { .. }
                | InputEvent::PointerButton { .. }
                | InputEvent::PointerAxis { .. }
                | InputEvent::GestureSwipeBegin { .. }
                | InputEvent::GestureSwipeUpdate { .. }
                | InputEvent::GestureSwipeEnd { .. }
                | InputEvent::GesturePinchBegin { .. }
                | InputEvent::GesturePinchUpdate { .. }
                | InputEvent::GesturePinchEnd { .. }
                | InputEvent::GestureHoldBegin { .. }
                | InputEvent::GestureHoldEnd { .. }
                | InputEvent::TouchDown { .. }
                | InputEvent::TouchMotion { .. }
                | InputEvent::TouchUp { .. }
                | InputEvent::TouchCancel { .. }
                | InputEvent::TouchFrame { .. }
                | InputEvent::TabletToolAxis { .. }
                | InputEvent::TabletToolProximity { .. }
                | InputEvent::TabletToolTip { .. }
                | InputEvent::TabletToolButton { .. }
        );
        if is_activity {
            let events = self
                .idle_runtime
                .activity(self.idle_seat_key(), self.clock.now());
            self.send_idle_events(events);
        }
        // Device lifecycle must remain correct while headless; otherwise unplugging the last
        // output can leave stale tablet protocol objects and routing state behind.
        match &event {
            InputEvent::DeviceAdded { device } => {
                self.tablet_device_added(device);
                return;
            }
            InputEvent::DeviceRemoved { device } => {
                self.tablet_device_removed(device);
                return;
            }
            _ => {}
        }
        if !self.desktop.outputs.contains_key(&self.active_output) {
            return;
        }
        match event {
            InputEvent::Keyboard { event } => {
                let pressed = event.state() == KeyState::Pressed;
                let key_code = event.key_code();
                let keyboard = self.keyboard.clone();
                let serial = self.next_serial();
                let recipient = keyboard
                    .current_focus()
                    .and_then(|surface| surface.client())
                    .map(|client| client.id());
                let intercepted = keyboard.input::<(), _>(
                    self,
                    key_code,
                    event.state(),
                    serial,
                    event.time_msec(),
                    move |state, modifiers, key| {
                        // The locker receives all keyboard input; compositor bindings must not
                        // launch programs or mutate the hidden desktop while locked.
                        if state.session_is_locked() {
                            if !pressed
                                && state.key_repeat.release(
                                    key_code,
                                    state.config.key_repeat.rate,
                                    state.clock.now(),
                                )
                            {
                                return FilterResult::Intercept(());
                            }
                            return FilterResult::Forward;
                        }
                        if state.seat.keyboard_shortcuts_inhibited()
                            || state.exclusive_layer_has_keyboard_focus()
                        {
                            state.key_repeat.cancel_repeats();
                            // A shortcut press consumed before inhibition must retain its consumed
                            // release; forwarding only that release would give the client an
                            // impossible key sequence.
                            if !pressed
                                && state.key_repeat.release(
                                    key_code,
                                    state.config.key_repeat.rate,
                                    state.clock.now(),
                                )
                            {
                                return FilterResult::Intercept(());
                            }
                            return FilterResult::Forward;
                        }
                        if !pressed {
                            return if state.key_repeat.release(
                                key_code,
                                state.config.key_repeat.rate,
                                state.clock.now(),
                            ) {
                                FilterResult::Intercept(())
                            } else {
                                FilterResult::Forward
                            };
                        }
                        let symbol = key
                            .raw_latin_sym_or_raw_current_sym()
                            .map(|symbol| symbol.raw());
                        if state.handle_binding(modifiers, symbol, key_code) {
                            state.key_repeat.intercept(key_code);
                            FilterResult::Intercept(())
                        } else {
                            FilterResult::Forward
                        }
                    },
                );
                if pressed
                    && intercepted.is_none()
                    && let Some(recipient) = recipient
                {
                    self.activation_tracker
                        .remember(serial, recipient, self.clock.now());
                }
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let size = self.desktop.outputs[&self.active_output]
                    .output
                    .logical_size;
                let location = event.position_transformed(
                    (saturating_i32(size.width), saturating_i32(size.height)).into(),
                );
                self.handle_absolute_pointer_motion(location, event.time_msec());
            }
            InputEvent::PointerMotion { event } => self.handle_relative_pointer_motion(event),
            InputEvent::GestureSwipeBegin { event } => {
                self.handle_absolute_pointer_motion(self.pointer_location, event.time_msec());
                self.start_swipe_gesture(event.time_msec(), event.fingers());
            }
            InputEvent::GestureSwipeUpdate { event } => {
                let Some(ActivePointerGesture::Swipe(surface)) =
                    self.active_pointer_gesture.as_ref().cloned()
                else {
                    return;
                };
                let seat = self.seat.clone();
                surface.gesture_swipe_update(
                    &seat,
                    self,
                    &SmithayGestureSwipeUpdateEvent {
                        time: event.time_msec(),
                        delta: event.delta(),
                    },
                );
            }
            InputEvent::GestureSwipeEnd { event } => {
                let Some(ActivePointerGesture::Swipe(surface)) = self.active_pointer_gesture.take()
                else {
                    return;
                };
                let serial = self.next_serial();
                let seat = self.seat.clone();
                surface.gesture_swipe_end(
                    &seat,
                    self,
                    &GestureSwipeEndEvent {
                        serial,
                        time: event.time_msec(),
                        cancelled: event.cancelled(),
                    },
                );
            }
            InputEvent::GesturePinchBegin { event } => {
                self.cancel_pointer_gesture(event.time_msec());
                self.handle_absolute_pointer_motion(self.pointer_location, event.time_msec());
                let pointer = self.pointer.clone();
                let Some(surface) = pointer.current_focus() else {
                    return;
                };
                let serial = self.next_serial();
                let seat = self.seat.clone();
                surface.gesture_pinch_begin(
                    &seat,
                    self,
                    &GesturePinchBeginEvent {
                        serial,
                        time: event.time_msec(),
                        fingers: event.fingers(),
                    },
                );
                self.active_pointer_gesture = Some(ActivePointerGesture::Pinch(surface));
            }
            InputEvent::GesturePinchUpdate { event } => {
                let Some(ActivePointerGesture::Pinch(surface)) =
                    self.active_pointer_gesture.as_ref().cloned()
                else {
                    return;
                };
                let seat = self.seat.clone();
                surface.gesture_pinch_update(
                    &seat,
                    self,
                    &SmithayGesturePinchUpdateEvent {
                        time: event.time_msec(),
                        delta: event.delta(),
                        scale: event.scale(),
                        rotation: event.rotation(),
                    },
                );
            }
            InputEvent::GesturePinchEnd { event } => {
                let Some(ActivePointerGesture::Pinch(surface)) = self.active_pointer_gesture.take()
                else {
                    return;
                };
                let serial = self.next_serial();
                let seat = self.seat.clone();
                surface.gesture_pinch_end(
                    &seat,
                    self,
                    &GesturePinchEndEvent {
                        serial,
                        time: event.time_msec(),
                        cancelled: event.cancelled(),
                    },
                );
            }
            InputEvent::GestureHoldBegin { event } => {
                self.cancel_pointer_gesture(event.time_msec());
                self.handle_absolute_pointer_motion(self.pointer_location, event.time_msec());
                let pointer = self.pointer.clone();
                let Some(surface) = pointer.current_focus() else {
                    return;
                };
                let serial = self.next_serial();
                let seat = self.seat.clone();
                surface.gesture_hold_begin(
                    &seat,
                    self,
                    &GestureHoldBeginEvent {
                        serial,
                        time: event.time_msec(),
                        fingers: event.fingers(),
                    },
                );
                self.active_pointer_gesture = Some(ActivePointerGesture::Hold(surface));
            }
            InputEvent::GestureHoldEnd { event } => {
                let Some(ActivePointerGesture::Hold(surface)) = self.active_pointer_gesture.take()
                else {
                    return;
                };
                let serial = self.next_serial();
                let seat = self.seat.clone();
                surface.gesture_hold_end(
                    &seat,
                    self,
                    &GestureHoldEndEvent {
                        serial,
                        time: event.time_msec(),
                        cancelled: event.cancelled(),
                    },
                );
            }
            InputEvent::TouchDown { event } => {
                let device = event.device().id();
                let slot = i32::from(event.slot());
                let Some(output) = self.touch_output_for_device(&device) else {
                    tracing::warn!(%device, "ignoring unmapped touch device on multi-output desktop");
                    return;
                };
                let synthetic_slot = self.allocate_touch_slot();
                self.touch_slots
                    .insert((device, slot), (output, synthetic_slot));
                let size = self.desktop.outputs[&output].output.logical_size;
                let location = event.position_transformed(
                    (saturating_i32(size.width), saturating_i32(size.height)).into(),
                );
                self.active_output = output;
                let hit = self.surface_under(location);
                if let Some((surface, _, window)) = &hit {
                    self.focus_interaction_target(surface, *window);
                } else {
                    self.sync_keyboard_focus();
                }
                let focus = hit.map(|(surface, origin, _)| (surface, origin));
                let recipient = focus
                    .as_ref()
                    .and_then(|(surface, _)| surface.client())
                    .map(|client| client.id());
                let touch = self.touch.clone();
                let serial = self.next_serial();
                touch.down(
                    self,
                    focus,
                    &DownEvent {
                        slot: synthetic_slot,
                        location,
                        serial,
                        time: event.time_msec(),
                    },
                );
                if let Some(recipient) = recipient {
                    self.activation_tracker
                        .remember(serial, recipient, self.clock.now());
                }
            }
            InputEvent::TouchMotion { event } => {
                let key = (event.device().id(), i32::from(event.slot()));
                let Some((output, synthetic_slot)) = self.touch_slots.get(&key).copied() else {
                    return;
                };
                let size = self.desktop.outputs[&output].output.logical_size;
                let location = event.position_transformed(
                    (saturating_i32(size.width), saturating_i32(size.height)).into(),
                );
                if self
                    .drag
                    .is_some_and(|drag| drag.source == DragSource::Touch(synthetic_slot))
                {
                    self.update_drag(location);
                    return;
                }
                if self
                    .dnd_touch_icon
                    .is_some_and(|(_, slot, _)| slot == synthetic_slot)
                {
                    self.dnd_touch_icon = Some((output, synthetic_slot, location));
                    self.mark_render_dirty();
                }
                let previous_output = self.active_output;
                self.active_output = output;
                let focus = self
                    .surface_under(location)
                    .map(|(surface, origin, _)| (surface, origin));
                self.active_output = previous_output;
                let touch = self.touch.clone();
                touch.motion(
                    self,
                    focus,
                    &SmithayTouchMotionEvent {
                        slot: synthetic_slot,
                        location,
                        time: event.time_msec(),
                    },
                );
            }
            InputEvent::TouchUp { event } => {
                let Some((_, synthetic_slot)) = self
                    .touch_slots
                    .remove(&(event.device().id(), i32::from(event.slot())))
                else {
                    return;
                };
                if self
                    .drag
                    .is_some_and(|drag| drag.source == DragSource::Touch(synthetic_slot))
                {
                    self.finish_drag();
                    return;
                }
                let touch = self.touch.clone();
                let serial = self.next_serial();
                touch.up(
                    self,
                    &UpEvent {
                        slot: synthetic_slot,
                        serial,
                        time: event.time_msec(),
                    },
                );
            }
            InputEvent::TouchCancel { .. } => {
                self.cancel_touch_sequences();
            }
            InputEvent::TouchFrame { .. } => {
                let touch = self.touch.clone();
                touch.frame(self);
            }
            InputEvent::TabletToolProximity { event } => self.handle_tablet_proximity(event),
            InputEvent::TabletToolAxis { event } => self.handle_tablet_axis(event),
            InputEvent::TabletToolTip { event } => self.handle_tablet_tip(event),
            InputEvent::TabletToolButton { event } => self.handle_tablet_button(event),
            InputEvent::PointerButton { event } => self.handle_pointer_button(
                event.button(),
                event.button_code(),
                event.state(),
                event.time_msec(),
            ),
            InputEvent::PointerAxis { event } => self.handle_pointer_axis(event),
            _ => {}
        }
    }

    pub fn bind_touch_device_output(&mut self, device: String, output_key: &str) {
        let output = self.output_aliases.get(output_key).copied().or_else(|| {
            self.desktop
                .outputs
                .iter()
                .find_map(|(id, output)| (output.output.stable_key == output_key).then_some(*id))
        });
        if let Some(output) = output {
            self.touch_device_outputs.insert(device, output);
        }
    }

    pub fn register_output_alias(&mut self, alias: String, output: OutputId) {
        if !self.desktop.outputs.contains_key(&output)
            || self.ambiguous_output_aliases.contains(&alias)
        {
            return;
        }
        if self
            .output_aliases
            .get(&alias)
            .is_some_and(|existing| *existing != output)
        {
            self.output_aliases.remove(&alias);
            self.ambiguous_output_aliases.insert(alias);
        } else {
            self.output_aliases.insert(alias, output);
        }
    }

    fn touch_output_for_device(&self, device: &str) -> Option<OutputId> {
        self.touch_device_outputs
            .get(device)
            .copied()
            .filter(|output| self.desktop.outputs.contains_key(output))
            .or_else(|| (self.desktop.outputs.len() == 1).then_some(self.active_output))
    }

    fn allocate_touch_slot(&mut self) -> smithay::backend::input::TouchSlot {
        loop {
            let slot = Some(self.next_touch_slot).into();
            self.next_touch_slot = self.next_touch_slot.wrapping_add(1);
            if self
                .touch_slots
                .values()
                .all(|(_, active_slot)| *active_slot != slot)
            {
                return slot;
            }
        }
    }

    pub(super) fn cancel_touch_sequences(&mut self) {
        if !touch_state_requires_cancel(self.touch_slots.is_empty(), self.touch.is_grabbed()) {
            return;
        }
        if self
            .drag
            .is_some_and(|drag| matches!(drag.source, DragSource::Touch(_)))
        {
            self.cancel_drag();
        }
        let touch = self.touch.clone();
        touch.cancel(self);
        // Smithay 0.7 may retain already-framed slots on cancel. Replacing the handle is a
        // fail-closed workaround: old wl_touch resources become inert and cannot retain focus.
        self.protocol.seat.remove_touch();
        self.protocol.touch = touch::add_touch(&mut self.protocol.seat);
        self.touch_slots.clear();
    }

    pub(super) fn cancel_surface_bound_input(&mut self) {
        self.cancel_pointer_gesture(0);
        self.cancel_touch_sequences();
        self.cancel_tablet_focus(0);
    }

    fn cancel_pointer_gesture(&mut self, time: u32) {
        let Some(gesture) = self.active_pointer_gesture.take() else {
            return;
        };
        let surface = gesture.surface().clone();
        if !surface.alive() {
            return;
        }
        let serial = self.next_serial();
        let seat = self.seat.clone();
        match gesture {
            ActivePointerGesture::Swipe(_) => surface.gesture_swipe_end(
                &seat,
                self,
                &GestureSwipeEndEvent {
                    serial,
                    time,
                    cancelled: true,
                },
            ),
            ActivePointerGesture::Pinch(_) => surface.gesture_pinch_end(
                &seat,
                self,
                &GesturePinchEndEvent {
                    serial,
                    time,
                    cancelled: true,
                },
            ),
            ActivePointerGesture::Hold(_) => surface.gesture_hold_end(
                &seat,
                self,
                &GestureHoldEndEvent {
                    serial,
                    time,
                    cancelled: true,
                },
            ),
        }
    }

    fn start_swipe_gesture(&mut self, time: u32, fingers: u32) {
        self.cancel_pointer_gesture(time);
        let pointer = self.pointer.clone();
        let Some(surface) = pointer.current_focus() else {
            return;
        };
        let serial = self.next_serial();
        let seat = self.seat.clone();
        surface.gesture_swipe_begin(
            &seat,
            self,
            &GestureSwipeBeginEvent {
                serial,
                time,
                fingers,
            },
        );
        self.active_pointer_gesture = Some(ActivePointerGesture::Swipe(surface));
    }

    fn handle_pointer_motion(
        &mut self,
        location: SmithayPoint<f64, smithay::utils::Logical>,
        time: u32,
    ) {
        self.pointer_location = location;
        let changed_owner = self.active_tablet_cursor.take().is_some();
        self.mark_render_dirty();
        if changed_owner {
            self.refresh_visible_scales();
        }
        // During compositor grabs, clients do not receive motion; the pending tiled placement is
        // committed only on release so the radial solver does not run for every pointer sample.
        if self
            .drag
            .is_some_and(|drag| drag.source == DragSource::Pointer)
        {
            self.update_drag(location);
            return;
        }
        let focus = self.surface_under(location);
        let pointer = self.pointer.clone();
        if let Some(previous) = pointer.current_focus()
            && focus.as_ref().is_none_or(|(next, _, _)| *next != previous)
        {
            // Constraints are surface-focus scoped. Scene changes can move or hide a surface
            // beneath a stationary pointer, so end its old constraint before sending wl_pointer
            // leave/enter. Otherwise a locked pointer can remain attached to an invisible surface
            // and suppress every future motion event.
            self.deactivate_pointer_constraint(&previous);
        }
        let serial = self.next_serial();
        let focus_origin = focus.as_ref().map(|(surface, origin, window)| {
            let scale = window
                .and_then(|window| self.visual_geometry(window).map(|geometry| geometry.2))
                .unwrap_or(1.0);
            (surface.clone(), *origin, scale)
        });
        pointer.motion(
            self,
            focus.map(|(surface, origin, _)| (surface, origin)),
            &MotionEvent {
                location,
                serial,
                time,
            },
        );
        self.pointer_focus_origin = pointer
            .current_focus()
            .and_then(|current| focus_origin.filter(|(surface, _, _)| *surface == current));
        pointer.frame(self);
        self.maybe_activate_pointer_constraint();
    }

    fn handle_absolute_pointer_motion(
        &mut self,
        target: SmithayPoint<f64, smithay::utils::Logical>,
        time: u32,
    ) {
        match self.constrain_pointer_target(target) {
            pointer_constraints::ConstrainedPointerTarget::Locked => {}
            pointer_constraints::ConstrainedPointerTarget::Motion(location) => {
                self.handle_pointer_motion(location, time);
            }
        }
    }

    fn handle_relative_pointer_motion<B: InputBackend, E: PointerMotionEvent<B>>(
        &mut self,
        event: E,
    ) {
        use smithay::input::pointer::RelativeMotionEvent;

        let pointer = self.pointer.clone();
        let previous = self
            .pointer_focus_origin
            .clone()
            .and_then(|(surface, origin, _)| {
                (pointer.current_focus().as_ref() == Some(&surface)).then_some((surface, origin))
            });
        let relative = RelativeMotionEvent {
            delta: event.delta(),
            delta_unaccel: event.delta_unaccel(),
            utime: event.time(),
        };
        let relative_focus = previous;
        if matches!(
            self.active_pointer_constraint(),
            Some(pointer_constraints::ActivePointerConstraint::Locked)
        ) {
            pointer.relative_motion(self, relative_focus, &relative);
            pointer.frame(self);
            return;
        }

        let delta = event.delta_unaccel();
        let constrained = self.active_pointer_constraint().is_some();
        let candidate = if constrained {
            let size = self.desktop.outputs[&self.active_output]
                .output
                .logical_size;
            SmithayPoint::from((
                (self.pointer_location.x + delta.x).clamp(0.0, size.width as f64 - 1.0),
                (self.pointer_location.y + delta.y).clamp(0.0, size.height as f64 - 1.0),
            ))
        } else {
            self.relative_pointer_location(delta.x, delta.y)
        };
        let location = match self.constrain_pointer_target(candidate) {
            pointer_constraints::ConstrainedPointerTarget::Locked => self.pointer_location,
            pointer_constraints::ConstrainedPointerTarget::Motion(location) => location,
        };
        pointer.relative_motion(self, relative_focus, &relative);
        self.handle_pointer_motion(location, event.time_msec());
    }

    fn handle_pointer_axis<B: InputBackend, E: PointerAxisEvent<B>>(&mut self, event: E) {
        if self.active_tablet_cursor.take().is_some() {
            self.mark_render_dirty();
            self.refresh_visible_scales();
        }
        if self.session_is_locked() {
            self.handle_pointer_motion(self.pointer_location, event.time_msec());
        }
        let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
        for axis in [Axis::Horizontal, Axis::Vertical] {
            frame = frame.relative_direction(axis, event.relative_direction(axis));
            if let Some(value) = event.amount(axis) {
                frame = frame.value(axis, value);
                if value == 0.0 {
                    frame = frame.stop(axis);
                }
            }
            if let Some(v120) = event.amount_v120(axis) {
                frame = frame
                    .v120(axis, v120.round() as i32)
                    .value(axis, v120 / 120.0 * 15.0);
            }
        }
        let pointer = self.pointer.clone();
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    fn next_serial(&mut self) -> Serial {
        let serial = self.serial;
        self.serial = self.serial.wrapping_add(1).max(1);
        serial.into()
    }

    fn relative_pointer_location(
        &mut self,
        dx: f64,
        dy: f64,
    ) -> SmithayPoint<f64, smithay::utils::Logical> {
        let previous_output = self.active_output;
        let mut x = self.pointer_location.x + dx;
        let y = self.pointer_location.y + dy;
        if self.drag.is_none() {
            loop {
                let width = self.desktop.outputs[&self.active_output]
                    .output
                    .logical_size
                    .width as f64;
                if x >= width {
                    let Some(next) = self.adjacent_output(self.active_output, true) else {
                        x = width - 1.0;
                        break;
                    };
                    x -= width;
                    self.active_output = next;
                } else if x < 0.0 {
                    let Some(previous) = self.adjacent_output(self.active_output, false) else {
                        x = 0.0;
                        break;
                    };
                    self.active_output = previous;
                    x += self.desktop.outputs[&previous].output.logical_size.width as f64;
                } else {
                    break;
                }
            }
        }
        let size = self.desktop.outputs[&self.active_output]
            .output
            .logical_size;
        let location = SmithayPoint::from((
            x.clamp(0.0, size.width as f64 - 1.0),
            y.clamp(0.0, size.height as f64 - 1.0),
        ));
        if self.active_output != previous_output {
            tracing::debug!(
                from = ?previous_output,
                to = ?self.active_output,
                "pointer crossed output boundary"
            );
            self.sync_keyboard_focus();
            self.refresh_visible_scales();
            self.mark_public_dirty();
        }
        location
    }

    fn adjacent_output(&self, output: OutputId, forward: bool) -> Option<OutputId> {
        let outputs = self.desktop.outputs.keys().copied().collect::<Vec<_>>();
        let index = outputs.iter().position(|candidate| *candidate == output)?;
        if forward {
            outputs.get(index + 1).copied()
        } else {
            index
                .checked_sub(1)
                .and_then(|index| outputs.get(index))
                .copied()
        }
    }

    fn handle_pointer_button(
        &mut self,
        button: Option<MouseButton>,
        button_code: u32,
        state: BackendButtonState,
        time: u32,
    ) {
        if self.active_tablet_cursor.take().is_some() {
            self.mark_render_dirty();
            self.refresh_visible_scales();
        }
        let pointer_drag = self
            .drag
            .is_some_and(|drag| drag.source == DragSource::Pointer);
        let compositor_drag = !self.session_is_locked()
            && button == Some(MouseButton::Left)
            && (pointer_drag || self.keyboard.modifier_state().logo);
        if compositor_drag {
            match state {
                BackendButtonState::Pressed => self.begin_drag(None),
                BackendButtonState::Released => self.finish_drag(),
            }
            return;
        }

        if state == BackendButtonState::Pressed
            && let Some((surface, _, window)) = self.surface_under(self.pointer_location)
        {
            self.focus_interaction_target(&surface, window);
        }
        // Scene-changing IPC/workspace actions may have changed the surface below a stationary
        // pointer. Refresh pointer focus before delivering the button to avoid targeting the
        // surface that occupied this coordinate before the transition.
        let focus = self.surface_under(self.pointer_location);
        let recipient = focus
            .as_ref()
            .and_then(|(surface, _, _)| surface.client())
            .map(|client| client.id());
        let pointer = self.pointer.clone();
        let serial = self.next_serial();
        if state == BackendButtonState::Pressed
            && let Some(recipient) = recipient
        {
            self.activation_tracker
                .remember(serial, recipient, self.clock.now());
        }
        pointer.motion(
            self,
            focus.map(|(surface, origin, _)| (surface, origin)),
            &MotionEvent {
                location: self.pointer_location,
                serial,
                time,
            },
        );
        pointer.button(
            self,
            &ButtonEvent {
                serial,
                time,
                button: button_code,
                state,
            },
        );
        pointer.frame(self);
    }

    pub(super) fn focus_interaction_target(
        &mut self,
        surface: &WlSurface,
        window: Option<WindowId>,
    ) {
        if let Some(window) = window {
            self.on_demand_layer_focus = None;
            if self.desktop.find_window(window).is_ok() && self.desktop.focus_window(window).is_ok()
            {
                self.mark_public_dirty();
            }
        } else if let Some((layer, _target, interactivity)) = self.layer_keyboard_target(surface) {
            self.on_demand_layer_focus =
                (interactivity == KeyboardInteractivity::OnDemand).then_some(layer);
            // Route every press through the common arbiter. A top/overlay exclusive layer must
            // remain focused even when another on-demand layer is pressed.
        }
        self.sync_keyboard_focus();
    }

    fn begin_drag(
        &mut self,
        requested: Option<(
            WindowId,
            DragSource,
            SmithayPoint<f64, smithay::utils::Logical>,
        )>,
    ) {
        let (window, source, location) = match requested {
            Some(requested) => requested,
            None => {
                let Some((_, _, Some(window))) = self.surface_under(self.pointer_location) else {
                    return;
                };
                (window, DragSource::Pointer, self.pointer_location)
            }
        };
        let Ok(workspace_id) = self.desktop.find_window(window) else {
            return;
        };
        let Ok(location_on_desktop) = self.desktop.workspace_location(workspace_id) else {
            return;
        };
        let Some(output) = location_on_desktop.output else {
            return;
        };
        let Some((origin, _, _, mode)) = self.visual_geometry_for_output(output, window) else {
            return;
        };
        if matches!(
            mode,
            WindowMode::Maximized | WindowMode::Fullscreen | WindowMode::Minimized
        ) {
            return;
        }
        let Ok(workspace) = self.desktop.workspace(workspace_id) else {
            return;
        };
        let start = match mode {
            WindowMode::Tiled => workspace.tiled[&window].geometry.origin,
            WindowMode::Floating => workspace.floating[&window].viewport.rect.origin,
            WindowMode::Maximized | WindowMode::Fullscreen | WindowMode::Minimized => {
                unreachable!()
            }
        };
        let size = workspace.window_size(window).unwrap();
        let start = astera_core::Rect::new(start.x, start.y, size.width, size.height);
        self.active_output = output;
        self.drag = Some(DragState {
            window,
            output,
            mode,
            kind: DragKind::Move,
            source,
            grab_offset: (location.x - origin.x as f64, location.y - origin.y as f64),
            pointer_start: (location.x, location.y),
            min_size: Size::new(1, 1),
            max_size: Size::new(i64::MAX, i64::MAX),
            target: start,
            start,
        });
        tracing::debug!(?window, ?workspace_id, ?mode, "compositor drag started");
        if self.desktop.focus_window(window).is_ok() {
            self.mark_public_dirty();
        }
        self.sync_keyboard_focus();
    }

    fn begin_resize(
        &mut self,
        window: WindowId,
        edges: ResizeEdges,
        source: DragSource,
        location: SmithayPoint<f64, smithay::utils::Logical>,
    ) {
        let Ok(workspace_id) = self.desktop.find_window(window) else {
            return;
        };
        let Ok(location_on_desktop) = self.desktop.workspace_location(workspace_id) else {
            return;
        };
        let Some(output) = location_on_desktop.output else {
            return;
        };
        let Some((origin, _, _, mode)) = self.visual_geometry_for_output(output, window) else {
            return;
        };
        if matches!(
            mode,
            WindowMode::Maximized | WindowMode::Fullscreen | WindowMode::Minimized
        ) {
            return;
        }
        let workspace = self.desktop.workspace(workspace_id).unwrap();
        let start = match mode {
            WindowMode::Tiled => workspace.tiled[&window].geometry,
            WindowMode::Floating => workspace.floating[&window].viewport.rect,
            WindowMode::Maximized | WindowMode::Fullscreen | WindowMode::Minimized => {
                unreachable!()
            }
        };
        let mapped = self
            .windows
            .iter()
            .find(|mapped| mapped.id == window)
            .unwrap();
        let (min_size, max_size) = with_states(mapped.surface.wl_surface(), |states| {
            let cached = *states.cached_state.get::<SurfaceCachedState>().current();
            let min_width = i64::from(cached.min_size.w.max(1));
            let min_height = i64::from(cached.min_size.h.max(1));
            (
                Size::new(min_width, min_height),
                Size::new(
                    if cached.max_size.w > 0 {
                        i64::from(cached.max_size.w).max(min_width)
                    } else {
                        i64::MAX
                    },
                    if cached.max_size.h > 0 {
                        i64::from(cached.max_size.h).max(min_height)
                    } else {
                        i64::MAX
                    },
                ),
            )
        });
        self.active_output = output;
        self.drag = Some(DragState {
            window,
            output,
            mode,
            kind: DragKind::Resize(edges),
            source,
            grab_offset: (location.x - origin.x as f64, location.y - origin.y as f64),
            pointer_start: (location.x, location.y),
            min_size,
            max_size,
            target: start,
            start,
        });
        if self.desktop.focus_window(window).is_ok() {
            self.mark_public_dirty();
        }
        self.sync_keyboard_focus();
        self.configure_resize_preview(window, start.size, true);
    }

    fn configure_resize_preview(&self, window: WindowId, size: Size, resizing: bool) {
        let Some(mapped) = self.windows.iter().find(|mapped| mapped.id == window) else {
            return;
        };
        mapped.surface.with_pending_state(|state| {
            state.size = Some((saturating_i32(size.width), saturating_i32(size.height)).into());
            if resizing {
                state.states.set(xdg_toplevel::State::Resizing);
            } else {
                state.states.unset(xdg_toplevel::State::Resizing);
            }
        });
        mapped.surface.send_pending_configure();
    }

    fn layer_keyboard_target(
        &self,
        surface: &WlSurface,
    ) -> Option<(u64, WlSurface, KeyboardInteractivity)> {
        self.layers.iter().find_map(|mapped| {
            let root = mapped.surface.wl_surface();
            let belongs_to_layer = surface_tree_contains(root, surface)
                || PopupManager::popups_for_surface(root)
                    .any(|(popup, _)| surface_tree_contains(popup.wl_surface(), surface));
            if !mapped.mapped || !belongs_to_layer {
                return None;
            }
            let state = with_states(mapped.surface.wl_surface(), |states| {
                *states
                    .cached_state
                    .get::<LayerSurfaceCachedState>()
                    .current()
            });
            (state.keyboard_interactivity != KeyboardInteractivity::None).then(|| {
                (
                    mapped.id,
                    mapped.surface.wl_surface().clone(),
                    state.keyboard_interactivity,
                )
            })
        })
    }

    fn update_drag(&mut self, location: SmithayPoint<f64, smithay::utils::Logical>) {
        let Some(mut drag) = self.drag else {
            return;
        };
        drag.target = match drag.kind {
            DragKind::Move => {
                let viewport_x = location.x - drag.grab_offset.0;
                let viewport_y = location.y - drag.grab_offset.1;
                let origin = if drag.mode == WindowMode::Floating {
                    Point::new(viewport_x.round() as i64, viewport_y.round() as i64)
                } else {
                    let output = &self.desktop.outputs[&drag.output];
                    let workspace = self.desktop.workspace_for_output(drag.output).unwrap();
                    let left = workspace.camera.center.x as f64
                        - output.output.logical_size.width as f64 / 2.0;
                    let top = workspace.camera.center.y as f64
                        - output.output.logical_size.height as f64 / 2.0;
                    Point::new(
                        (left + viewport_x).round() as i64,
                        (top + viewport_y).round() as i64,
                    )
                };
                astera_core::Rect {
                    origin,
                    size: drag.start.size,
                }
            }
            DragKind::Resize(edges) => resized_rect(
                drag.start,
                drag.pointer_start,
                location,
                edges,
                drag.min_size,
                drag.max_size,
            ),
        };
        self.drag = Some(drag);
        if matches!(drag.kind, DragKind::Resize(_)) {
            self.configure_resize_preview(drag.window, drag.target.size, true);
        }
        // The preview geometry is already authoritative for rendering. Reactive popups must be
        // constrained against that same moving/resizing parent, not wait for the drag transaction.
        self.reconstrain_reactive_popups();
    }

    fn finish_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        match drag.source {
            DragSource::Pointer if self.pointer.is_grabbed() => {
                let pointer = self.pointer.clone();
                let serial = self.next_serial();
                pointer.unset_grab(self, serial, 0);
            }
            DragSource::Touch(_) if self.touch.is_grabbed() => {
                let touch = self.touch.clone();
                touch.unset_grab(self);
            }
            _ => {}
        }
        let Ok(workspace) = self.desktop.find_window(drag.window) else {
            return;
        };
        let viewport_size = self.desktop.outputs[&drag.output].output.logical_size;
        let direction = astera_core::Direction::between(
            drag.start.center(),
            drag.target.center(),
            self.desktop
                .workspace(workspace)
                .unwrap()
                .layout_direction_hint,
        );
        let transaction = match (drag.mode, drag.kind) {
            (WindowMode::Tiled, DragKind::Move) => WindowTransaction::MoveTiledFinished {
                id: drag.window,
                target: drag.target.origin,
                seed_direction: direction,
            },
            (WindowMode::Floating, DragKind::Move) => WindowTransaction::MoveFloating {
                id: drag.window,
                target: drag.target,
                viewport_size,
            },
            (WindowMode::Tiled, DragKind::Resize(_)) => WindowTransaction::ResizeTiledFinished {
                id: drag.window,
                target: drag.target,
                seed_direction: direction,
            },
            (WindowMode::Floating, DragKind::Resize(_)) => WindowTransaction::ResizeFloating {
                id: drag.window,
                target: drag.target,
                viewport_size,
            },
            (WindowMode::Maximized | WindowMode::Fullscreen | WindowMode::Minimized, _) => return,
        };
        if let Err(error) = self.desktop.apply_window(workspace, transaction) {
            tracing::warn!(%error, window = ?drag.window, "drag transaction failed");
        } else {
            if matches!(drag.kind, DragKind::Resize(_)) {
                self.configure_resize_preview(drag.window, drag.target.size, false);
            }
            self.mark_public_dirty();
            tracing::info!(
                window = ?drag.window,
                ?workspace,
                mode = ?drag.mode,
                from = ?drag.start,
                to = ?drag.target,
                "compositor drag committed"
            );
        }
    }

    pub(super) fn cancel_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if matches!(drag.kind, DragKind::Resize(_)) {
            self.configure_resize_preview(drag.window, drag.start.size, false);
        }
        match drag.source {
            DragSource::Pointer if self.pointer.is_grabbed() => {
                let pointer = self.pointer.clone();
                let serial = self.next_serial();
                pointer.unset_grab(self, serial, 0);
            }
            DragSource::Touch(_) if self.touch.is_grabbed() => {
                let touch = self.touch.clone();
                touch.unset_grab(self);
            }
            _ => {}
        }
        self.mark_render_dirty();
    }

    fn handle_binding(
        &mut self,
        modifiers: &ModifiersState,
        symbol: Option<u32>,
        keycode: smithay::backend::input::Keycode,
    ) -> bool {
        let modifiers = BindingModifiers::from_state(
            modifiers.ctrl,
            modifiers.alt,
            modifiers.shift,
            modifiers.logo,
        );
        // Prefer an explicitly configured physical key over the layout-dependent keysym.
        // XKB keycodes are evdev codes plus eight, so remove the offset for config lookup.
        let binding = keycode
            .raw()
            .checked_sub(8)
            .and_then(|code| self.config.bindings.get(&BindingKey::code(modifiers, code)))
            .or_else(|| {
                symbol.and_then(|symbol| {
                    self.config
                        .bindings
                        .get(&BindingKey::keysym(modifiers, symbol))
                })
            })
            .cloned();
        let Some(binding) = binding else {
            return false;
        };
        if let Err(message) = self.execute_action(binding.action.clone()) {
            tracing::warn!(%message, "key binding action failed");
        }
        if binding.repeat {
            // Registration happens only after the initial action, matching normal key repeat.
            self.key_repeat.register(
                keycode,
                modifiers,
                binding.action,
                self.config.key_repeat.delay_ms,
                self.clock.now(),
            );
        }
        true
    }

    pub fn process_key_repeats(&mut self) {
        if self.session_is_locked()
            || self.seat.keyboard_shortcuts_inhibited()
            || self.exclusive_layer_has_keyboard_focus()
        {
            self.key_repeat.cancel_repeats();
            return;
        }
        // Read modifiers again on every tick; releasing a modifier cancels the held action.
        let state = self.keyboard.modifier_state();
        let current = BindingModifiers::from_state(state.ctrl, state.alt, state.shift, state.logo);
        let Some(action) =
            self.key_repeat
                .next_action(self.clock.now(), current, self.config.key_repeat.rate)
        else {
            return;
        };
        if let Err(message) = self.execute_action(action) {
            tracing::warn!(%message, "repeated key binding action failed");
        }
    }

    pub fn next_timer_deadline(&self) -> Option<std::time::Instant> {
        [
            self.next_visual_timer_deadline(),
            self.idle_runtime.deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub fn next_visual_timer_deadline(&self) -> Option<std::time::Instant> {
        [
            self.key_repeat.deadline(),
            self.config_watcher
                .as_ref()
                .and_then(ConfigWatcher::deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub fn process_idle_timers(&mut self) {
        let events = self.idle_runtime.process_due(self.clock.now());
        self.send_idle_events(events);
    }

    fn send_idle_events(&self, events: Vec<IdleEvent>) {
        for event in events {
            match event {
                IdleEvent::Idled(id) => {
                    if let Some(resource) = self.idle_notifications.get(&id) {
                        resource.idled();
                    }
                }
                IdleEvent::Resumed(id) => {
                    if let Some(resource) = self.idle_notifications.get(&id) {
                        resource.resumed();
                    }
                }
            }
        }
    }

    fn execute_action(&mut self, action: Action) -> AnyResult<()> {
        // Resolve focus once so every focused-window action observes the same state snapshot.
        let focused = self
            .desktop
            .workspace_for_output(self.active_output)
            .and_then(|workspace| workspace.focused_window);
        let command = match action {
            // Process actions bypass IPC but still return errors through the same binding path.
            Action::Spawn(argv) => return process::spawn(argv),
            Action::FocusWorkspace { workspace } => Some(Command::FocusWorkspace {
                workspace: self.resolve_binding_workspace(workspace)?,
            }),
            Action::MoveWindowToWorkspace {
                workspace,
                activate,
            } => Some(Command::MoveWindow {
                window: focused.ok_or_else(|| anyhow!("no focused window"))?.into(),
                target: self.resolve_binding_workspace(workspace)?,
                activate,
            }),
            Action::FocusOutput { output } => {
                Some(Command::FocusOutput(OutputSelector::Key(output)))
            }
            Action::MoveWorkspaceToOutput {
                output,
                index,
                activate,
            } => Some(Command::MoveWorkspace {
                workspace: self
                    .desktop
                    .active_workspace_id(self.active_output)
                    .ok_or_else(|| anyhow!("active output has no workspace"))?
                    .into(),
                target_output: OutputSelector::Key(output),
                target_index: index.map(|index| u32::try_from(index - 1)).transpose()?,
                activate,
            }),
            Action::FocusDirection(direction) => {
                Some(Command::FocusDirection(direction.as_direction().into()))
            }
            Action::PanCamera { x, y } => Some(Command::PanCamera {
                workspace: self
                    .desktop
                    .active_workspace_id(self.active_output)
                    .ok_or_else(|| anyhow!("active output has no workspace"))?
                    .into(),
                dx: x,
                dy: y,
            }),
            Action::SetWindowMode(mode) => Some(Command::SetWindowMode {
                window: focused.ok_or_else(|| anyhow!("no focused window"))?.into(),
                mode: mode.into(),
            }),
            Action::ToggleFloating => Some(Command::SetWindowMode {
                window: focused.ok_or_else(|| anyhow!("no focused window"))?.into(),
                mode: self.toggle_floating_mode(focused.unwrap())?.into(),
            }),
            Action::ToggleFullscreen => Some(Command::SetWindowMode {
                window: focused.ok_or_else(|| anyhow!("no focused window"))?.into(),
                mode: self.toggle_fullscreen_mode(focused.unwrap())?.into(),
            }),
            Action::CloseWindow => {
                let window = focused.ok_or_else(|| anyhow!("no focused window"))?;
                let mapped = self
                    .windows
                    .iter()
                    .find(|mapped| mapped.id == window)
                    .ok_or_else(|| anyhow!("focused window is not mapped"))?;
                // Request a cooperative close; never terminate the owning client process here.
                mapped.surface.send_close();
                None
            }
            Action::Quit => {
                self.should_quit = true;
                None
            }
        };
        if let Some(command) = command {
            // Reuse the command executor so bindings and IPC have identical transaction rules.
            match self.execute_command(command) {
                Response::Success(_) => Ok(()),
                Response::Error(error) => Err(anyhow!(error.message)),
            }
        } else {
            Ok(())
        }
    }

    fn resolve_binding_workspace(
        &self,
        selector: BindingWorkspaceSelector,
    ) -> AnyResult<WorkspaceSelector> {
        Ok(match selector {
            BindingWorkspaceSelector::Index(index, output) => WorkspaceSelector::LocalIndex {
                output: output
                    .map(OutputSelector::Key)
                    .unwrap_or(OutputSelector::Active),
                index: u32::try_from(index)
                    .map_err(|_| anyhow!("binding workspace index exceeds protocol range"))?,
            },
            BindingWorkspaceSelector::Name(name) => WorkspaceSelector::Name(name),
            BindingWorkspaceSelector::Id(id) => WorkspaceSelector::Id(WorkspaceId(id).into()),
        })
    }

    fn toggle_floating_mode(&self, window: WindowId) -> AnyResult<WindowMode> {
        let workspace = self.desktop.find_window(window)?;
        match self
            .desktop
            .workspace(workspace)
            .unwrap()
            .window_mode(window)
        {
            Some(WindowMode::Floating) => Ok(WindowMode::Tiled),
            Some(WindowMode::Tiled | WindowMode::Maximized | WindowMode::Fullscreen) => {
                Ok(WindowMode::Floating)
            }
            Some(WindowMode::Minimized) => Err(anyhow!("minimized window must be restored first")),
            None => Err(anyhow!("focused window has no mode")),
        }
    }

    fn toggle_fullscreen_mode(&self, window: WindowId) -> AnyResult<WindowMode> {
        let workspace = self.desktop.find_window(window)?;
        let state = self.desktop.workspace(workspace).unwrap();
        match state.window_mode(window) {
            Some(WindowMode::Fullscreen) => match &state.fullscreen.as_ref().unwrap().restore {
                astera_core::FullscreenRestorePlacement::Tiled { .. } => Ok(WindowMode::Tiled),
                astera_core::FullscreenRestorePlacement::Floating { .. } => {
                    Ok(WindowMode::Floating)
                }
                astera_core::FullscreenRestorePlacement::Maximized { .. } => {
                    Ok(WindowMode::Maximized)
                }
            },
            Some(WindowMode::Tiled | WindowMode::Floating | WindowMode::Maximized) => {
                Ok(WindowMode::Fullscreen)
            }
            Some(WindowMode::Minimized) => Err(anyhow!("minimized window must be restored first")),
            None => Err(anyhow!("focused window has no mode")),
        }
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn set_output_configuration_supported(&mut self, supported: bool) {
        self.output_configuration_supported = supported;
    }

    pub fn watch_config(&mut self, path: std::path::PathBuf) {
        tracing::info!(path = %path.display(), "configuration watcher enabled");
        let watcher = match ConfigWatcher::new(path.clone()) {
            Ok(watcher) => watcher,
            Err(error) => {
                tracing::error!(path = %path.display(), %error, "could not watch configuration");
                self.config_failed = true;
                self.config_error = Some(error.to_string());
                self.mark_public_dirty();
                return;
            }
        };
        self.config_source = watcher
            .exists()
            .then(|| watcher.path().to_string_lossy().into_owned());
        self.config_watcher = Some(watcher);
        self.mark_public_dirty();
    }

    pub fn config_watch_fd(&mut self) -> std::io::Result<Option<OwnedFd>> {
        self.config_watcher
            .as_mut()
            .map(ConfigWatcher::duplicate_fd)
            .transpose()
    }

    pub fn notify_config_changed(&mut self) {
        let Some(watcher) = self.config_watcher.as_mut() else {
            return;
        };
        if let Err(error) = watcher.notify(self.clock.now()) {
            tracing::error!(%error, "configuration watcher read failed");
        }
    }

    pub fn poll_config(&mut self) {
        let Some(watcher) = self.config_watcher.as_mut() else {
            return;
        };
        let path = watcher.path().to_owned();
        let Some(result) = watcher.poll(self.clock.now()) else {
            return;
        };
        match result {
            Ok(config) => {
                // apply_config is transactional; a rejected layout keeps the old config alive.
                if let Err(error) = self.apply_config(config) {
                    tracing::error!(path = %path.display(), %error, "configuration reload rejected");
                    self.record_config_loaded(Some(error.to_string()));
                } else {
                    self.config_source =
                        path.is_file().then(|| path.to_string_lossy().into_owned());
                    self.record_config_loaded(None);
                }
            }
            Err(error) => {
                tracing::error!(path = %path.display(), %error, "configuration reload failed");
                self.record_config_loaded(Some(error.to_string()));
            }
        }
    }

    fn apply_config(&mut self, config: Config) -> AnyResult<()> {
        if config.input_service != self.config.input_service {
            anyhow::bail!("changing input-service requires restarting the compositor");
        }
        // Validate layout changes on a clone before publishing any part of the new config.
        let mut desktop = self.desktop.clone();
        desktop.reconfigure_layout(config.gap, config.camera)?;
        self.keyboard.change_repeat_info(
            config.key_repeat.rate as i32,
            config.key_repeat.delay_ms as i32,
        );
        // Existing repeat actions belong to the old binding map and must not survive reload.
        self.key_repeat.cancel_repeats();
        self.desktop = desktop;
        self.config = config;
        self.mark_public_dirty();
        tracing::info!(
            bindings = self.config.bindings.len(),
            "configuration reloaded"
        );
        Ok(())
    }

    fn map_buffered_toplevels(&mut self) {
        let pending = self
            .windows
            .iter()
            .enumerate()
            .filter_map(|(index, window)| {
                (!window.mapped
                    && with_renderer_surface_state(window.surface.wl_surface(), |state| {
                        state.buffer().is_some()
                    })
                    .unwrap_or(false))
                .then_some(index)
            })
            .collect::<Vec<_>>();
        for index in pending {
            self.map_toplevel(index);
        }
    }

    fn map_toplevel(&mut self, index: usize) {
        let Some(workspace_id) = self.desktop.active_workspace_id(self.active_output) else {
            return;
        };
        let id = self.windows[index].id;
        let workspace = self.desktop.workspace(workspace_id).unwrap();
        // New toplevels occupy the center of the currently visible world.  The radial solver then
        // displaces every conflicting tiled window away from that point.  Anchoring to the
        // previously focused window made insertion depend on focus mode and could even place a
        // window at the world origin after the camera had moved.
        let anchor = workspace.camera.center;
        let transaction = WindowTransaction::InsertTiled {
            id,
            size: DEFAULT_WINDOW_SIZE,
            anchor,
            seed_direction: workspace.layout_direction_hint,
        };
        if let Err(error) = self.desktop.apply_window(workspace_id, transaction) {
            tracing::error!(?id, %error, "could not map toplevel");
            return;
        }
        let initial_mode = self.windows[index].initial_mode.take();
        if let Some(mode) = initial_mode
            && let Err(error) = self.desktop.apply_window(
                workspace_id,
                WindowTransaction::SetMode {
                    id,
                    mode,
                    viewport_size: self.desktop.outputs[&self.active_output]
                        .output
                        .logical_size,
                },
            )
        {
            tracing::warn!(?id, ?mode, %error, "could not apply initial toplevel mode");
        }
        self.windows[index].mapped = true;
        self.mark_public_dirty();
        self.windows[index].surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });
        if let Some(mode) = initial_mode {
            self.configure_window_mode(id, mode);
        } else {
            self.windows[index].surface.send_pending_configure();
        }
        tracing::info!(window = ?id, workspace = ?workspace_id, output = ?self.active_output, "toplevel mapped");
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
    }

    fn unmap_toplevel(&mut self, index: usize) {
        self.cancel_surface_bound_input();
        let id = self.windows[index].id;
        if let Ok(workspace) = self.desktop.find_window(id)
            && let Err(error) = self
                .desktop
                .apply_window(workspace, WindowTransaction::Remove { id })
        {
            tracing::warn!(?id, %error, "could not unmap toplevel");
            return;
        }
        self.windows[index].mapped = false;
        self.mark_public_dirty();
        if self.drag.is_some_and(|drag| drag.window == id) {
            self.cancel_drag();
        }
        tracing::info!(window = ?id, "toplevel unmapped");
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
        self.handle_pointer_motion(self.pointer_location, 0);
    }

    pub fn remove_dead_windows(&mut self) {
        self.popup_manager.cleanup();
        if self.dnd_icon.as_ref().is_some_and(|icon| !icon.alive()) {
            self.dnd_icon = None;
            self.dnd_touch_icon = None;
            self.mark_render_dirty();
        }
        let layer_count = self.layers.len();
        self.layers.retain(|mapped| mapped.surface.alive());
        let dead: Vec<_> = self
            .windows
            .iter()
            .filter(|mapped| !mapped.surface.alive())
            .map(|mapped| mapped.id)
            .collect();
        self.windows.retain(|mapped| mapped.surface.alive());
        let scene_changed = self.layers.len() != layer_count || !dead.is_empty();
        if scene_changed {
            self.cancel_surface_bound_input();
            self.mark_public_dirty();
        }
        for id in dead {
            if let Ok(workspace) = self.desktop.find_window(id) {
                match self
                    .desktop
                    .apply_window(workspace, WindowTransaction::Remove { id })
                {
                    Ok(()) => tracing::info!(window = ?id, ?workspace, "window removed"),
                    Err(error) => {
                        tracing::warn!(window = ?id, ?workspace, %error, "window removal failed")
                    }
                }
            }
        }
        self.refresh_visible_scales();
        if scene_changed {
            self.sync_keyboard_focus();
            self.handle_pointer_motion(self.pointer_location, 0);
        }
    }
}

fn touch_state_requires_cancel(slots_empty: bool, grabbed: bool) -> bool {
    !slots_empty || grabbed
}

fn resized_rect(
    start: astera_core::Rect,
    pointer_start: (f64, f64),
    pointer: SmithayPoint<f64, smithay::utils::Logical>,
    edges: ResizeEdges,
    min_size: Size,
    max_size: Size,
) -> astera_core::Rect {
    let dx = (pointer.x - pointer_start.0).round() as i64;
    let dy = (pointer.y - pointer_start.1).round() as i64;
    let right = start.origin.x.saturating_add(start.size.width);
    let bottom = start.origin.y.saturating_add(start.size.height);
    let mut left = start.origin.x;
    let mut top = start.origin.y;
    let mut new_right = right;
    let mut new_bottom = bottom;
    if edges.left {
        left = start
            .origin
            .x
            .saturating_add(dx)
            .min(right.saturating_sub(1));
    }
    if edges.right {
        new_right = right.saturating_add(dx).max(left.saturating_add(1));
    }
    if edges.top {
        top = start
            .origin
            .y
            .saturating_add(dy)
            .min(bottom.saturating_sub(1));
    }
    if edges.bottom {
        new_bottom = bottom.saturating_add(dy).max(top.saturating_add(1));
    }
    let width = new_right
        .saturating_sub(left)
        .clamp(min_size.width.max(1), max_size.width.max(min_size.width));
    let height = new_bottom
        .saturating_sub(top)
        .clamp(min_size.height.max(1), max_size.height.max(min_size.height));
    if edges.left {
        left = right.saturating_sub(width);
    } else {
        new_right = left.saturating_add(width);
    }
    if edges.top {
        top = bottom.saturating_sub(height);
    } else {
        new_bottom = top.saturating_add(height);
    }
    astera_core::Rect::new(
        left,
        top,
        new_right.saturating_sub(left).max(1),
        new_bottom.saturating_sub(top).max(1),
    )
}

mod command;

mod protocol;

fn extend_surface_tree(surfaces: &mut HashSet<WlSurface>, root: &WlSurface) {
    with_surface_tree_downward(
        root,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |surface, _, &()| {
            surfaces.insert(surface.clone());
        },
        |_, _, &()| true,
    );
}

fn surface_tree_contains(root: &WlSurface, wanted: &WlSurface) -> bool {
    let found = std::cell::Cell::new(false);
    with_surface_tree_downward(
        root,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |surface, _, &()| found.set(found.get() || surface == wanted),
        |_, _, &()| !found.get(),
    );
    found.get()
}

#[derive(Default)]
pub struct ClientState {
    compositor_state: CompositorClientState,
    trusted_input: bool,
}

impl ClientState {
    pub(crate) fn trusted_input() -> Self {
        Self {
            trusted_input: true,
            ..Self::default()
        }
    }
}

impl ClientData for ClientState {
    fn initialized(&self, client_id: ClientId) {
        tracing::debug!(?client_id, "Wayland client initialized");
    }

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        tracing::debug!(?client_id, ?reason, "Wayland client disconnected");
    }
}

#[cfg(test)]
mod tests;
