use super::*;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{message}")]
struct CommandError {
    // `code` is stable IPC behavior; `message` remains diagnostic and may evolve.
    code: ErrorCode,
    message: String,
}

impl CommandError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl Astera {
    pub fn execute_command(&mut self, command: Command) -> Response {
        self.execute_command_at(command, 0)
    }

    pub fn execute_command_at(&mut self, command: Command, sequence: u64) -> Response {
        let returns_state = matches!(&command, Command::GetState);
        match &command {
            Command::GetState => tracing::debug!("IPC state snapshot requested"),
            command => tracing::info!(kind = command_kind(command), "IPC command started"),
        }
        // Protocol configure is emitted only after the core transaction commits successfully.
        let mode_change = match &command {
            Command::SetWindowMode { window, mode } => Some((*window, *mode)),
            _ => None,
        };
        match self.execute_command_inner(command) {
            Ok(()) => {
                if !returns_state {
                    // Touch focus is fixed for a contact. End it before a successful command can
                    // move its target to another workspace/output or behind a changed scene.
                    self.cancel_touch_sequences();
                    // The diff remains authoritative, so successful semantic no-ops still produce
                    // no event even though they request one snapshot comparison.
                    self.mark_public_dirty();
                }
                if let Some((window, mode)) = mode_change {
                    self.configure_window_mode(window.into(), mode.into());
                }
                self.configure_fullscreen_windows();
                self.refresh_visible_scales();
                self.sync_keyboard_focus();
                if returns_state {
                    Response::Success(Success::State {
                        sequence,
                        snapshot: self.public_snapshot(),
                    })
                } else {
                    Response::Success(Success::Handled { sequence })
                }
            }
            Err(error) => {
                tracing::warn!(code = ?error.code, %error, "command failed");
                Response::Error(astera_ipc::Error {
                    code: error.code,
                    message: error.to_string(),
                    sequence,
                })
            }
        }
    }

    pub(super) fn configure_window_mode(&self, window: WindowId, mode: WindowMode) {
        let Ok(workspace_id) = self.desktop.find_window(window) else {
            return;
        };
        let workspace = self.desktop.workspace(workspace_id).unwrap();
        let Some(mapped) = self.windows.iter().find(|mapped| mapped.id == window) else {
            return;
        };
        let size = match mode {
            WindowMode::Fullscreen => self
                .desktop
                .workspace_location(workspace_id)
                .ok()
                .and_then(|location| location.output)
                .and_then(|output| self.desktop.output(output))
                .map(|output| output.logical_size),
            WindowMode::Maximized => self
                .desktop
                .workspace_location(workspace_id)
                .ok()
                .and_then(|location| location.output)
                .and_then(|output| self.usable_rect(output))
                .map(|rect| rect.size),
            WindowMode::Tiled | WindowMode::Floating => workspace.window_size(window),
        };
        mapped.surface.with_pending_state(|state| {
            state.size =
                size.map(|size| (saturating_i32(size.width), saturating_i32(size.height)).into());
            if mode == WindowMode::Fullscreen {
                state.states.set(xdg_toplevel::State::Fullscreen);
            } else {
                state.states.unset(xdg_toplevel::State::Fullscreen);
            }
            if mode == WindowMode::Maximized {
                state.states.set(xdg_toplevel::State::Maximized);
            } else {
                state.states.unset(xdg_toplevel::State::Maximized);
            }
        });
        mapped.surface.send_pending_configure();
    }

    pub(super) fn configure_fullscreen_windows(&self) {
        for workspace in self.desktop.workspaces() {
            let Some(output_id) = self
                .desktop
                .workspace_location(workspace.id)
                .ok()
                .and_then(|location| location.output)
            else {
                continue;
            };
            if let Some(maximized) = workspace.maximized.as_ref()
                && let Some(mapped) = self
                    .windows
                    .iter()
                    .find(|mapped| mapped.id == maximized.window)
            {
                let size = self.usable_rect(output_id).unwrap().size;
                mapped.surface.with_pending_state(|state| {
                    state.size =
                        Some((saturating_i32(size.width), saturating_i32(size.height)).into());
                    state.states.unset(xdg_toplevel::State::Fullscreen);
                    state.states.set(xdg_toplevel::State::Maximized);
                });
                mapped.surface.send_pending_configure();
            }
            let Some(fullscreen) = workspace.fullscreen.as_ref() else {
                continue;
            };
            let Some(mapped) = self
                .windows
                .iter()
                .find(|mapped| mapped.id == fullscreen.window)
            else {
                continue;
            };
            let size = self.desktop.outputs[&output_id].output.logical_size;
            mapped.surface.with_pending_state(|state| {
                state.size = Some((saturating_i32(size.width), saturating_i32(size.height)).into());
                state.states.set(xdg_toplevel::State::Fullscreen);
                state.states.unset(xdg_toplevel::State::Maximized);
            });
            mapped.surface.send_pending_configure();
        }
    }

