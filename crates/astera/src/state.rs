use std::{collections::BTreeMap, os::fd::OwnedFd};

use astera_config::Config;
use astera_core::{
    Desktop, Output, OutputId, OutputTransform, Point, Size, WindowId, WindowMode,
    WindowTransaction, Workspace, WorkspaceId, WorkspaceTransaction,
};
use astera_ipc::{Command, DesktopSnapshot, ErrorCode, Response};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, ButtonState as BackendButtonState, Event, InputBackend,
            InputEvent, KeyState, KeyboardKeyEvent, MouseButton, PointerButtonEvent,
            PointerMotionEvent,
        },
        renderer::utils::on_commit_buffer_handler,
    },
    delegate_compositor, delegate_data_device, delegate_fractional_scale, delegate_layer_shell,
    delegate_output, delegate_seat, delegate_shm, delegate_viewporter, delegate_xdg_shell,
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, find_popup_root_surface,
    },
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, ModifiersState, keysyms},
        pointer::{ButtonEvent, Focus as PointerFocusMode, MotionEvent, PointerHandle},
    },
    output::{Mode, Output as SmithayOutput, PhysicalProperties, Scale, Subpixel},
    reexports::wayland_server::{
        Client, DisplayHandle,
        backend::{ClientData, ClientId, DisconnectReason, GlobalId},
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

#[derive(Clone)]
struct MappedWindow {
    id: WindowId,
    surface: ToplevelSurface,
}

#[derive(Clone, Copy, Debug)]
struct DragState {
    window: WindowId,
    mode: WindowMode,
    grab_offset: (f64, f64),
    target: Point,
    start: Point,
}

#[derive(Clone, Debug)]
struct MappedLayer {
    surface: LayerSurface,
    layer: Layer,
    output: OutputId,
}

#[derive(Debug)]
struct OutputRuntime {
    wayland: SmithayOutput,
    global: GlobalId,
    entered_surfaces: Vec<WlSurface>,
}

pub struct Astera {
    display: DisplayHandle,
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    layer_shell_state: WlrLayerShellState,
    _fractional_scale_state: FractionalScaleManagerState,
    _viewporter_state: ViewporterState,
    _output_manager_state: OutputManagerState,
    output_runtime: BTreeMap<OutputId, OutputRuntime>,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    popup_manager: PopupManager,
    seat: Seat<Self>,
    keyboard: smithay::input::keyboard::KeyboardHandle<Self>,
    pointer: PointerHandle<Self>,
    desktop: Desktop,
    active_output: OutputId,
    windows: Vec<MappedWindow>,
    layers: Vec<MappedLayer>,
    next_window_id: u64,
    pointer_location: SmithayPoint<f64, smithay::utils::Logical>,
    drag: Option<DragState>,
    serial: u32,
}

impl Astera {
    pub fn new(display: &DisplayHandle, config: Config) -> Self {
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
            .add_keyboard(Default::default(), 200, 25)
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
        for id in 0..config.workspace_count {
            let mut workspace = Workspace::new(WorkspaceId(id));
            workspace.camera.policy = config.camera;
            desktop
                .add_workspace(workspace)
                .expect("initial workspace is valid");
        }
        desktop
            .apply(WorkspaceTransaction::Bind {
                workspace: WorkspaceId(0),
                output: active_output,
            })
            .expect("initial workspace can bind");

        Self {
            display: display.clone(),
            compositor_state,
            xdg_shell_state,
            layer_shell_state,
            _fractional_scale_state: fractional_scale_state,
            _viewporter_state: viewporter_state,
            _output_manager_state: output_manager_state,
            output_runtime: BTreeMap::from([(
                active_output,
                OutputRuntime {
                    wayland: wayland_output,
                    global: output_global,
                    entered_surfaces: Vec::new(),
                },
            )]),
            shm_state,
            seat_state,
            data_device_state,
            popup_manager: PopupManager::default(),
            seat,
            keyboard,
            pointer,
            desktop,
            active_output,
            windows: Vec::new(),
            layers: Vec::new(),
            next_window_id: 1,
            pointer_location: (0.0, 0.0).into(),
            drag: None,
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
                entered_surfaces: Vec::new(),
            },
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
        if self.active_output == output {
            if let Some(next) = self.desktop.outputs.keys().next().copied() {
                self.active_output = next;
            }
        }
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
                let keyboard = self.keyboard.clone();
                let serial = self.next_serial();
                keyboard.input::<(), _>(
                    self,
                    event.key_code(),
                    event.state(),
                    serial,
                    event.time_msec(),
                    move |state, modifiers, key| {
                        let symbol = key
                            .raw_latin_sym_or_raw_current_sym()
                            .map(|symbol| symbol.raw());
                        if symbol
                            .is_some_and(|symbol| state.handle_shortcut(modifiers, symbol, pressed))
                        {
                            FilterResult::Intercept(())
                        } else {
                            FilterResult::Forward
                        }
                    },
                );
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let size = self.desktop.outputs[&self.active_output].logical_size;
                let location = event.position_transformed(
                    (saturating_i32(size.width), saturating_i32(size.height)).into(),
                );
                self.pointer_location = location;
                if self.drag.is_some() {
                    self.update_drag(location);
                } else {
                    let focus = self.surface_under(location);
                    let pointer = self.pointer.clone();
                    let serial = self.next_serial();
                    pointer.motion(
                        self,
                        focus.map(|(surface, origin, _)| (surface, origin)),
                        &MotionEvent {
                            location,
                            serial,
                            time: event.time_msec(),
                        },
                    );
                    pointer.frame(self);
                }
            }
            InputEvent::PointerMotion { event } => {
                let size = self.desktop.outputs[&self.active_output].logical_size;
                let delta = event.delta_unaccel();
                let location = SmithayPoint::from((
                    (self.pointer_location.x + delta.x).clamp(0.0, size.width as f64 - 1.0),
                    (self.pointer_location.y + delta.y).clamp(0.0, size.height as f64 - 1.0),
                ));
                self.pointer_location = location;
                if self.drag.is_some() {
                    self.update_drag(location);
                } else {
                    let focus = self.surface_under(location);
                    let pointer = self.pointer.clone();
                    let serial = self.next_serial();
                    pointer.motion(
                        self,
                        focus.map(|(surface, origin, _)| (surface, origin)),
                        &MotionEvent {
                            location,
                            serial,
                            time: event.time_msec(),
                        },
                    );
                    pointer.frame(self);
                }
            }
            InputEvent::PointerButton { event } => self.handle_pointer_button(
                event.button(),
                event.button_code(),
                event.state(),
                event.time_msec(),
            ),
            _ => {}
        }
    }

    fn next_serial(&mut self) -> Serial {
        let serial = self.serial;
        self.serial = self.serial.wrapping_add(1).max(1);
        serial.into()
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
                candidates.push((
                    (mode_layer(mode), index, 0usize),
                    mapped.surface.wl_surface().clone(),
                    (origin.x as f64, origin.y as f64).into(),
                    Some(mapped.id),
                ));
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
                    candidates.push((
                        (mode_layer(mode), index, popup_index + 1),
                        popup.wl_surface().clone(),
                        (popup_origin.x as f64, popup_origin.y as f64).into(),
                        Some(mapped.id),
                    ));
                }
            }
        }
        for (index, mapped) in self.layers.iter().enumerate() {
            if mapped.output != self.active_output {
                continue;
            }
            let Some((origin, size)) = self.layer_geometry(mapped) else {
                continue;
            };
            let order = layer_rank(mapped.layer);
            if point_inside(location, origin, size, 1.0) {
                candidates.push((
                    (order, index, 0),
                    mapped.surface.wl_surface().clone(),
                    (origin.x as f64, origin.y as f64).into(),
                    None,
                ));
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
                    candidates.push((
                        (order, index, popup_index + 1),
                        popup.wl_surface().clone(),
                        (popup_origin.x as f64, popup_origin.y as f64).into(),
                        None,
                    ));
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
                let zoom = workspace.camera.zoom.max(0.01);
                let left = workspace.camera.center.x as f64
                    - output.logical_size.width as f64 / (2.0 * zoom);
                let top = workspace.camera.center.y as f64
                    - output.logical_size.height as f64 / (2.0 * zoom);
                Some((
                    Point::new(
                        ((rect.origin.x as f64 - left) * zoom).round() as i64,
                        ((rect.origin.y as f64 - top) * zoom).round() as i64,
                    ),
                    rect.size,
                    zoom,
                    mode,
                ))
            }
            WindowMode::Floating => {
                let mut rect = workspace.floating[&id].rect;
                if let Some(drag) = self
                    .drag
                    .filter(|drag| drag.window == id && output_id == self.active_output)
                {
                    rect.origin = drag.target;
                }
                Some((rect.origin, rect.size, 1.0, mode))
            }
            WindowMode::Fullscreen => Some((Point::ORIGIN, output.logical_size, 1.0, mode)),
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

        if state == BackendButtonState::Pressed {
            if let Some((surface, _, window)) = self.surface_under(self.pointer_location) {
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
        }
        let pointer = self.pointer.clone();
        let serial = self.next_serial();
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
        let workspace = &self.desktop.workspaces[&workspace_id];
        let start = match mode {
            WindowMode::Tiled => workspace.tiled[&window].geometry.origin,
            WindowMode::Floating => workspace.floating[&window].rect.origin,
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
        let _ = self.desktop.focus_window(window);
        self.sync_keyboard_focus();
    }

    fn layer_accepts_keyboard(&self, surface: &WlSurface) -> bool {
        self.layers.iter().any(|mapped| {
            if mapped.surface.wl_surface() != surface {
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
            let zoom = workspace.camera.zoom.max(0.01);
            let left =
                workspace.camera.center.x as f64 - output.logical_size.width as f64 / (2.0 * zoom);
            let top =
                workspace.camera.center.y as f64 - output.logical_size.height as f64 / (2.0 * zoom);
            Point::new(
                (left + viewport_x / zoom).round() as i64,
                (top + viewport_y / zoom).round() as i64,
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
        let viewport_size = self.desktop.outputs[&self.active_output].logical_size;
        let transaction = match drag.mode {
            WindowMode::Tiled => WindowTransaction::MoveTiledFinished {
                id: drag.window,
                target: drag.target,
                seed_direction: astera_core::Direction::between(
                    drag.start,
                    drag.target,
                    self.desktop.workspaces[&workspace].focus_direction,
                ),
            },
            WindowMode::Floating => {
                let size = self.desktop.workspaces[&workspace].floating[&drag.window]
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
        }
    }

    fn handle_shortcut(&mut self, modifiers: &ModifiersState, symbol: u32, pressed: bool) -> bool {
        if !modifiers.logo {
            return false;
        }
        let workspace_id = self.desktop.outputs[&self.active_output].current_workspace;
        let focused_window =
            workspace_id.and_then(|workspace| self.desktop.workspaces[&workspace].focused_window);
        let command = if (keysyms::KEY_1..=keysyms::KEY_9).contains(&symbol) {
            let target = WorkspaceId(symbol - keysyms::KEY_1);
            if modifiers.shift {
                focused_window.map(|window| Command::SendWindowToWorkspace { window, target })
            } else {
                Some(Command::BindWorkspace {
                    workspace: target,
                    output: self.active_output,
                })
            }
        } else {
            match symbol {
                keysyms::KEY_space => focused_window.and_then(|window| {
                    let workspace = self.desktop.find_window(window).ok()?;
                    let mode = match self.desktop.workspaces[&workspace].window_mode(window)? {
                        WindowMode::Tiled | WindowMode::Fullscreen => WindowMode::Floating,
                        WindowMode::Floating => WindowMode::Tiled,
                    };
                    Some(Command::SetWindowMode { window, mode })
                }),
                keysyms::KEY_f => focused_window.and_then(|window| {
                    let workspace = self.desktop.find_window(window).ok()?;
                    let state = &self.desktop.workspaces[&workspace];
                    let mode = match state.window_mode(window)? {
                        WindowMode::Fullscreen => match &state.fullscreen.as_ref()?.restore {
                            astera_core::RestorePlacement::Tiled { .. } => WindowMode::Tiled,
                            astera_core::RestorePlacement::Floating { .. } => WindowMode::Floating,
                        },
                        WindowMode::Tiled | WindowMode::Floating => WindowMode::Fullscreen,
                    };
                    Some(Command::SetWindowMode { window, mode })
                }),
                keysyms::KEY_Left | keysyms::KEY_Right | keysyms::KEY_Up | keysyms::KEY_Down => {
                    workspace_id.map(|workspace| {
                        let step = 160;
                        let (dx, dy) = match symbol {
                            keysyms::KEY_Left => (-step, 0),
                            keysyms::KEY_Right => (step, 0),
                            keysyms::KEY_Up => (0, -step),
                            keysyms::KEY_Down => (0, step),
                            _ => unreachable!(),
                        };
                        Command::PanCamera { workspace, dx, dy }
                    })
                }
                _ => None,
            }
        };
        let Some(command) = command else {
            return false;
        };
        if pressed {
            if let Response::Error { code, message } = self.execute_command(command) {
                tracing::warn!(?code, %message, "shortcut command failed");
            }
        }
        true
    }

    fn mapped_windows_for_output(
        &self,
        output: OutputId,
    ) -> impl Iterator<Item = (&ToplevelSurface, SmithayPoint<i32, Physical>, f64)> {
        let mut instances: Vec<_> = self
            .windows
            .iter()
            .filter_map(|mapped| {
                let (origin, _, scale, mode) =
                    self.visual_geometry_for_output(output, mapped.id)?;
                let layer = mode_layer(mode);
                Some((layer, &mapped.surface, origin, scale))
            })
            .collect();
        instances.sort_by_key(|(layer, _, _, _)| std::cmp::Reverse(*layer));
        instances.into_iter().map(|(_, surface, origin, scale)| {
            (
                surface,
                SmithayPoint::from((saturating_i32(origin.x), saturating_i32(origin.y))),
                scale,
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
        let mut roots = Vec::new();
        roots.extend(self.layer_roots(output, Layer::Overlay));
        roots.extend(
            self.mapped_windows_for_output(output)
                .filter(|(surface, _, _)| {
                    self.window_mode_for_surface(output, surface.wl_surface())
                        == Some(WindowMode::Fullscreen)
                })
                .map(|(surface, location, scale)| (surface.wl_surface().clone(), location, scale)),
        );
        roots.extend(self.layer_roots(output, Layer::Top));
        roots.extend(
            self.mapped_windows_for_output(output)
                .filter(|(surface, _, _)| {
                    matches!(
                        self.window_mode_for_surface(output, surface.wl_surface()),
                        Some(WindowMode::Floating | WindowMode::Tiled)
                    )
                })
                .map(|(surface, location, scale)| (surface.wl_surface().clone(), location, scale)),
        );
        roots.extend(self.layer_roots(output, Layer::Bottom));
        roots.extend(self.layer_roots(output, Layer::Background));
        roots
    }

    fn window_mode_for_surface(&self, output: OutputId, surface: &WlSurface) -> Option<WindowMode> {
        let id = self
            .windows
            .iter()
            .find(|mapped| mapped.surface.wl_surface() == surface)?
            .id;
        self.desktop.workspace_for_output(output)?.window_mode(id)
    }

    fn layer_roots(
        &self,
        output: OutputId,
        wanted: Layer,
    ) -> impl Iterator<Item = (WlSurface, SmithayPoint<i32, Physical>, f64)> + '_ {
        self.layers
            .iter()
            .filter(move |mapped| mapped.output == output && mapped.layer == wanted)
            .filter_map(|mapped| {
                let (origin, _) = self.layer_geometry(mapped)?;
                Some((
                    mapped.surface.wl_surface().clone(),
                    (saturating_i32(origin.x), saturating_i32(origin.y)).into(),
                    1.0,
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
            (output.logical_size.width - i64::from(requested.margin.left + requested.margin.right))
                .max(1)
        } else {
            i64::from(requested.size.w)
        };
        let height = if requested.size.h == 0 {
            (output.logical_size.height - i64::from(requested.margin.top + requested.margin.bottom))
                .max(1)
        } else {
            i64::from(requested.size.h)
        };
        let x = if requested.anchor.contains(Anchor::LEFT) {
            i64::from(requested.margin.left)
        } else if requested.anchor.contains(Anchor::RIGHT) {
            output.logical_size.width - width - i64::from(requested.margin.right)
        } else {
            (output.logical_size.width - width) / 2
        };
        let y = if requested.anchor.contains(Anchor::TOP) {
            i64::from(requested.margin.top)
        } else if requested.anchor.contains(Anchor::BOTTOM) {
            output.logical_size.height - height - i64::from(requested.margin.bottom)
        } else {
            (output.logical_size.height - height) / 2
        };
        Some((Point::new(x, y), Size::new(width, height)))
    }

    pub fn update_output_size(&mut self, width: i64, height: i64) {
        let size = Size::new(width, height);
        if self.desktop.outputs[&self.active_output].logical_size != size {
            let current = self.desktop.outputs[&self.active_output].clone();
            if let Err(error) = self.configure_output(
                self.active_output,
                size,
                size,
                current.native_scale,
                current.transform,
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
        self.configure_fullscreen_windows();
        self.configure_layer_surfaces();
        self.refresh_visible_scales();
        Ok(())
    }

    fn active_scale(&self) -> f64 {
        self.output_scale(self.active_output)
    }

    fn output_scale(&self, output: OutputId) -> f64 {
        self.desktop.outputs[&output].native_scale.0 as f64 / 120.0
    }

    fn refresh_visible_scales(&mut self) {
        let scenes: BTreeMap<_, _> = self
            .output_runtime
            .keys()
            .copied()
            .map(|output| {
                let scale = self.output_scale(output);
                let visible = self
                    .render_roots_for_output(output)
                    .into_iter()
                    .map(|(surface, _, _)| surface)
                    .collect::<Vec<_>>();
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
                let _ = self
                    .desktop
                    .apply_window(workspace, WindowTransaction::Remove { id });
            }
        }
        self.refresh_visible_scales();
    }

    pub fn execute_command(&mut self, command: Command) -> Response<DesktopSnapshot> {
        let mode_change = match &command {
            Command::SetWindowMode { window, mode } => Some((*window, *mode)),
            _ => None,
        };
        match self.execute_command_inner(command) {
            Ok(()) => {
                if let Some((window, mode)) = mode_change {
                    self.configure_window_mode(window, mode);
                }
                self.configure_fullscreen_windows();
                self.refresh_visible_scales();
                self.sync_keyboard_focus();
                Response::Ok(
                    DesktopSnapshot::from(&self.desktop)
                        .with_active_output(Some(self.active_output)),
                )
            }
            Err((code, message)) => Response::Error { code, message },
        }
    }

    fn configure_window_mode(&self, window: WindowId, mode: WindowMode) {
        let Ok(workspace_id) = self.desktop.find_window(window) else {
            return;
        };
        let workspace = &self.desktop.workspaces[&workspace_id];
        let Some(mapped) = self.windows.iter().find(|mapped| mapped.id == window) else {
            return;
        };
        let size = if mode == WindowMode::Fullscreen {
            workspace
                .bound_output
                .and_then(|output| self.desktop.outputs.get(&output))
                .map(|output| output.logical_size)
        } else {
            workspace.window_size(window)
        };
        mapped.surface.with_pending_state(|state| {
            state.size =
                size.map(|size| (saturating_i32(size.width), saturating_i32(size.height)).into());
            if mode == WindowMode::Fullscreen {
                state.states.set(xdg_toplevel::State::Fullscreen);
            } else {
                state.states.unset(xdg_toplevel::State::Fullscreen);
            }
        });
        mapped.surface.send_pending_configure();
    }

    fn configure_fullscreen_windows(&self) {
        for workspace in self.desktop.workspaces.values() {
            let (Some(fullscreen), Some(output_id)) =
                (workspace.fullscreen.as_ref(), workspace.bound_output)
            else {
                continue;
            };
            let Some(mapped) = self
                .windows
                .iter()
                .find(|mapped| mapped.id == fullscreen.window)
            else {
                continue;
            };
            let size = self.desktop.outputs[&output_id].logical_size;
            mapped.surface.with_pending_state(|state| {
                state.size = Some((saturating_i32(size.width), saturating_i32(size.height)).into());
                state.states.set(xdg_toplevel::State::Fullscreen);
            });
            mapped.surface.send_pending_configure();
        }
    }

    fn configure_layer_surface(&self, surface: &LayerSurface) {
        let Some(mapped) = self.layers.iter().find(|mapped| mapped.surface == *surface) else {
            return;
        };
        let Some((_, size)) = self.layer_geometry(mapped) else {
            return;
        };
        surface.with_pending_state(|state| {
            state.size = Some((saturating_i32(size.width), saturating_i32(size.height)).into());
        });
        surface.send_pending_configure();
    }

    fn configure_layer_surfaces(&self) {
        for mapped in &self.layers {
            self.configure_layer_surface(&mapped.surface);
        }
    }

    fn sync_keyboard_focus(&mut self) {
        let layer_target = self.layers.iter().rev().find_map(|mapped| {
            let state = with_states(mapped.surface.wl_surface(), |states| {
                *states
                    .cached_state
                    .get::<LayerSurfaceCachedState>()
                    .current()
            });
            (mapped.output == self.active_output
                && mapped.surface.alive()
                && state.keyboard_interactivity == KeyboardInteractivity::Exclusive)
                .then(|| mapped.surface.wl_surface().clone())
        });
        let focused = self
            .desktop
            .workspace_for_output(self.active_output)
            .and_then(|workspace| workspace.focused_window);
        let window_target = focused.and_then(|id| {
            self.windows
                .iter()
                .find(|mapped| mapped.id == id)
                .map(|mapped| mapped.surface.wl_surface().clone())
        });
        let target = layer_target.or(window_target);
        for mapped in &self.windows {
            let activated = Some(mapped.surface.wl_surface()) == target.as_ref();
            mapped.surface.with_pending_state(|state| {
                if activated {
                    state.states.set(xdg_toplevel::State::Activated);
                } else {
                    state.states.unset(xdg_toplevel::State::Activated);
                }
            });
            mapped.surface.send_pending_configure();
        }
        let keyboard = self.keyboard.clone();
        let serial = self.next_serial();
        keyboard.set_focus(self, target, serial);
    }

    fn execute_command_inner(&mut self, command: Command) -> Result<(), (ErrorCode, String)> {
        match command {
            Command::GetState => Ok(()),
            Command::FocusOutput(output) => {
                if !self.desktop.outputs.contains_key(&output) {
                    return Err((ErrorCode::NotFound, format!("unknown output {output:?}")));
                }
                self.active_output = output;
                Ok(())
            }
            Command::BindWorkspace { workspace, output }
            | Command::MoveWorkspaceToOutput { workspace, output } => self
                .desktop
                .apply(WorkspaceTransaction::Bind { workspace, output })
                .map(|_| ())
                .map_err(map_desktop_error),
            Command::SwapWorkspaces { first, second } => self
                .desktop
                .apply(WorkspaceTransaction::Swap { first, second })
                .map(|_| ())
                .map_err(map_desktop_error),
            Command::SendWindowToWorkspace { window, target } => self
                .desktop
                .apply(WorkspaceTransaction::SendWindow { window, target })
                .map(|_| ())
                .map_err(map_desktop_error),
            Command::SetWindowMode { window, mode } => {
                let workspace = self
                    .desktop
                    .find_window(window)
                    .map_err(map_desktop_error)?;
                let viewport_size = self
                    .desktop
                    .workspaces
                    .get(&workspace)
                    .and_then(|workspace| workspace.bound_output)
                    .and_then(|output| self.desktop.outputs.get(&output))
                    .map(|output| output.logical_size)
                    .unwrap_or(Size::new(1920, 1080));
                self.desktop
                    .apply_window(
                        workspace,
                        WindowTransaction::SetMode {
                            id: window,
                            mode,
                            viewport_size,
                        },
                    )
                    .map_err(map_desktop_error)
            }
            Command::SetCameraPolicy { workspace, policy } => {
                let state = self.desktop.workspaces.get_mut(&workspace).ok_or_else(|| {
                    (
                        ErrorCode::NotFound,
                        format!("unknown workspace {workspace:?}"),
                    )
                })?;
                state.camera.policy = policy;
                Ok(())
            }
            Command::PanCamera { workspace, dx, dy } => {
                let state = self.desktop.workspaces.get_mut(&workspace).ok_or_else(|| {
                    (
                        ErrorCode::NotFound,
                        format!("unknown workspace {workspace:?}"),
                    )
                })?;
                state.camera.center.x = state.camera.center.x.saturating_add(dx);
                state.camera.center.y = state.camera.center.y.saturating_add(dy);
                Ok(())
            }
            Command::FocusWindow(window) => {
                let workspace_id = self
                    .desktop
                    .focus_window(window)
                    .map_err(map_desktop_error)?;
                if let Some(output) = self.desktop.workspaces[&workspace_id].bound_output {
                    self.active_output = output;
                }
                Ok(())
            }
            Command::FocusDirection(direction) => {
                let workspace_id = self.desktop.outputs[&self.active_output]
                    .current_workspace
                    .ok_or_else(|| {
                        (
                            ErrorCode::Conflict,
                            "active output has no workspace".to_owned(),
                        )
                    })?;
                self.desktop
                    .workspaces
                    .get_mut(&workspace_id)
                    .unwrap()
                    .focus_direction = direction;
                Ok(())
            }
        }
    }
}

fn map_desktop_error(error: astera_core::DesktopError) -> (ErrorCode, String) {
    let code = match error {
        astera_core::DesktopError::UnknownWorkspace(_)
        | astera_core::DesktopError::UnknownOutput(_)
        | astera_core::DesktopError::UnknownWindow(_) => ErrorCode::NotFound,
        astera_core::DesktopError::DuplicateWindow { .. }
        | astera_core::DesktopError::Layout(_) => ErrorCode::Conflict,
    };
    (code, error.to_string())
}

fn mode_layer(mode: WindowMode) -> u8 {
    match mode {
        WindowMode::Tiled => 2,
        WindowMode::Floating => 3,
        WindowMode::Fullscreen => 5,
    }
}

fn layer_rank(layer: Layer) -> u8 {
    match layer {
        Layer::Background => 0,
        Layer::Bottom => 1,
        Layer::Top => 4,
        Layer::Overlay => 6,
    }
}

fn output_transform(transform: OutputTransform) -> smithay::utils::Transform {
    match transform {
        OutputTransform::Normal => smithay::utils::Transform::Normal,
        OutputTransform::Rotate90 => smithay::utils::Transform::_90,
        OutputTransform::Rotate180 => smithay::utils::Transform::_180,
        OutputTransform::Rotate270 => smithay::utils::Transform::_270,
        OutputTransform::Flipped => smithay::utils::Transform::Flipped,
    }
}

fn point_inside(
    point: SmithayPoint<f64, smithay::utils::Logical>,
    origin: Point,
    size: Size,
    scale: f64,
) -> bool {
    point.x >= origin.x as f64
        && point.x < origin.x as f64 + size.width as f64 * scale
        && point.y >= origin.y as f64
        && point.y < origin.y as f64 + size.height as f64 * scale
}

fn saturating_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

impl BufferHandler for Astera {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl OutputHandler for Astera {}

impl FractionalScaleHandler for Astera {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self.active_scale();
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(scale);
            });
        });
    }
}

impl CompositorHandler for Astera {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("all Astera clients have compositor state")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.popup_manager.commit(surface);
        if let Some(layer) = self
            .layers
            .iter()
            .find(|mapped| mapped.surface.wl_surface() == surface)
            .map(|mapped| mapped.surface.clone())
        {
            self.configure_layer_surface(&layer);
            self.sync_keyboard_focus();
        }
    }
}

impl WlrLayerShellHandler for Astera {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        output: Option<WlOutput>,
        layer: Layer,
        _namespace: String,
    ) {
        let output = output
            .as_ref()
            .and_then(|requested| {
                self.output_runtime
                    .iter()
                    .find_map(|(id, runtime)| runtime.wayland.owns(requested).then_some(*id))
            })
            .unwrap_or(self.active_output);
        self.layers.push(MappedLayer {
            surface: surface.clone(),
            layer,
            output,
        });
        self.configure_layer_surface(&surface);
        self.refresh_visible_scales();
    }

    fn new_popup(&mut self, _parent: LayerSurface, popup: PopupSurface) {
        if let Err(error) = self.popup_manager.track_popup(popup.clone().into()) {
            tracing::warn!(%error, "could not track layer popup");
            return;
        }
        if let Err(error) = popup.send_configure() {
            tracing::warn!(%error, "could not configure layer popup");
        }
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        self.layers.retain(|mapped| mapped.surface != surface);
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
    }
}

impl XdgShellHandler for Astera {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        let workspace_id = self.desktop.outputs[&self.active_output]
            .current_workspace
            .expect("active output has a workspace");
        let workspace = &self.desktop.workspaces[&workspace_id];
        let anchor = workspace
            .focused_window
            .and_then(|focused| workspace.tiled.get(&focused))
            .map(|window| window.geometry.center())
            .unwrap_or(Point::ORIGIN);
        let seed_direction = workspace.focus_direction;
        let result = self.desktop.apply_window(
            workspace_id,
            WindowTransaction::InsertTiled {
                id,
                size: DEFAULT_WINDOW_SIZE,
                anchor,
                seed_direction,
            },
        );
        if let Err(error) = result {
            tracing::error!(?id, %error, "could not place toplevel");
            return;
        }

        surface.with_pending_state(|state| {
            state.size = Some(
                (
                    DEFAULT_WINDOW_SIZE.width as i32,
                    DEFAULT_WINDOW_SIZE.height as i32,
                )
                    .into(),
            );
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
        self.windows.push(MappedWindow { id, surface });
        self.refresh_visible_scales();
        self.sync_keyboard_focus();
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = positioner.get_geometry();
        });
        if let Err(error) = self.popup_manager.track_popup(surface.clone().into()) {
            tracing::warn!(%error, "could not track xdg popup");
            return;
        }
        if let Err(error) = surface.send_configure() {
            tracing::warn!(%error, "could not configure xdg popup");
        }
    }

    fn grab(&mut self, surface: PopupSurface, _seat: wl_seat::WlSeat, serial: Serial) {
        let popup = PopupKind::from(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        let seat = self.seat.clone();
        let Ok(grab) = self
            .popup_manager
            .grab_popup::<Self>(root, popup, &seat, serial)
        else {
            return;
        };
        let keyboard = self.keyboard.clone();
        keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        let pointer = self.pointer.clone();
        pointer.set_grab(
            self,
            PopupPointerGrab::new(&grab),
            serial,
            PointerFocusMode::Keep,
        );
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = positioner.get_geometry();
        });
        surface.send_repositioned(token);
    }
}

impl ShmHandler for Astera {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for Astera {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
    }
}

impl SelectionHandler for Astera {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Astera {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for Astera {}

impl ServerDndGrabHandler for Astera {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

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
mod tests {
    use astera_core::{Scale120, WorkspaceTransaction};
    use smithay::reexports::wayland_server::Display;

    use super::*;

    #[test]
    fn hotplug_keeps_disconnected_workspace_in_background() {
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), Config::default());
        let mut second = Output::new(OutputId(1), "test-output-2", Size::new(2560, 1440));
        second.physical_size = Size::new(3840, 2160);
        second.native_scale = Scale120(180);
        second.transform = OutputTransform::Rotate90;

        state.connect_output(second).unwrap();
        let runtime = &state.output_runtime[&OutputId(1)].wayland;
        assert_eq!(runtime.current_mode().unwrap().size, (3840, 2160).into());
        assert_eq!(runtime.current_scale().fractional_scale(), 1.5);
        assert_eq!(runtime.current_transform(), smithay::utils::Transform::_90);
        state
            .desktop
            .apply(WorkspaceTransaction::Bind {
                workspace: WorkspaceId(1),
                output: OutputId(1),
            })
            .unwrap();
        state.disconnect_output(OutputId(0)).unwrap();

        assert_eq!(state.active_output, OutputId(1));
        assert!(!state.output_runtime.contains_key(&OutputId(0)));
        assert_eq!(state.desktop.workspaces[&WorkspaceId(0)].bound_output, None);
        assert_eq!(
            state.desktop.outputs[&OutputId(1)].current_workspace,
            Some(WorkspaceId(1))
        );
    }

    #[test]
    fn output_reconfigure_preserves_camera_and_updates_protocol_state() {
        let display = Display::<Astera>::new().unwrap();
        let mut state = Astera::new(&display.handle(), Config::default());
        state
            .desktop
            .workspaces
            .get_mut(&WorkspaceId(0))
            .unwrap()
            .camera
            .center = Point::new(740, -320);

        state
            .configure_output(
                OutputId(0),
                Size::new(3000, 2000),
                Size::new(2000, 1333),
                Scale120(180),
                OutputTransform::Rotate180,
            )
            .unwrap();

        let output = &state.desktop.outputs[&OutputId(0)];
        assert_eq!(output.physical_size, Size::new(3000, 2000));
        assert_eq!(output.logical_size, Size::new(2000, 1333));
        assert_eq!(
            state.desktop.workspaces[&WorkspaceId(0)].camera.center,
            Point::new(740, -320)
        );
        let runtime = &state.output_runtime[&OutputId(0)].wayland;
        assert_eq!(runtime.current_mode().unwrap().size, (3000, 2000).into());
        assert_eq!(runtime.current_scale().fractional_scale(), 1.5);
        assert_eq!(runtime.current_transform(), smithay::utils::Transform::_180);
    }
}
