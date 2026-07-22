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
    pub fn execute_command(&mut self, command: Command) -> Response<DesktopSnapshot> {
        match &command {
            Command::GetState => tracing::debug!("IPC state snapshot requested"),
            command => tracing::info!(?command, "command started"),
        }
        // Protocol configure is emitted only after the core transaction commits successfully.
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
                    DesktopSnapshot::from(&self.desktop).with_active_output(
                        self.desktop
                            .outputs
                            .contains_key(&self.active_output)
                            .then_some(self.active_output),
                    ),
                )
            }
            Err(error) => {
                tracing::warn!(code = ?error.code, %error, "command failed");
                Response::Error {
                    code: error.code,
                    message: error.to_string(),
                }
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
        let size = if mode == WindowMode::Fullscreen {
            self.desktop
                .workspace_location(workspace_id)
                .ok()
                .and_then(|location| location.output)
                .and_then(|output| self.desktop.outputs.get(&output))
                .map(|output| output.output.logical_size)
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

    pub(super) fn configure_fullscreen_windows(&self) {
        for workspace in self.desktop.workspaces() {
            let output = self
                .desktop
                .workspace_location(workspace.id)
                .ok()
                .and_then(|location| location.output);
            let (Some(fullscreen), Some(output_id)) = (workspace.fullscreen.as_ref(), output)
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
            let size = self.desktop.outputs[&output_id].output.logical_size;
            mapped.surface.with_pending_state(|state| {
                state.size = Some((saturating_i32(size.width), saturating_i32(size.height)).into());
                state.states.set(xdg_toplevel::State::Fullscreen);
            });
            mapped.surface.send_pending_configure();
        }
    }

    pub(super) fn configure_layer_surface(&self, surface: &LayerSurface) {
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

    pub(super) fn configure_layer_surfaces(&self) {
        for mapped in &self.layers {
            self.configure_layer_surface(&mapped.surface);
        }
    }

    pub(super) fn sync_keyboard_focus(&mut self) {
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

    fn resolve_output(&self, selector: OutputSelector) -> Result<OutputId, CommandError> {
        let output = match selector {
            OutputSelector::Id(output) => Some(output),
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
                .workspace(workspace)
                .ok()
                .map(|workspace| workspace.id),
            WorkspaceSelector::Name(name) => self
                .desktop
                .workspace_by_name(&name)
                .map(|workspace| workspace.id),
            WorkspaceSelector::LocalIndex { output, index } => {
                let output = self.resolve_output(output)?;
                self.desktop
                    .workspace_by_local_index(output, index)
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
                physical_size,
                logical_size,
                native_scale,
                transform,
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
                        workspace,
                        target_output,
                        target_index,
                        activate,
                    })
                    .map(|_| ())
                    .map_err(map_desktop_error)
            }
            Command::SetWorkspaceName { workspace, name } => self
                .desktop
                .apply(WorkspaceTransaction::SetName { workspace, name })
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
                        window,
                        target,
                        activate,
                    })
                    .map(|_| ())
                    .map_err(map_desktop_error)
            }
            Command::SetWindowMode { window, mode } => self.set_window_mode_command(window, mode),
            Command::SetCameraPolicy { workspace, policy } => {
                let state = self
                    .desktop
                    .workspace_mut(workspace)
                    .map_err(map_desktop_error)?;
                state.camera.policy = policy;
                Ok(())
            }
            Command::PanCamera { workspace, dx, dy } => {
                let state = self
                    .desktop
                    .workspace_mut(workspace)
                    .map_err(map_desktop_error)?;
                state.camera.center.x = state.camera.center.x.saturating_add(dx);
                state.camera.center.y = state.camera.center.y.saturating_add(dy);
                Ok(())
            }
            Command::FocusWindow(window) => self.focus_window_command(window),
            Command::FocusDirection(direction) => self.focus_direction_command(direction),
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
                "the native backend does not support live KMS output reconfiguration",
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
        | astera_core::DesktopError::InvalidOutputScale => ErrorCode::InvalidCommand,
    };
    CommandError::new(code, error.to_string())
}