    pub(super) fn configure_layer_surface(&self, surface: &smithay::desktop::LayerSurface) {
        let Some(mapped) = self.layers.iter().find(|mapped| mapped.surface == *surface) else {
            return;
        };
        let Some((_, size)) = self.layer_geometry(mapped) else {
            return;
        };
        surface.layer_surface().with_pending_state(|state| {
            state.size = Some((saturating_i32(size.width), saturating_i32(size.height)).into());
        });
        surface.layer_surface().send_pending_configure();
    }

    pub(super) fn configure_layer_surfaces(&self) {
        for mapped in &self.layers {
            self.configure_layer_surface(&mapped.surface);
        }
    }

    pub(super) fn sync_keyboard_focus(&mut self) {
        let lock_target = self
            .lock_surface_for_output(self.active_output)
            .map(|surface| surface.wl_surface().clone());
        let layer_target = self
            .layers
            .iter()
            .enumerate()
            .filter(|(_, mapped)| {
                let state = with_states(mapped.surface.wl_surface(), |states| {
                    *states
                        .cached_state
                        .get::<LayerSurfaceCachedState>()
                        .current()
                });
                mapped.mapped
                    && mapped.output == self.active_output
                    && mapped.surface.alive()
                    && state.keyboard_interactivity == KeyboardInteractivity::Exclusive
            })
            .max_by_key(|(index, mapped)| (layer_rank(mapped.layer), *index))
            .map(|(_, mapped)| mapped.surface.wl_surface().clone());
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
        let target = if self.session_is_locked() {
            lock_target
        } else {
            layer_target.or(window_target)
        };
        for mapped in &mut self.windows {
            let activated = Some(mapped.surface.wl_surface()) == target.as_ref();
            if activated {
                mapped.urgent = false;
            }
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
        keyboard.set_focus(self, target.clone(), serial);
        // Smithay's keyboard callback is not invoked when focus becomes None.
        let seat = self.seat.clone();
        self.update_shortcut_inhibitor(&seat, target.as_ref());
    }

    fn resolve_output(&self, selector: OutputSelector) -> Result<OutputId, CommandError> {
        let output = match selector {
            OutputSelector::Id(output) => Some(output.into()),
            OutputSelector::Key(key) => self
                .desktop
                .outputs
                .iter()
                .find_map(|(id, set)| (set.output.stable_key == key).then_some(*id)),
            OutputSelector::Active => Some(self.active_output),
        };
        output
            .filter(|output| self.desktop.outputs.contains_key(output))
            .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "unknown output"))
    }

    fn resolve_workspace(&self, selector: WorkspaceSelector) -> Result<WorkspaceId, CommandError> {
        let workspace = match selector {
            WorkspaceSelector::Id(workspace) => self
                .desktop
                .workspace(workspace.into())
                .ok()
                .map(|workspace| workspace.id),
            WorkspaceSelector::Name(name) => self
                .desktop
                .workspace_by_name(&name)
                .map(|workspace| workspace.id),
            WorkspaceSelector::LocalIndex { output, index } => {
                let output = self.resolve_output(output)?;
                self.desktop
                    .workspace_by_local_index(output, index as usize)
                    .map(|workspace| workspace.id)
            }
        };
        workspace.ok_or_else(|| CommandError::new(ErrorCode::NotFound, "unknown workspace"))
    }

    fn execute_command_inner(&mut self, command: Command) -> Result<(), CommandError> {
        match command {
            Command::GetState => Ok(()),
            Command::FocusOutput(output) => {
                let output = self.resolve_output(output)?;
                self.active_output = output;
                Ok(())
            }
            Command::ConfigureOutput {
                output,
                physical_size,
                logical_size,
                native_scale,
                transform,
            } => self.configure_output_command(
                output,
                physical_size.into(),
                logical_size.into(),
                native_scale.into(),
                transform.into(),
            ),
            Command::FocusWorkspace { workspace } => self.focus_workspace_command(workspace),
            Command::MoveWorkspace {
                workspace,
                target_output,
                target_index,
                activate,
            } => {
                let target_output = self.resolve_output(target_output)?;
                self.desktop
                    .apply(WorkspaceTransaction::Move {
                        workspace: workspace.into(),
                        target_output,
                        target_index: target_index.map(|index| index as usize),
                        activate,
                    })
                    .map(|_| ())
                    .map_err(map_desktop_error)
            }
            Command::SetWorkspaceName { workspace, name } => self
                .desktop
                .apply(WorkspaceTransaction::SetName {
                    workspace: workspace.into(),
                    name,
                })
                .map(|_| ())
                .map_err(map_desktop_error),
            Command::MoveWindow {
                window,
                target,
                activate,
            } => {
                let target = self.resolve_workspace(target)?;
                self.desktop
                    .apply(WorkspaceTransaction::SendWindow {
                        window: window.into(),
                        target,
                        activate,
                    })
                    .map(|_| ())
                    .map_err(map_desktop_error)
            }
            Command::SetWindowMode { window, mode } => {
                self.set_window_mode_command(window.into(), mode.into())
            }
            Command::CloseWindow(window) => {
                let window: WindowId = window.into();
                let mapped = self
                    .windows
                    .iter()
                    .find(|mapped| mapped.id == window)
                    .ok_or_else(|| CommandError::new(ErrorCode::NotFound, "unknown window"))?;
                mapped.surface.send_close();
                Ok(())
            }
            Command::SetCameraPolicy { workspace, policy } => {
                let state = self
                    .desktop
                    .workspace_mut(workspace.into())
                    .map_err(map_desktop_error)?;
                state.camera.policy = policy.into();
                Ok(())
            }
            Command::PanCamera { workspace, dx, dy } => {
                let state = self
                    .desktop
                    .workspace_mut(workspace.into())
                    .map_err(map_desktop_error)?;
                state.camera.center.x = state.camera.center.x.saturating_add(dx);
                state.camera.center.y = state.camera.center.y.saturating_add(dy);
                Ok(())
            }
            Command::FocusWindow(window) => self.focus_window_command(window.into()),
            Command::FocusDirection(direction) => self.focus_direction_command(direction.into()),
        }
    }

    fn configure_output_command(
        &mut self,
        selector: OutputSelector,
        physical_size: Size,
        logical_size: Size,
        native_scale: astera_core::Scale120,
        transform: OutputTransform,
    ) -> Result<(), CommandError> {
        if !self.output_configuration_supported {
            return Err(CommandError::new(
                ErrorCode::Conflict,
                "this backend does not support live output reconfiguration",
            ));
        }
        let output = self.resolve_output(selector)?;
        self.configure_output(output, physical_size, logical_size, native_scale, transform)
            .map_err(map_desktop_error)
    }

    fn focus_workspace_command(&mut self, selector: WorkspaceSelector) -> Result<(), CommandError> {
        let workspace = self.resolve_workspace(selector)?;
        let output = self
            .desktop
            .workspace_location(workspace)
            .map_err(map_desktop_error)?
            .output
            .ok_or_else(|| CommandError::new(ErrorCode::Conflict, "workspace is detached"))?;
        self.desktop
            .apply(WorkspaceTransaction::Focus { output, workspace })
            .map_err(map_desktop_error)?;
        self.active_output = output;
        Ok(())
    }

    fn set_window_mode_command(
        &mut self,
        window: WindowId,
        mode: WindowMode,
    ) -> Result<(), CommandError> {
        let workspace = self
            .desktop
            .find_window(window)
            .map_err(map_desktop_error)?;
        let viewport_size = self
            .desktop
            .workspace_location(workspace)
            .ok()
            .and_then(|location| location.output)
            .and_then(|output| self.desktop.output(output))
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

    fn focus_window_command(&mut self, window: WindowId) -> Result<(), CommandError> {
        let workspace = self
            .desktop
            .focus_window(window)
            .map_err(map_desktop_error)?;
        if let Some(output) = self
            .desktop
            .workspace_location(workspace)
            .map_err(map_desktop_error)?
            .output
        {
            self.active_output = output;
        }
        Ok(())
    }

    fn focus_direction_command(
        &mut self,
        direction: astera_core::Direction,
    ) -> Result<(), CommandError> {
        let workspace = self
            .desktop
            .active_workspace_id(self.active_output)
            .ok_or_else(|| {
                CommandError::new(ErrorCode::Conflict, "active output has no workspace")
            })?;
        self.desktop
            .focus_direction(workspace, direction)
            .map_err(map_desktop_error)?;
        Ok(())
    }
}

