use std::os::fd::OwnedFd;

use astera_config::Config;
use astera_core::{
    Desktop, Output, OutputId, Point, Size, WindowId, WindowMode, WindowTransaction, Workspace,
    WorkspaceId, WorkspaceTransaction,
};
use astera_ipc::{Command, DesktopSnapshot, ErrorCode, Response};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, ButtonState as BackendButtonState, Event, InputEvent, KeyState,
            KeyboardKeyEvent, MouseButton, PointerButtonEvent,
        },
        renderer::utils::on_commit_buffer_handler,
        winit::WinitInput,
    },
    delegate_compositor, delegate_data_device, delegate_seat, delegate_shm, delegate_xdg_shell,
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, ModifiersState, keysyms},
        pointer::{ButtonEvent, MotionEvent, PointerHandle},
    },
    reexports::wayland_server::{
        Client, DisplayHandle,
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
    },
    utils::{Physical, Point as SmithayPoint, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
            TraversalAction, with_surface_tree_downward,
        },
        selection::{
            SelectionHandler,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
        shm::{ShmHandler, ShmState},
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

pub struct Astera {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    _seat: Seat<Self>,
    keyboard: smithay::input::keyboard::KeyboardHandle<Self>,
    pointer: PointerHandle<Self>,
    desktop: Desktop,
    active_output: OutputId,
    windows: Vec<MappedWindow>,
    next_window_id: u64,
    pointer_location: SmithayPoint<f64, smithay::utils::Logical>,
    drag: Option<DragState>,
    serial: u32,
}

impl Astera {
    pub fn new(display: &DisplayHandle, config: Config) -> Self {
        let compositor_state = CompositorState::new::<Self>(display);
        let xdg_shell_state = XdgShellState::new::<Self>(display);
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
            compositor_state,
            xdg_shell_state,
            shm_state,
            seat_state,
            data_device_state,
            _seat: seat,
            keyboard,
            pointer,
            desktop,
            active_output,
            windows: Vec::new(),
            next_window_id: 1,
            pointer_location: (0.0, 0.0).into(),
            drag: None,
            serial: 1,
        }
    }

    pub fn process_input(&mut self, event: InputEvent<WinitInput>) {
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
        WindowId,
    )> {
        self.windows
            .iter()
            .enumerate()
            .filter_map(|(index, mapped)| {
                let (origin, size, scale, mode) = self.visual_geometry(mapped.id)?;
                let width = size.width as f64 * scale;
                let height = size.height as f64 * scale;
                let inside = location.x >= origin.x as f64
                    && location.x < origin.x as f64 + width
                    && location.y >= origin.y as f64
                    && location.y < origin.y as f64 + height;
                inside.then_some((
                    (mode_layer(mode), index),
                    mapped.surface.wl_surface().clone(),
                    (origin.x as f64, origin.y as f64).into(),
                    mapped.id,
                ))
            })
            .max_by_key(|(order, _, _, _)| *order)
            .map(|(_, surface, origin, id)| (surface, origin, id))
    }

    fn visual_geometry(&self, id: WindowId) -> Option<(Point, Size, f64, WindowMode)> {
        let output = self.desktop.outputs.get(&self.active_output)?;
        let workspace = self.desktop.workspace_for_output(self.active_output)?;
        let mode = workspace.window_mode(id)?;
        match mode {
            WindowMode::Tiled => {
                let mut rect = workspace.tiled[&id].geometry;
                if let Some(drag) = self.drag.filter(|drag| drag.window == id) {
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
                if let Some(drag) = self.drag.filter(|drag| drag.window == id) {
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
            if let Some((_surface, _, window)) = self.surface_under(self.pointer_location) {
                if self.desktop.find_window(window).is_ok() {
                    let _ = self.desktop.focus_window(window);
                }
                self.sync_keyboard_focus();
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

    pub fn mapped_windows(
        &self,
    ) -> impl Iterator<Item = (&ToplevelSurface, SmithayPoint<i32, Physical>, f64)> {
        let mut instances: Vec<_> = self
            .windows
            .iter()
            .filter_map(|mapped| {
                let (origin, _, scale, mode) = self.visual_geometry(mapped.id)?;
                let layer = mode_layer(mode);
                Some((layer, &mapped.surface, origin, scale))
            })
            .collect();
        instances.sort_by_key(|(layer, _, _, _)| *layer);
        instances.into_iter().map(|(_, surface, origin, scale)| {
            (
                surface,
                SmithayPoint::from((saturating_i32(origin.x), saturating_i32(origin.y))),
                scale,
            )
        })
    }

    pub fn update_output_size(&mut self, width: i64, height: i64) {
        let size = Size::new(width, height);
        if self.desktop.outputs[&self.active_output].logical_size != size {
            let _ = self.desktop.resize_output(self.active_output, size);
            self.configure_fullscreen_windows();
        }
    }

    pub fn remove_dead_windows(&mut self) {
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

    fn sync_keyboard_focus(&mut self) {
        let focused = self
            .desktop
            .workspace_for_output(self.active_output)
            .and_then(|workspace| workspace.focused_window);
        let target = focused.and_then(|id| {
            self.windows
                .iter()
                .find(|mapped| mapped.id == id)
                .map(|mapped| mapped.surface.wl_surface().clone())
        });
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
        WindowMode::Tiled => 0,
        WindowMode::Floating => 1,
        WindowMode::Fullscreen => 2,
    }
}

fn saturating_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

impl BufferHandler for Astera {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
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
        self.sync_keyboard_focus();
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
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
delegate_compositor!(Astera);
delegate_shm!(Astera);
delegate_seat!(Astera);
delegate_data_device!(Astera);
