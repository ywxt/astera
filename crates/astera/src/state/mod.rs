use std::{
    collections::{BTreeMap, HashSet},
    ops::{Deref, DerefMut},
    os::fd::OwnedFd,
    sync::Arc,
};

mod clock;
mod config_watcher;
mod geometry;
mod key_repeat;
mod model;
mod process;
#[cfg(test)]
mod snapshot;

use clock::{Clock, SystemClock};
use config_watcher::ConfigWatcher;
use geometry::{
    layer_rank, mode_layer, output_transform, physical_point, point_inside, saturating_i32,
};
use key_repeat::KeyRepeatState;
use model::{DragState, MappedLayer, MappedWindow, OutputRuntime, ProtocolState};

use astera_config::{
    Action, BindingKey, Config, Modifiers as BindingModifiers,
    WorkspaceSelector as BindingWorkspaceSelector,
};
use astera_core::{
    Desktop, Output, OutputId, OutputTransform, Point, Size, WindowId, WindowMode,
    WindowTransaction, WorkspaceId, WorkspaceTransaction,
};
use astera_ipc::{
    Command, DesktopSnapshot, ErrorCode, OutputSelector, Response, WorkspaceSelector,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, Axis, ButtonState as BackendButtonState, Event, InputBackend,
            InputEvent, KeyState, KeyboardKeyEvent, MouseButton, PointerAxisEvent,
            PointerButtonEvent, PointerMotionEvent,
        },
        renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state},
    },
    delegate_compositor, delegate_data_device, delegate_fractional_scale, delegate_layer_shell,
    delegate_output, delegate_seat, delegate_shm, delegate_viewporter, delegate_xdg_shell,
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, WindowSurfaceType,
        find_popup_root_surface, utils::under_from_surface_tree,
    },
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, Focus as PointerFocusMode, MotionEvent},
    },
    output::{Mode, Output as SmithayOutput, PhysicalProperties, Scale, Subpixel},
    reexports::wayland_server::{
        Client, DisplayHandle,
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::{wl_buffer, wl_output::WlOutput, wl_seat, wl_surface::WlSurface},
    },
    utils::{Physical, Point as SmithayPoint, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
            TraversalAction, with_states, with_surface_tree_downward,
        },
        fractional_scale::{
            FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale,
        },
        output::{OutputHandler, OutputManagerState},
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
        },
        shell::wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerSurface, LayerSurfaceCachedState,
            WlrLayerShellHandler, WlrLayerShellState,
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
        viewporter::ViewporterState,
    },
};

const DEFAULT_WINDOW_SIZE: Size = Size::new(800, 600);