fn command_kind(command: &Command) -> &'static str {
    match command {
        Command::GetState => "get-state",
        Command::FocusWindow(_) => "focus-window",
        Command::FocusDirection(_) => "focus-direction",
        Command::FocusOutput(_) => "focus-output",
        Command::ConfigureOutput { .. } => "configure-output",
        Command::FocusWorkspace { .. } => "focus-workspace",
        Command::MoveWorkspace { .. } => "move-workspace",
        Command::SetWorkspaceName { .. } => "set-workspace-name",
        Command::MoveWindow { .. } => "move-window",
        Command::SetWindowMode { .. } => "set-window-mode",
        Command::CloseWindow(_) => "close-window",
        Command::SetCameraPolicy { .. } => "set-camera-policy",
        Command::PanCamera { .. } => "pan-camera",
    }
}

fn map_desktop_error(error: astera_core::DesktopError) -> CommandError {
    let code = match error {
        astera_core::DesktopError::UnknownWorkspace(_)
        | astera_core::DesktopError::UnknownOutput(_)
        | astera_core::DesktopError::UnknownWindow(_) => ErrorCode::NotFound,
        astera_core::DesktopError::DuplicateWindow { .. }
        | astera_core::DesktopError::DuplicateWorkspaceName(_)
        | astera_core::DesktopError::DuplicateOutputStableKey(_)
        | astera_core::DesktopError::InvalidWorkspaceState(_)
        | astera_core::DesktopError::Layout(_) => ErrorCode::Conflict,
        astera_core::DesktopError::InvalidOutputSize
        | astera_core::DesktopError::InvalidOutputScale => ErrorCode::InvalidRequest,
    };
    CommandError::new(code, error.to_string())
}
