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
        input::{InputEvent, KeyboardKeyEvent},
        renderer::utils::on_commit_buffer_handler,
        winit::WinitInput,
    },
    delegate_compositor, delegate_data_device, delegate_seat, delegate_shm, delegate_xdg_shell,
    input::{Seat, SeatHandler, SeatState, keyboard::FilterResult},
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

pub struct Astera {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    _seat: Seat<Self>,
    keyboard: smithay::input::keyboard::KeyboardHandle<Self>,
    desktop: Desktop,
    active_output: OutputId,
    windows: Vec<MappedWindow>,
    next_window_id: u64,
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
            desktop,
            active_output,
            windows: Vec::new(),
            next_window_id: 1,
        }
    }

    pub fn process_input(&mut self, event: InputEvent<WinitInput>) {
        if let InputEvent::Keyboard { event } = event {
            let keyboard = self.keyboard.clone();
            keyboard.input::<(), _>(
                self,
                event.key_code(),
                event.state(),
                0.into(),
                0,
                |_, _, _| FilterResult::Forward,
            );
        }
    }

    pub fn mapped_windows(
        &self,
    ) -> impl Iterator<Item = (&ToplevelSurface, SmithayPoint<i32, Physical>, f64)> {
        let output = self.desktop.outputs.get(&self.active_output);
        let workspace = self.desktop.workspace_for_output(self.active_output);
        let mut instances: Vec<_> = self
            .windows
            .iter()
            .filter_map(move |mapped| {
                let output = output?;
                let workspace = workspace?;
                let mode = workspace.window_mode(mapped.id)?;
                let (origin, scale) = match mode {
                    WindowMode::Tiled => {
                        let rect = workspace.tiled[&mapped.id].geometry;
                        let zoom = workspace.camera.zoom.max(0.01);
                        let left = workspace.camera.center.x as f64
                            - output.logical_size.width as f64 / (2.0 * zoom);
                        let top = workspace.camera.center.y as f64
                            - output.logical_size.height as f64 / (2.0 * zoom);
                        (
                            Point::new(
                                ((rect.origin.x as f64 - left) * zoom).round() as i64,
                                ((rect.origin.y as f64 - top) * zoom).round() as i64,
                            ),
                            zoom,
                        )
                    }
                    WindowMode::Floating => (workspace.floating[&mapped.id].rect.origin, 1.0),
                    WindowMode::Fullscreen => (Point::ORIGIN, 1.0),
                };
                let layer = match mode {
                    WindowMode::Tiled => 0,
                    WindowMode::Floating => 1,
                    WindowMode::Fullscreen => 2,
                };
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
                    .find_window(window)
                    .map_err(map_desktop_error)?;
                let workspace = self.desktop.workspaces.get_mut(&workspace_id).unwrap();
                workspace.focus(window);
                if let Some(output) = workspace.bound_output {
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
        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, Some(surface.wl_surface().clone()), 0.into());
        self.windows.push(MappedWindow { id, surface });
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
