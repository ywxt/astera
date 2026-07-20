use std::collections::BTreeMap;

use thiserror::Error;

use crate::{
    FloatingPlacement, LayoutError, Output, OutputId, OutputTransform, Point, RadialSolver, Rect,
    RestorePlacement, Scale120, Size, WindowId, WindowMode, WindowTransaction, Workspace,
    WorkspaceId,
};

#[derive(Clone, Debug)]
pub enum WorkspaceTransaction {
    Bind {
        workspace: WorkspaceId,
        output: OutputId,
    },
    Swap {
        first: WorkspaceId,
        second: WorkspaceId,
    },
    Unbind {
        workspace: WorkspaceId,
    },
    SendWindow {
        window: WindowId,
        target: WorkspaceId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesktopEvent {
    WorkspaceBound {
        workspace: WorkspaceId,
        output: OutputId,
    },
    WorkspaceUnbound {
        workspace: WorkspaceId,
    },
    OutputDisconnected {
        output: OutputId,
    },
    WorkspacesSwapped {
        first: WorkspaceId,
        second: WorkspaceId,
    },
    WindowSent {
        window: WindowId,
        source: WorkspaceId,
        target: WorkspaceId,
    },
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DesktopError {
    #[error("workspace {0:?} does not exist")]
    UnknownWorkspace(WorkspaceId),
    #[error("output {0:?} does not exist")]
    UnknownOutput(OutputId),
    #[error("window {0:?} does not exist")]
    UnknownWindow(WindowId),
    #[error("window {window:?} exists in multiple workspaces {first:?} and {second:?}")]
    DuplicateWindow {
        window: WindowId,
        first: WorkspaceId,
        second: WorkspaceId,
    },
    #[error(transparent)]
    Layout(#[from] LayoutError),
}

#[derive(Clone, Debug)]
pub struct Desktop {
    pub workspaces: BTreeMap<WorkspaceId, Workspace>,
    pub outputs: BTreeMap<OutputId, Output>,
    solver: RadialSolver,
}

impl Desktop {
    pub fn new(gap: i64) -> Self {
        Self {
            workspaces: BTreeMap::new(),
            outputs: BTreeMap::new(),
            solver: RadialSolver::new(gap),
        }
    }

    pub fn add_workspace(&mut self, workspace: Workspace) -> Result<(), DesktopError> {
        if let Some(output) = workspace.bound_output {
            if !self.outputs.contains_key(&output) {
                return Err(DesktopError::UnknownOutput(output));
            }
        }
        let mut working = self.clone();
        working.workspaces.insert(workspace.id, workspace);
        working.validate()?;
        *self = working;
        Ok(())
    }

    pub fn connect_output(&mut self, output: Output) -> Result<(), DesktopError> {
        let mut working = self.clone();
        working.outputs.insert(output.id, output);
        working.validate()?;
        *self = working;
        Ok(())
    }

    /// Removes an output and leaves its workspace intact in the background.
    pub fn disconnect_output(&mut self, output: OutputId) -> Result<DesktopEvent, DesktopError> {
        let mut working = self.clone();
        let removed = working
            .outputs
            .remove(&output)
            .ok_or(DesktopError::UnknownOutput(output))?;
        let event = if let Some(workspace) = removed.current_workspace {
            working
                .workspaces
                .get_mut(&workspace)
                .ok_or(DesktopError::UnknownWorkspace(workspace))?
                .bound_output = None;
            DesktopEvent::WorkspaceUnbound { workspace }
        } else {
            DesktopEvent::OutputDisconnected { output }
        };
        working.validate()?;
        *self = working;
        Ok(event)
    }

    pub fn apply(
        &mut self,
        transaction: WorkspaceTransaction,
    ) -> Result<DesktopEvent, DesktopError> {
        let mut working = self.clone();
        let event = working.apply_working(transaction)?;
        working.validate()?;
        *self = working;
        Ok(event)
    }

    pub fn apply_window(
        &mut self,
        workspace: WorkspaceId,
        transaction: WindowTransaction,
    ) -> Result<(), DesktopError> {
        let mut working = self.clone();
        let viewport_size = working
            .workspaces
            .get(&workspace)
            .and_then(|workspace| workspace.bound_output)
            .and_then(|output| working.outputs.get(&output))
            .map(|output| output.logical_size);
        let workspace = working
            .workspaces
            .get_mut(&workspace)
            .ok_or(DesktopError::UnknownWorkspace(workspace))?;
        working.solver.apply(workspace, transaction)?;
        if let Some(viewport_size) = viewport_size {
            workspace.follow_focus(viewport_size);
        }
        working.validate()?;
        *self = working;
        Ok(())
    }

    pub fn workspace_for_output(&self, output: OutputId) -> Option<&Workspace> {
        let workspace = self.outputs.get(&output)?.current_workspace?;
        self.workspaces.get(&workspace)
    }

    pub fn resize_output(
        &mut self,
        output: OutputId,
        logical_size: Size,
    ) -> Result<(), DesktopError> {
        let mut working = self.clone();
        let output_state = working
            .outputs
            .get_mut(&output)
            .ok_or(DesktopError::UnknownOutput(output))?;
        let old_size = output_state.logical_size;
        output_state.logical_size = logical_size;
        output_state.physical_size = logical_size;
        if let Some(workspace) = output_state.current_workspace {
            remap_floating(
                working.workspaces.get_mut(&workspace).unwrap(),
                Some(old_size),
                logical_size,
            );
        }
        working.validate()?;
        *self = working;
        Ok(())
    }

    pub fn configure_output(
        &mut self,
        output: OutputId,
        physical_size: Size,
        logical_size: Size,
        native_scale: Scale120,
        transform: OutputTransform,
    ) -> Result<(), DesktopError> {
        let mut working = self.clone();
        let output_state = working
            .outputs
            .get_mut(&output)
            .ok_or(DesktopError::UnknownOutput(output))?;
        let old_size = output_state.logical_size;
        let workspace = output_state.current_workspace;
        output_state.physical_size = physical_size;
        output_state.logical_size = logical_size;
        output_state.native_scale = native_scale;
        output_state.transform = transform;
        if let Some(workspace) = workspace {
            remap_floating(
                working.workspaces.get_mut(&workspace).unwrap(),
                Some(old_size),
                logical_size,
            );
        }
        working.validate()?;
        *self = working;
        Ok(())
    }

    pub fn find_window(&self, window: WindowId) -> Result<WorkspaceId, DesktopError> {
        let mut found = None;
        for workspace in self.workspaces.values() {
            if workspace.contains_window(window) {
                if let Some(first) = found {
                    return Err(DesktopError::DuplicateWindow {
                        window,
                        first,
                        second: workspace.id,
                    });
                }
                found = Some(workspace.id);
            }
        }
        found.ok_or(DesktopError::UnknownWindow(window))
    }

    pub fn focus_window(&mut self, window: WindowId) -> Result<WorkspaceId, DesktopError> {
        let mut working = self.clone();
        let workspace_id = working.find_window(window)?;
        let viewport_size = working.workspace_viewport_size(workspace_id);
        let workspace = working.workspaces.get_mut(&workspace_id).unwrap();
        workspace.focus(window);
        if let Some(viewport_size) = viewport_size {
            workspace.follow_focus(viewport_size);
        }
        working.validate()?;
        *self = working;
        Ok(workspace_id)
    }

    fn apply_working(
        &mut self,
        transaction: WorkspaceTransaction,
    ) -> Result<DesktopEvent, DesktopError> {
        match transaction {
            WorkspaceTransaction::Bind { workspace, output } => self.bind(workspace, output),
            WorkspaceTransaction::Swap { first, second } => self.swap(first, second),
            WorkspaceTransaction::Unbind { workspace } => self.unbind(workspace),
            WorkspaceTransaction::SendWindow { window, target } => self.send_window(window, target),
        }
    }

    fn bind(
        &mut self,
        workspace_id: WorkspaceId,
        output_id: OutputId,
    ) -> Result<DesktopEvent, DesktopError> {
        let old_output = self
            .workspaces
            .get(&workspace_id)
            .ok_or(DesktopError::UnknownWorkspace(workspace_id))?
            .bound_output;
        let displaced = self
            .outputs
            .get(&output_id)
            .ok_or(DesktopError::UnknownOutput(output_id))?
            .current_workspace;
        if old_output == Some(output_id) {
            return Ok(DesktopEvent::WorkspaceBound {
                workspace: workspace_id,
                output: output_id,
            });
        }

        let old_size = old_output.and_then(|id| self.outputs.get(&id).map(|out| out.logical_size));
        let new_size = self.outputs[&output_id].logical_size;
        remap_floating(
            self.workspaces.get_mut(&workspace_id).unwrap(),
            old_size,
            new_size,
        );

        if let Some(old_output) = old_output {
            self.outputs.get_mut(&old_output).unwrap().current_workspace = displaced;
        }
        if let Some(displaced) = displaced {
            let displaced_old_size = Some(new_size);
            let displaced_new_size =
                old_output.and_then(|id| self.outputs.get(&id).map(|out| out.logical_size));
            let workspace = self.workspaces.get_mut(&displaced).unwrap();
            remap_floating_optional(workspace, displaced_old_size, displaced_new_size);
            workspace.bound_output = old_output;
        }
        self.outputs.get_mut(&output_id).unwrap().current_workspace = Some(workspace_id);
        self.workspaces.get_mut(&workspace_id).unwrap().bound_output = Some(output_id);

        Ok(DesktopEvent::WorkspaceBound {
            workspace: workspace_id,
            output: output_id,
        })
    }

    fn swap(
        &mut self,
        first: WorkspaceId,
        second: WorkspaceId,
    ) -> Result<DesktopEvent, DesktopError> {
        let first_output = self
            .workspaces
            .get(&first)
            .ok_or(DesktopError::UnknownWorkspace(first))?
            .bound_output;
        let second_output = self
            .workspaces
            .get(&second)
            .ok_or(DesktopError::UnknownWorkspace(second))?
            .bound_output;
        let first_size =
            first_output.and_then(|id| self.outputs.get(&id).map(|out| out.logical_size));
        let second_size =
            second_output.and_then(|id| self.outputs.get(&id).map(|out| out.logical_size));

        remap_floating_optional(
            self.workspaces.get_mut(&first).unwrap(),
            first_size,
            second_size,
        );
        remap_floating_optional(
            self.workspaces.get_mut(&second).unwrap(),
            second_size,
            first_size,
        );
        self.workspaces.get_mut(&first).unwrap().bound_output = second_output;
        self.workspaces.get_mut(&second).unwrap().bound_output = first_output;
        if let Some(output) = first_output {
            self.outputs.get_mut(&output).unwrap().current_workspace = Some(second);
        }
        if let Some(output) = second_output {
            self.outputs.get_mut(&output).unwrap().current_workspace = Some(first);
        }
        Ok(DesktopEvent::WorkspacesSwapped { first, second })
    }

    fn unbind(&mut self, workspace: WorkspaceId) -> Result<DesktopEvent, DesktopError> {
        let output = self
            .workspaces
            .get_mut(&workspace)
            .ok_or(DesktopError::UnknownWorkspace(workspace))?
            .bound_output
            .take();
        if let Some(output) = output {
            self.outputs.get_mut(&output).unwrap().current_workspace = None;
        }
        Ok(DesktopEvent::WorkspaceUnbound { workspace })
    }

    fn send_window(
        &mut self,
        window: WindowId,
        target: WorkspaceId,
    ) -> Result<DesktopEvent, DesktopError> {
        let source = self.find_window(window)?;
        if !self.workspaces.contains_key(&target) {
            return Err(DesktopError::UnknownWorkspace(target));
        }
        if source == target {
            return Ok(DesktopEvent::WindowSent {
                window,
                source,
                target,
            });
        }

        let mode = self.workspaces[&source].window_mode(window).unwrap();
        match mode {
            WindowMode::Tiled => {
                let size = self.workspaces[&source].tiled[&window].geometry.size;
                self.solver.apply(
                    self.workspaces.get_mut(&source).unwrap(),
                    WindowTransaction::Remove { id: window },
                )?;
                let target_workspace = self.workspaces.get_mut(&target).unwrap();
                let anchor = target_workspace
                    .focused_window
                    .and_then(|focused| target_workspace.tiled.get(&focused))
                    .map(|focused| focused.geometry.center())
                    .unwrap_or(Point::ORIGIN);
                let direction = target_workspace.focus_direction;
                self.solver.apply(
                    target_workspace,
                    WindowTransaction::InsertTiled {
                        id: window,
                        size,
                        anchor,
                        seed_direction: direction,
                    },
                )?;
            }
            WindowMode::Floating => {
                let source_viewport = self.workspace_viewport_size(source);
                let target_viewport = self.workspace_viewport_size(target);
                let placement = self
                    .workspaces
                    .get_mut(&source)
                    .unwrap()
                    .floating
                    .remove(&window)
                    .unwrap();
                self.workspaces
                    .get_mut(&source)
                    .unwrap()
                    .remove_focus(window);
                let viewport = target_viewport.unwrap_or(Size::new(1920, 1080));
                let placement = FloatingPlacement {
                    window,
                    rect: source_viewport
                        .map(|old| remap_rect(placement.rect, old, viewport))
                        .unwrap_or_else(|| {
                            crate::layout::clamp_to_viewport(placement.rect, viewport)
                        }),
                };
                let target_workspace = self.workspaces.get_mut(&target).unwrap();
                target_workspace.floating.insert(window, placement);
                target_workspace.focus(window);
                target_workspace.generation = target_workspace.generation.wrapping_add(1);
            }
            WindowMode::Fullscreen => {
                if let Some(fullscreen) = &self.workspaces[&target].fullscreen {
                    return Err(LayoutError::FullscreenOccupied(fullscreen.window).into());
                }
                let source_viewport = self.workspace_viewport_size(source);
                let target_viewport = self.workspace_viewport_size(target);
                let mut fullscreen = self
                    .workspaces
                    .get_mut(&source)
                    .unwrap()
                    .fullscreen
                    .take()
                    .unwrap();
                if let (RestorePlacement::Floating { viewport_rect }, Some(target_size)) =
                    (&mut fullscreen.restore, target_viewport)
                {
                    *viewport_rect = source_viewport
                        .map(|old| remap_rect(*viewport_rect, old, target_size))
                        .unwrap_or_else(|| {
                            crate::layout::clamp_to_viewport(*viewport_rect, target_size)
                        });
                }
                self.workspaces
                    .get_mut(&source)
                    .unwrap()
                    .remove_focus(window);
                let target_workspace = self.workspaces.get_mut(&target).unwrap();
                target_workspace.fullscreen = Some(fullscreen);
                target_workspace.focus(window);
                target_workspace.generation = target_workspace.generation.wrapping_add(1);
            }
        }
        if let Some(viewport_size) = self.workspace_viewport_size(target) {
            self.workspaces
                .get_mut(&target)
                .unwrap()
                .follow_focus(viewport_size);
        }
        Ok(DesktopEvent::WindowSent {
            window,
            source,
            target,
        })
    }

    fn workspace_viewport_size(&self, workspace: WorkspaceId) -> Option<Size> {
        let output = self.workspaces.get(&workspace)?.bound_output?;
        Some(self.outputs.get(&output)?.logical_size)
    }

    fn validate(&self) -> Result<(), DesktopError> {
        for workspace in self.workspaces.values() {
            if let Some(output_id) = workspace.bound_output {
                let output = self
                    .outputs
                    .get(&output_id)
                    .ok_or(DesktopError::UnknownOutput(output_id))?;
                if output.current_workspace != Some(workspace.id) {
                    return Err(DesktopError::UnknownWorkspace(workspace.id));
                }
            }
        }
        for output in self.outputs.values() {
            if let Some(workspace_id) = output.current_workspace {
                let workspace = self
                    .workspaces
                    .get(&workspace_id)
                    .ok_or(DesktopError::UnknownWorkspace(workspace_id))?;
                if workspace.bound_output != Some(output.id) {
                    return Err(DesktopError::UnknownOutput(output.id));
                }
            }
        }
        let mut windows = BTreeMap::new();
        for workspace in self.workspaces.values() {
            for window in workspace
                .tiled
                .keys()
                .chain(workspace.floating.keys())
                .chain(workspace.fullscreen.iter().map(|full| &full.window))
            {
                if let Some(first) = windows.insert(*window, workspace.id) {
                    return Err(DesktopError::DuplicateWindow {
                        window: *window,
                        first,
                        second: workspace.id,
                    });
                }
            }
        }
        Ok(())
    }
}

fn remap_floating(workspace: &mut Workspace, old: Option<Size>, new: Size) {
    if let Some(old) = old {
        for placement in workspace.floating.values_mut() {
            placement.rect = remap_rect(placement.rect, old, new);
        }
        if let Some(fullscreen) = &mut workspace.fullscreen {
            if let RestorePlacement::Floating { viewport_rect } = &mut fullscreen.restore {
                *viewport_rect = remap_rect(*viewport_rect, old, new);
            }
        }
    } else {
        for placement in workspace.floating.values_mut() {
            placement.rect = crate::layout::clamp_to_viewport(placement.rect, new);
        }
    }
}

fn remap_floating_optional(workspace: &mut Workspace, old: Option<Size>, new: Option<Size>) {
    if let Some(new) = new {
        remap_floating(workspace, old, new);
    }
}

fn remap_rect(rect: Rect, old: Size, new: Size) -> Rect {
    if !old.is_valid() || !new.is_valid() {
        return rect;
    }
    let center_x = rect.origin.x + rect.size.width / 2;
    let center_y = rect.origin.y + rect.size.height / 2;
    let mapped_center_x = center_x.saturating_mul(new.width) / old.width;
    let mapped_center_y = center_y.saturating_mul(new.height) / old.height;
    crate::layout::clamp_to_viewport(
        Rect::new(
            mapped_center_x - rect.size.width / 2,
            mapped_center_y - rect.size.height / 2,
            rect.size.width,
            rect.size.height,
        ),
        new,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CameraState, FullscreenPlacement, TiledWindow};

    fn desktop() -> Desktop {
        let mut desktop = Desktop::new(8);
        desktop
            .connect_output(Output::new(OutputId(1), "A", Size::new(1920, 1080)))
            .unwrap();
        desktop
            .connect_output(Output::new(OutputId(2), "B", Size::new(1280, 720)))
            .unwrap();
        desktop
            .add_workspace(Workspace::new(WorkspaceId(1)))
            .unwrap();
        desktop
            .add_workspace(Workspace::new(WorkspaceId(2)))
            .unwrap();
        desktop
            .apply(WorkspaceTransaction::Bind {
                workspace: WorkspaceId(1),
                output: OutputId(1),
            })
            .unwrap();
        desktop
            .apply(WorkspaceTransaction::Bind {
                workspace: WorkspaceId(2),
                output: OutputId(2),
            })
            .unwrap();
        desktop
    }

    #[test]
    fn occupied_bind_swaps_workspace_ownership() {
        let mut desktop = desktop();
        desktop
            .apply(WorkspaceTransaction::Bind {
                workspace: WorkspaceId(1),
                output: OutputId(2),
            })
            .unwrap();
        assert_eq!(
            desktop.workspaces[&WorkspaceId(1)].bound_output,
            Some(OutputId(2))
        );
        assert_eq!(
            desktop.workspaces[&WorkspaceId(2)].bound_output,
            Some(OutputId(1))
        );
    }

    #[test]
    fn disconnect_leaves_workspace_in_background_with_state() {
        let mut desktop = desktop();
        desktop.workspaces.get_mut(&WorkspaceId(1)).unwrap().camera = CameraState {
            center: Point::new(500, -200),
            zoom: 1.5,
            ..CameraState::default()
        };
        desktop.disconnect_output(OutputId(1)).unwrap();
        let workspace = &desktop.workspaces[&WorkspaceId(1)];
        assert_eq!(workspace.bound_output, None);
        assert_eq!(workspace.camera.center, Point::new(500, -200));
        assert_eq!(workspace.camera.zoom, 1.5);
    }

    #[test]
    fn floating_size_is_preserved_and_center_is_remapped() {
        let mut desktop = desktop();
        desktop
            .workspaces
            .get_mut(&WorkspaceId(1))
            .unwrap()
            .floating
            .insert(
                WindowId(9),
                FloatingPlacement {
                    window: WindowId(9),
                    rect: Rect::new(860, 440, 200, 200),
                },
            );
        desktop
            .apply(WorkspaceTransaction::Bind {
                workspace: WorkspaceId(1),
                output: OutputId(2),
            })
            .unwrap();
        let rect = desktop.workspaces[&WorkspaceId(1)].floating[&WindowId(9)].rect;
        assert_eq!(rect.size, Size::new(200, 200));
        assert_eq!(rect.center(), Point::new(640, 360));
    }

    #[test]
    fn send_tiled_window_uses_target_focus_anchor() {
        let mut desktop = desktop();
        desktop
            .workspaces
            .get_mut(&WorkspaceId(1))
            .unwrap()
            .tiled
            .insert(
                WindowId(1),
                TiledWindow {
                    id: WindowId(1),
                    geometry: Rect::new(0, 0, 100, 80),
                },
            );
        desktop
            .workspaces
            .get_mut(&WorkspaceId(2))
            .unwrap()
            .tiled
            .insert(
                WindowId(2),
                TiledWindow {
                    id: WindowId(2),
                    geometry: Rect::new(400, 300, 100, 80),
                },
            );
        desktop
            .workspaces
            .get_mut(&WorkspaceId(2))
            .unwrap()
            .focus(WindowId(2));
        desktop
            .apply(WorkspaceTransaction::SendWindow {
                window: WindowId(1),
                target: WorkspaceId(2),
            })
            .unwrap();
        assert!(!desktop.workspaces[&WorkspaceId(1)].contains_window(WindowId(1)));
        assert_eq!(
            desktop.workspaces[&WorkspaceId(2)].tiled[&WindowId(1)]
                .geometry
                .center(),
            Point::new(450, 340)
        );
        assert!(desktop.workspaces[&WorkspaceId(2)].tiled_windows_are_stable(8));
    }

    #[test]
    fn failed_fullscreen_send_is_fully_rolled_back() {
        let mut desktop = desktop();
        desktop
            .workspaces
            .get_mut(&WorkspaceId(1))
            .unwrap()
            .fullscreen = Some(FullscreenPlacement {
            window: WindowId(1),
            restore: RestorePlacement::Tiled {
                world_rect: Rect::new(0, 0, 100, 80),
            },
        });
        desktop
            .workspaces
            .get_mut(&WorkspaceId(2))
            .unwrap()
            .fullscreen = Some(FullscreenPlacement {
            window: WindowId(2),
            restore: RestorePlacement::Tiled {
                world_rect: Rect::new(0, 0, 100, 80),
            },
        });
        let error = desktop
            .apply(WorkspaceTransaction::SendWindow {
                window: WindowId(1),
                target: WorkspaceId(2),
            })
            .unwrap_err();
        assert_eq!(
            error,
            DesktopError::Layout(LayoutError::FullscreenOccupied(WindowId(2)))
        );
        assert_eq!(
            desktop.workspaces[&WorkspaceId(1)]
                .fullscreen
                .as_ref()
                .unwrap()
                .window,
            WindowId(1)
        );
    }

    #[test]
    fn sending_floating_window_preserves_size_and_relative_center() {
        let mut desktop = desktop();
        desktop
            .workspaces
            .get_mut(&WorkspaceId(1))
            .unwrap()
            .floating
            .insert(
                WindowId(3),
                FloatingPlacement {
                    window: WindowId(3),
                    rect: Rect::new(860, 440, 200, 200),
                },
            );
        desktop
            .apply(WorkspaceTransaction::SendWindow {
                window: WindowId(3),
                target: WorkspaceId(2),
            })
            .unwrap();
        let rect = desktop.workspaces[&WorkspaceId(2)].floating[&WindowId(3)].rect;
        assert_eq!(rect.size, Size::new(200, 200));
        assert_eq!(rect.center(), Point::new(640, 360));
    }

    #[test]
    fn fractional_scale_change_does_not_modify_workspace_camera_or_tiling() {
        let mut desktop = desktop();
        let workspace = desktop.workspaces.get_mut(&WorkspaceId(1)).unwrap();
        workspace.camera.center = Point::new(700, -300);
        workspace.tiled.insert(
            WindowId(4),
            TiledWindow {
                id: WindowId(4),
                geometry: Rect::new(50, 60, 800, 600),
            },
        );
        desktop
            .configure_output(
                OutputId(1),
                Size::new(2400, 1350),
                Size::new(1920, 1080),
                Scale120(150),
                OutputTransform::Normal,
            )
            .unwrap();
        let workspace = &desktop.workspaces[&WorkspaceId(1)];
        assert_eq!(workspace.camera.center, Point::new(700, -300));
        assert_eq!(
            workspace.tiled[&WindowId(4)].geometry,
            Rect::new(50, 60, 800, 600)
        );
    }

    #[test]
    fn centered_policy_moves_camera_to_focused_tiled_window() {
        let mut desktop = desktop();
        let workspace = desktop.workspaces.get_mut(&WorkspaceId(1)).unwrap();
        workspace.camera.policy = crate::CameraPolicy::Centered;
        workspace.tiled.insert(
            WindowId(5),
            TiledWindow {
                id: WindowId(5),
                geometry: Rect::new(1000, -400, 200, 100),
            },
        );
        desktop.focus_window(WindowId(5)).unwrap();
        assert_eq!(
            desktop.workspaces[&WorkspaceId(1)].camera.center,
            Point::new(1100, -350)
        );
    }

    #[test]
    fn keep_visible_policy_only_moves_camera_by_required_distance() {
        let mut desktop = desktop();
        let workspace = desktop.workspaces.get_mut(&WorkspaceId(1)).unwrap();
        workspace.camera.policy = crate::CameraPolicy::KeepVisible { margin: 32 };
        workspace.tiled.insert(
            WindowId(6),
            TiledWindow {
                id: WindowId(6),
                geometry: Rect::new(950, 0, 200, 100),
            },
        );
        desktop.focus_window(WindowId(6)).unwrap();
        assert_eq!(desktop.workspaces[&WorkspaceId(1)].camera.center.x, 222);
        assert_eq!(desktop.workspaces[&WorkspaceId(1)].camera.center.y, 0);
    }
}