pub struct Astera {
    protocol: ProtocolState,
    output_runtime: BTreeMap<OutputId, OutputRuntime>,
    desktop: Desktop,
    active_output: OutputId,
    windows: Vec<MappedWindow>,
    layers: Vec<MappedLayer>,
    next_window_id: u64,
    pointer_location: SmithayPoint<f64, smithay::utils::Logical>,
    drag: Option<DragState>,
    key_repeat: KeyRepeatState,
    clock: Arc<dyn Clock>,
    config: Config,
    config_watcher: Option<ConfigWatcher>,
    should_quit: bool,
    output_configuration_supported: bool,
    serial: u32,
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
        let xdg_shell_state = XdgShellState::new::<Self>(display);
        let layer_shell_state = WlrLayerShellState::new::<Self>(display);
        let fractional_scale_state = FractionalScaleManagerState::new::<Self>(display);
        let viewporter_state = ViewporterState::new::<Self>(display);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(display);
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
            size: (1, 1).into(),
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
        let data_device_state = DataDeviceState::new::<Self>(display);

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
                layer_shell_state,
                _fractional_scale_state: fractional_scale_state,
                _viewporter_state: viewporter_state,
                _output_manager_state: output_manager_state,
                shm_state,
                seat_state,
                data_device_state,
                popup_manager: PopupManager::default(),
                seat,
                keyboard,
                pointer,
            },
            output_runtime: BTreeMap::from([(
                active_output,
                OutputRuntime {
                    wayland: wayland_output,
                    global: output_global,
                    entered_surfaces: HashSet::new(),
                    location: Point::ORIGIN,
                },
            )]),
            desktop,
            active_output,
            windows: Vec::new(),
            layers: Vec::new(),
            next_window_id: 1,
            pointer_location: (0.0, 0.0).into(),
            drag: None,
            key_repeat: KeyRepeatState::default(),
            clock,
            config,
            config_watcher: None,
            should_quit: false,
            output_configuration_supported: true,
            serial: 1,
        }
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
                location: Point::ORIGIN,
            },
        );
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
        Ok(())
    }

    #[allow(dead_code)] // Used by the native backend's hotplug path.
    pub fn disconnect_output(&mut self, output: OutputId) -> Result<(), astera_core::DesktopError> {
        let event = self.desktop.disconnect_output(output)?;
        let runtime = self
            .output_runtime
            .remove(&output)
            .expect("desktop output has a Wayland runtime");
        for surface in runtime.entered_surfaces {
            runtime.wayland.leave(&surface);
        }
        self.display.disable_global::<Self>(runtime.global);
        self.layers.retain(|mapped| mapped.output != output);
        if self.active_output == output
            && let Some(next) = self.desktop.outputs.keys().next().copied()
        {
            self.active_output = next;
        }
        self.reflow_outputs();
        tracing::info!(?output, ?event, "output disconnected");
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
        Ok(())
    }

    pub fn process_input<B: InputBackend>(&mut self, event: InputEvent<B>) {
        if !self.desktop.outputs.contains_key(&self.active_output) {
            return;
        }
        match event {
            InputEvent::Keyboard { event } => {
                let pressed = event.state() == KeyState::Pressed;
                let key_code = event.key_code();
                let keyboard = self.keyboard.clone();
                let serial = self.next_serial();
                keyboard.input::<(), _>(
                    self,
                    key_code,
                    event.state(),
                    serial,
                    event.time_msec(),
                    move |state, modifiers, key| {
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
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let size = self.desktop.outputs[&self.active_output]
                    .output
                    .logical_size;
                let location = event.position_transformed(
                    (saturating_i32(size.width), saturating_i32(size.height)).into(),
                );
                self.handle_pointer_motion(location, event.time_msec());
            }
            InputEvent::PointerMotion { event } => {
                let delta = event.delta_unaccel();
                let location = self.relative_pointer_location(delta.x, delta.y);
                self.handle_pointer_motion(location, event.time_msec());
            }
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

    fn handle_pointer_motion(
        &mut self,
        location: SmithayPoint<f64, smithay::utils::Logical>,
        time: u32,
    ) {
        self.pointer_location = location;
        if self.drag.is_some() {
            self.update_drag(location);
            return;
        }
        let focus = self.surface_under(location);
        let pointer = self.pointer.clone();
        let serial = self.next_serial();
        pointer.motion(
            self,
            focus.map(|(surface, origin, _)| (surface, origin)),
            &MotionEvent {
                location,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    fn handle_pointer_axis<B: InputBackend, E: PointerAxisEvent<B>>(&mut self, event: E) {
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

    fn surface_under(
        &self,
        location: SmithayPoint<f64, smithay::utils::Logical>,
    ) -> Option<(
        WlSurface,
        SmithayPoint<f64, smithay::utils::Logical>,
        Option<WindowId>,
    )> {
        let mut candidates = Vec::new();
        for (index, mapped) in self.windows.iter().enumerate() {
            let Some((origin, size, scale, mode)) = self.visual_geometry(mapped.id) else {
                continue;
            };
            if point_inside(location, origin, size, scale) {
                let local = SmithayPoint::from((
                    (location.x - origin.x as f64) / scale,
                    (location.y - origin.y as f64) / scale,
                ));
                if let Some((surface, offset)) = under_from_surface_tree(
                    mapped.surface.wl_surface(),
                    local,
                    (0, 0),
                    WindowSurfaceType::ALL,
                ) {
                    candidates.push((
                        (mode_layer(mode), index, 0usize),
                        surface,
                        (
                            origin.x as f64 + f64::from(offset.x) * scale,
                            origin.y as f64 + f64::from(offset.y) * scale,
                        )
                            .into(),
                        Some(mapped.id),
                    ));
                }
            }
            for (popup_index, (popup, popup_offset)) in
                PopupManager::popups_for_surface(mapped.surface.wl_surface()).enumerate()
            {
                let geometry = popup.geometry();
                let popup_origin = Point::new(
                    origin.x + ((popup_offset.x - geometry.loc.x) as f64 * scale).round() as i64,
                    origin.y + ((popup_offset.y - geometry.loc.y) as f64 * scale).round() as i64,
                );
                let popup_size = Size::new(i64::from(geometry.size.w), i64::from(geometry.size.h));
                if point_inside(location, popup_origin, popup_size, scale) {
                    let local = SmithayPoint::from((
                        (location.x - popup_origin.x as f64) / scale,
                        (location.y - popup_origin.y as f64) / scale,
                    ));
                    if let Some((surface, offset)) = under_from_surface_tree(
                        popup.wl_surface(),
                        local,
                        (0, 0),
                        WindowSurfaceType::ALL,
                    ) {
                        candidates.push((
                            (mode_layer(mode), index, popup_index + 1),
                            surface,
                            (
                                popup_origin.x as f64 + f64::from(offset.x) * scale,
                                popup_origin.y as f64 + f64::from(offset.y) * scale,
                            )
                                .into(),
                            Some(mapped.id),
                        ));
                    }
                }
            }
        }
        for (index, mapped) in self.layers.iter().enumerate() {
            if !mapped.mapped || mapped.output != self.active_output {
                continue;
            }
            let Some((origin, size)) = self.layer_geometry(mapped) else {
                continue;
            };
            let order = layer_rank(mapped.layer);
            if point_inside(location, origin, size, 1.0) {
                let local = SmithayPoint::from((
                    location.x - origin.x as f64,
                    location.y - origin.y as f64,
                ));
                if let Some((surface, offset)) = under_from_surface_tree(
                    mapped.surface.wl_surface(),
                    local,
                    (0, 0),
                    WindowSurfaceType::ALL,
                ) {
                    candidates.push((
                        (order, index, 0),
                        surface,
                        (
                            origin.x as f64 + f64::from(offset.x),
                            origin.y as f64 + f64::from(offset.y),
                        )
                            .into(),
                        None,
                    ));
                }
            }
            for (popup_index, (popup, popup_offset)) in
                PopupManager::popups_for_surface(mapped.surface.wl_surface()).enumerate()
            {
                let geometry = popup.geometry();
                let popup_origin = Point::new(
                    origin.x + i64::from(popup_offset.x - geometry.loc.x),
                    origin.y + i64::from(popup_offset.y - geometry.loc.y),
                );
                let popup_size = Size::new(i64::from(geometry.size.w), i64::from(geometry.size.h));
                if point_inside(location, popup_origin, popup_size, 1.0) {
                    let local = SmithayPoint::from((
                        location.x - popup_origin.x as f64,
                        location.y - popup_origin.y as f64,
                    ));
                    if let Some((surface, offset)) = under_from_surface_tree(
                        popup.wl_surface(),
                        local,
                        (0, 0),
                        WindowSurfaceType::ALL,
                    ) {
                        candidates.push((
                            (order, index, popup_index + 1),
                            surface,
                            (
                                popup_origin.x as f64 + f64::from(offset.x),
                                popup_origin.y as f64 + f64::from(offset.y),
                            )
                                .into(),
                            None,
                        ));
                    }
                }
            }
        }
        candidates
            .into_iter()
            .max_by_key(|(order, _, _, _)| *order)
            .map(|(_, surface, origin, id)| (surface, origin, id))
    }

    fn visual_geometry(&self, id: WindowId) -> Option<(Point, Size, f64, WindowMode)> {
        self.visual_geometry_for_output(self.active_output, id)
    }

    fn visual_geometry_for_output(
        &self,
        output_id: OutputId,
        id: WindowId,
    ) -> Option<(Point, Size, f64, WindowMode)> {
        let output = self.desktop.outputs.get(&output_id)?;
        let workspace = self.desktop.workspace_for_output(output_id)?;
        let mode = workspace.window_mode(id)?;
        match mode {
            WindowMode::Tiled => {
                let mut rect = workspace.tiled[&id].geometry;
                if let Some(drag) = self
                    .drag
                    .filter(|drag| drag.window == id && output_id == self.active_output)
                {
                    rect.origin = drag.target;
                }
                let left = workspace.camera.center.x as f64
                    - output.output.logical_size.width as f64 / 2.0;
                let top = workspace.camera.center.y as f64
                    - output.output.logical_size.height as f64 / 2.0;
                Some((
                    Point::new(
                        (rect.origin.x as f64 - left).round() as i64,
                        (rect.origin.y as f64 - top).round() as i64,
                    ),
                    rect.size,
                    1.0,
                    mode,
                ))
            }
            WindowMode::Floating => {
                let mut rect = workspace.floating[&id].viewport.rect;
                if let Some(drag) = self
                    .drag
                    .filter(|drag| drag.window == id && output_id == self.active_output)
                {
                    rect.origin = drag.target;
                }
                Some((rect.origin, rect.size, 1.0, mode))
            }
            WindowMode::Fullscreen => Some((Point::ORIGIN, output.output.logical_size, 1.0, mode)),
        }
    }

    fn handle_pointer_button(
        &mut self,
        button: Option<MouseButton>,
        button_code: u32,
        state: BackendButtonState,
        time: u32,
    ) {
        let compositor_drag = button == Some(MouseButton::Left)
            && (self.drag.is_some() || self.keyboard.modifier_state().logo);
        if compositor_drag {
            match state {
                BackendButtonState::Pressed => self.begin_drag(),
                BackendButtonState::Released => self.finish_drag(),
            }
            return;
        }

        if state == BackendButtonState::Pressed
            && let Some((surface, _, window)) = self.surface_under(self.pointer_location)
        {
            if let Some(window) = window {
                if self.desktop.find_window(window).is_ok() {
                    let _ = self.desktop.focus_window(window);
                }
                self.sync_keyboard_focus();
            } else if self.layer_accepts_keyboard(&surface) {
                let keyboard = self.keyboard.clone();
                let serial = self.next_serial();
                keyboard.set_focus(self, Some(surface), serial);
            }
        }
        // Scene-changing IPC/workspace actions may have changed the surface below a stationary
        // pointer. Refresh pointer focus before delivering the button to avoid targeting the
        // surface that occupied this coordinate before the transition.
        let focus = self.surface_under(self.pointer_location);
        let pointer = self.pointer.clone();
        let serial = self.next_serial();
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

    fn begin_drag(&mut self) {
        let Some((_surface, origin, window)) = self.surface_under(self.pointer_location) else {
            return;
        };
        let Some(window) = window else {
            return;
        };
        let Some((_, _, _, mode)) = self.visual_geometry(window) else {
            return;
        };
        if mode == WindowMode::Fullscreen {
            return;
        }
        let workspace_id = self.desktop.find_window(window).unwrap();
        let Ok(workspace) = self.desktop.workspace(workspace_id) else {
            return;
        };
        let start = match mode {
            WindowMode::Tiled => workspace.tiled[&window].geometry.origin,
            WindowMode::Floating => workspace.floating[&window].viewport.rect.origin,
            WindowMode::Fullscreen => unreachable!(),
        };
        self.drag = Some(DragState {
            window,
            mode,
            grab_offset: (
                self.pointer_location.x - origin.x,
                self.pointer_location.y - origin.y,
            ),
            target: start,
            start,
        });
        tracing::debug!(?window, ?workspace_id, ?mode, "compositor drag started");
        let _ = self.desktop.focus_window(window);
        self.sync_keyboard_focus();
    }

    fn layer_accepts_keyboard(&self, surface: &WlSurface) -> bool {
        self.layers.iter().any(|mapped| {
            if !mapped.mapped || mapped.surface.wl_surface() != surface {
                return false;
            }
            let state = with_states(surface, |states| {
                *states
                    .cached_state
                    .get::<LayerSurfaceCachedState>()
                    .current()
            });
            state.keyboard_interactivity != KeyboardInteractivity::None
        })
    }

    fn update_drag(&mut self, location: SmithayPoint<f64, smithay::utils::Logical>) {
        let Some(mut drag) = self.drag else {
            return;
        };
        let viewport_x = location.x - drag.grab_offset.0;
        let viewport_y = location.y - drag.grab_offset.1;
        drag.target = if drag.mode == WindowMode::Floating {
            Point::new(viewport_x.round() as i64, viewport_y.round() as i64)
        } else {
            let output = &self.desktop.outputs[&self.active_output];
            let workspace = self
                .desktop
                .workspace_for_output(self.active_output)
                .unwrap();
            let left =
                workspace.camera.center.x as f64 - output.output.logical_size.width as f64 / 2.0;
            let top =
                workspace.camera.center.y as f64 - output.output.logical_size.height as f64 / 2.0;
            Point::new(
                (left + viewport_x).round() as i64,
                (top + viewport_y).round() as i64,
            )
        };
        self.drag = Some(drag);
    }

    fn finish_drag(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let Ok(workspace) = self.desktop.find_window(drag.window) else {
            return;
        };
        let viewport_size = self.desktop.outputs[&self.active_output]
            .output
            .logical_size;
        let transaction = match drag.mode {
            WindowMode::Tiled => WindowTransaction::MoveTiledFinished {
                id: drag.window,
                target: drag.target,
                seed_direction: astera_core::Direction::between(
                    drag.start,
                    drag.target,
                    self.desktop
                        .workspace(workspace)
                        .unwrap()
                        .layout_direction_hint,
                ),
            },
            WindowMode::Floating => {
                let size = self.desktop.workspace(workspace).unwrap().floating[&drag.window]
                    .viewport
                    .rect
                    .size;
                WindowTransaction::MoveFloating {
                    id: drag.window,
                    target: astera_core::Rect {
                        origin: drag.target,
                        size,
                    },
                    viewport_size,
                }
            }
            WindowMode::Fullscreen => return,
        };
        if let Err(error) = self.desktop.apply_window(workspace, transaction) {
            tracing::warn!(%error, window = ?drag.window, "drag transaction failed");
        } else {
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

    fn execute_action(&mut self, action: Action) -> Result<(), String> {
        // Resolve focus once so every focused-window action observes the same state snapshot.
        let focused = self
            .desktop
            .workspace_for_output(self.active_output)
            .and_then(|workspace| workspace.focused_window);
        let command = match action {
            // Process actions bypass IPC but still return errors through the same binding path.
            Action::Spawn(argv) => return process::spawn(argv),
            Action::SpawnShell(script) => {
                return process::spawn(vec!["/bin/sh".into(), "-c".into(), script]);
            }
            Action::FocusWorkspace { workspace } => Some(Command::FocusWorkspace {
                workspace: self.resolve_binding_workspace(workspace)?,
            }),
            Action::MoveWindowToWorkspace {
                workspace,
                activate,
            } => Some(Command::MoveWindow {
                window: focused.ok_or_else(|| "no focused window".to_owned())?,
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
                    .ok_or_else(|| "active output has no workspace".to_owned())?,
                target_output: OutputSelector::Key(output),
                target_index: index.map(|index| index - 1),
                activate,
            }),
            Action::FocusDirection(direction) => {
                Some(Command::FocusDirection(direction.as_direction()))
            }
            Action::PanCamera { x, y } => Some(Command::PanCamera {
                workspace: self
                    .desktop
                    .active_workspace_id(self.active_output)
                    .ok_or_else(|| "active output has no workspace".to_owned())?,
                dx: x,
                dy: y,
            }),
            Action::SetWindowMode(mode) => Some(Command::SetWindowMode {
                window: focused.ok_or_else(|| "no focused window".to_owned())?,
                mode,
            }),
            Action::ToggleFloating => Some(Command::SetWindowMode {
                window: focused.ok_or_else(|| "no focused window".to_owned())?,
                mode: self.toggle_floating_mode(focused.unwrap())?,
            }),
            Action::ToggleFullscreen => Some(Command::SetWindowMode {
                window: focused.ok_or_else(|| "no focused window".to_owned())?,
                mode: self.toggle_fullscreen_mode(focused.unwrap())?,
            }),
            Action::CloseWindow => {
                let window = focused.ok_or_else(|| "no focused window".to_owned())?;
                let mapped = self
                    .windows
                    .iter()
                    .find(|mapped| mapped.id == window)
                    .ok_or_else(|| "focused window is not mapped".to_owned())?;
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
                Response::Ok(_) => Ok(()),
                Response::Error { message, .. } => Err(message),
            }
        } else {
            Ok(())
        }
    }

    fn resolve_binding_workspace(
        &self,
        selector: BindingWorkspaceSelector,
    ) -> Result<WorkspaceSelector, String> {
        Ok(match selector {
            BindingWorkspaceSelector::Index(index, output) => WorkspaceSelector::LocalIndex {
                output: output
                    .map(OutputSelector::Key)
                    .unwrap_or(OutputSelector::Active),
                index,
            },
            BindingWorkspaceSelector::Name(name) => WorkspaceSelector::Name(name),
        })
    }

    fn toggle_floating_mode(&self, window: WindowId) -> Result<WindowMode, String> {
        let workspace = self
            .desktop
            .find_window(window)
            .map_err(|error| error.to_string())?;
        match self
            .desktop
            .workspace(workspace)
            .unwrap()
            .window_mode(window)
        {
            Some(WindowMode::Floating) => Ok(WindowMode::Tiled),
            Some(WindowMode::Tiled | WindowMode::Fullscreen) => Ok(WindowMode::Floating),
            None => Err("focused window has no mode".into()),
        }
    }

    fn toggle_fullscreen_mode(&self, window: WindowId) -> Result<WindowMode, String> {
        let workspace = self
            .desktop
            .find_window(window)
            .map_err(|error| error.to_string())?;
        let state = self.desktop.workspace(workspace).unwrap();
        match state.window_mode(window) {
            Some(WindowMode::Fullscreen) => match &state.fullscreen.as_ref().unwrap().restore {
                astera_core::RestorePlacement::Tiled { .. } => Ok(WindowMode::Tiled),
                astera_core::RestorePlacement::Floating { .. } => Ok(WindowMode::Floating),
            },
            Some(WindowMode::Tiled | WindowMode::Floating) => Ok(WindowMode::Fullscreen),
            None => Err("focused window has no mode".into()),
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
        self.config_watcher = Some(ConfigWatcher::new(path));
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
                }
            }
            Err(error) => {
                tracing::error!(path = %path.display(), %error, "configuration reload failed")
            }
        }
    }

    fn apply_config(&mut self, config: Config) -> Result<(), String> {
        // Validate layout changes on a clone before publishing any part of the new config.
        let mut desktop = self.desktop.clone();
        desktop
            .reconfigure_layout(config.gap, config.camera)
            .map_err(|error| error.to_string())?;
        self.keyboard.change_repeat_info(
            config.key_repeat.rate as i32,
            config.key_repeat.delay_ms as i32,
        );
        // Existing repeat actions belong to the old binding map and must not survive reload.
        self.key_repeat.cancel_repeats();
        self.desktop = desktop;
        self.config = config;
        tracing::info!(
            bindings = self.config.bindings.len(),
            "configuration reloaded"
        );
        Ok(())
    }

    fn mapped_windows_for_output(
        &self,
        output: OutputId,
    ) -> impl Iterator<
        Item = (
            &ToplevelSurface,
            SmithayPoint<i32, Physical>,
            f64,
            WindowMode,
        ),
    > {
        let output_scale = self.output_scale(output);
        let mut instances: Vec<_> = self
            .windows
            .iter()
            .filter_map(|mapped| {
                let (origin, _, scale, mode) =
                    self.visual_geometry_for_output(output, mapped.id)?;
                let layer = mode_layer(mode);
                Some((layer, &mapped.surface, origin, scale, mode))
            })
            .collect();
        instances.sort_by_key(|(layer, _, _, _, _)| std::cmp::Reverse(*layer));
        instances
            .into_iter()
            .map(move |(_, surface, origin, scale, mode)| {
                (
                    surface,
                    physical_point(origin, output_scale),
                    scale * output_scale,
                    mode,
                )
            })
    }

    pub fn render_roots(&self) -> Vec<(WlSurface, SmithayPoint<i32, Physical>, f64)> {
        self.render_roots_for_output(self.active_output)
    }

    pub fn render_roots_for_output(
        &self,
        output: OutputId,
    ) -> Vec<(WlSurface, SmithayPoint<i32, Physical>, f64)> {
        // Compute window geometry once. Previously each frame rebuilt and sorted this list twice,
        // then performed another linear surface-to-window lookup for every item.
        let windows = self.mapped_windows_for_output(output).collect::<Vec<_>>();
        let mut roots = Vec::new();
        roots.extend(self.layer_roots(output, Layer::Overlay));
        roots.extend(
            windows
                .iter()
                .filter(|(_, _, _, mode)| *mode == WindowMode::Fullscreen)
                .map(|(surface, location, scale, _)| {
                    (surface.wl_surface().clone(), *location, *scale)
                }),
        );
        roots.extend(self.layer_roots(output, Layer::Top));
        roots.extend(
            windows
                .iter()
                .filter(|(_, _, _, mode)| matches!(mode, WindowMode::Floating | WindowMode::Tiled))
                .map(|(surface, location, scale, _)| {
                    (surface.wl_surface().clone(), *location, *scale)
                }),
        );
        roots.extend(self.layer_roots(output, Layer::Bottom));
        roots.extend(self.layer_roots(output, Layer::Background));
        roots
    }

    pub(crate) fn protocol_output(&self, output: OutputId) -> Option<SmithayOutput> {
        self.output_runtime
            .get(&output)
            .map(|runtime| runtime.wayland.clone())
    }

    fn layer_roots(
        &self,
        output: OutputId,
        wanted: Layer,
    ) -> impl Iterator<Item = (WlSurface, SmithayPoint<i32, Physical>, f64)> + '_ {
        let scale = self.output_scale(output);
        self.layers
            .iter()
            .filter(move |mapped| {
                mapped.mapped && mapped.output == output && mapped.layer == wanted
            })
            .filter_map(move |mapped| {
                let (origin, _) = self.layer_geometry(mapped)?;
                Some((
                    mapped.surface.wl_surface().clone(),
                    physical_point(origin, scale),
                    scale,
                ))
            })
    }

    fn layer_geometry(&self, mapped: &MappedLayer) -> Option<(Point, Size)> {
        let output = self.desktop.outputs.get(&mapped.output)?;
        let requested = with_states(mapped.surface.wl_surface(), |states| {
            *states
                .cached_state
                .get::<LayerSurfaceCachedState>()
                .current()
        });
        let width = if requested.size.w == 0 {
            (output.output.logical_size.width
                - i64::from(requested.margin.left + requested.margin.right))
            .max(1)
        } else {
            i64::from(requested.size.w)
        };
        let height = if requested.size.h == 0 {
            (output.output.logical_size.height
                - i64::from(requested.margin.top + requested.margin.bottom))
            .max(1)
        } else {
            i64::from(requested.size.h)
        };
        let x = if requested.anchor.contains(Anchor::LEFT) {
            i64::from(requested.margin.left)
        } else if requested.anchor.contains(Anchor::RIGHT) {
            output.output.logical_size.width - width - i64::from(requested.margin.right)
        } else {
            (output.output.logical_size.width - width) / 2
        };
        let y = if requested.anchor.contains(Anchor::TOP) {
            i64::from(requested.margin.top)
        } else if requested.anchor.contains(Anchor::BOTTOM) {
            output.output.logical_size.height - height - i64::from(requested.margin.bottom)
        } else {
            (output.output.logical_size.height - height) / 2
        };
        Some((Point::new(x, y), Size::new(width, height)))
    }

    pub fn update_output_size(&mut self, width: i64, height: i64) {
        let size = Size::new(width, height);
        if self.desktop.outputs[&self.active_output]
            .output
            .logical_size
            != size
        {
            let current = self.desktop.outputs[&self.active_output].clone();
            if let Err(error) = self.configure_output(
                self.active_output,
                size,
                size,
                current.output.native_scale,
                current.output.transform,
            ) {
                tracing::error!(%error, "could not resize nested output");
            }
        }
    }

    pub fn configure_output(
        &mut self,
        output: OutputId,
        physical_size: Size,
        logical_size: Size,
        native_scale: astera_core::Scale120,
        transform: OutputTransform,
    ) -> Result<(), astera_core::DesktopError> {
        self.desktop.configure_output(
            output,
            physical_size,
            logical_size,
            native_scale,
            transform,
        )?;
        let mode = Mode {
            size: (
                saturating_i32(physical_size.width),
                saturating_i32(physical_size.height),
            )
                .into(),
            refresh: 60_000,
        };
        let runtime = self
            .output_runtime
            .get(&output)
            .expect("desktop output has a Wayland runtime");
        runtime.wayland.change_current_state(
            Some(mode),
            Some(output_transform(transform)),
            Some(Scale::Fractional(native_scale.0 as f64 / 120.0)),
            None,
        );
        runtime.wayland.set_preferred(mode);
        self.reflow_outputs();
        self.configure_fullscreen_windows();
        self.configure_layer_surfaces();
        self.refresh_visible_scales();
        Ok(())
    }

    fn reflow_outputs(&mut self) {
        let mut x = 0_i64;
        let placements = self
            .desktop
            .outputs
            .iter()
            .map(|(id, output)| {
                let placement = (*id, Point::new(x, 0));
                x = x.saturating_add(output.output.logical_size.width);
                placement
            })
            .collect::<Vec<_>>();
        for (output, location) in placements {
            let runtime = self
                .output_runtime
                .get_mut(&output)
                .expect("desktop output has a Wayland runtime");
            runtime.location = location;
            runtime.wayland.change_current_state(
                None,
                None,
                None,
                Some((saturating_i32(location.x), saturating_i32(location.y)).into()),
            );
        }
    }

    fn output_scale(&self, output: OutputId) -> f64 {
        self.desktop.outputs[&output].output.native_scale.0 as f64 / 120.0
    }

    fn refresh_visible_scales(&mut self) {
        let scenes: BTreeMap<_, _> = self
            .output_runtime
            .keys()
            .copied()
            .map(|output| {
                let scale = self.output_scale(output);
                let roots = self
                    .render_roots_for_output(output)
                    .into_iter()
                    .map(|(surface, _, _)| surface)
                    .collect::<Vec<_>>();
                let mut visible = HashSet::new();
                for root in roots {
                    for (popup, _) in PopupManager::popups_for_surface(&root) {
                        extend_surface_tree(&mut visible, popup.wl_surface());
                    }
                    extend_surface_tree(&mut visible, &root);
                }
                (output, (scale, visible))
            })
            .collect();
        for (output, (scale, visible)) in scenes {
            let runtime = self
                .output_runtime
                .get_mut(&output)
                .expect("scene output has a Wayland runtime");
            for surface in &runtime.entered_surfaces {
                if !visible.contains(surface) {
                    runtime.wayland.leave(surface);
                }
            }
            for surface in &visible {
                if !runtime.entered_surfaces.contains(surface) {
                    runtime.wayland.enter(surface);
                }
                with_states(surface, |states| {
                    with_fractional_scale(states, |fractional| {
                        fractional.set_preferred_scale(scale);
                    });
                });
            }
            runtime.entered_surfaces = visible;
        }
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
        let anchor = workspace
            .focused_window
            .and_then(|focused| workspace.tiled.get(&focused))
            .map(|window| window.geometry.center())
            .unwrap_or(Point::ORIGIN);
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
        self.windows[index].mapped = true;
        self.windows[index].surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });
        self.windows[index].surface.send_pending_configure();
        tracing::info!(window = ?id, workspace = ?workspace_id, output = ?self.active_output, "toplevel mapped");
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
    }

    fn unmap_toplevel(&mut self, index: usize) {
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
        if self.drag.is_some_and(|drag| drag.window == id) {
            self.drag = None;
        }
        tracing::info!(window = ?id, "toplevel unmapped");
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
    }

    pub fn remove_dead_windows(&mut self) {
        self.popup_manager.cleanup();
        self.layers.retain(|mapped| mapped.surface.alive());
        let dead: Vec<_> = self
            .windows
            .iter()
            .filter(|mapped| !mapped.surface.alive())
            .map(|mapped| mapped.id)
            .collect();
        self.windows.retain(|mapped| mapped.surface.alive());
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
    }
}

mod command;

mod protocol;

pub fn send_frames_surface_tree(surface: &WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surface, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

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

#[derive(Default)]
pub struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, client_id: ClientId) {
        tracing::debug!(?client_id, "Wayland client initialized");
    }

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        tracing::debug!(?client_id, ?reason, "Wayland client disconnected");
    }
}

delegate_xdg_shell!(Astera);
delegate_layer_shell!(Astera);
delegate_fractional_scale!(Astera);
delegate_viewporter!(Astera);
delegate_output!(Astera);
delegate_compositor!(Astera);
delegate_shm!(Astera);
delegate_seat!(Astera);
delegate_data_device!(Astera);

#[cfg(test)]
mod tests;
