use std::collections::BTreeMap;

use thiserror::Error;

use crate::{
    LayoutError, Output, OutputId, OutputTransform, OutputWorkspaceSet, Point, RadialSolver,
    RestorePlacement, Scale120, Size, WindowId, WindowMode, WindowTransaction, Workspace,
    WorkspaceId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceTransaction {
    Focus {
        output: OutputId,
        workspace: WorkspaceId,
    },
    Move {
        workspace: WorkspaceId,
        target_output: OutputId,
        target_index: Option<usize>,
        activate: bool,
    },
    SetName {
        workspace: WorkspaceId,
        name: Option<String>,
    },
    SendWindow {
        window: WindowId,
        target: WorkspaceId,
        activate: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesktopEvent {
    WorkspaceFocused {
        workspace: WorkspaceId,
        output: OutputId,
    },
    WorkspaceMoved {
        workspace: WorkspaceId,
        source: Option<OutputId>,
        target: OutputId,
    },
    WorkspaceNamed {
        workspace: WorkspaceId,
        name: Option<String>,
    },
    OutputConnected {
        output: OutputId,
    },
    OutputDisconnected {
        output: OutputId,
    },
    WindowSent {
        window: WindowId,
        source: WorkspaceId,
        target: WorkspaceId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceLocation {
    pub output: Option<OutputId>,
    pub index: usize,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DesktopError {
    #[error("workspace {0:?} does not exist")]
    UnknownWorkspace(WorkspaceId),
    #[error("output {0:?} does not exist")]
    UnknownOutput(OutputId),
    #[error("window {0:?} does not exist")]
    UnknownWindow(WindowId),
    #[error("workspace name {0:?} is already in use")]
    DuplicateWorkspaceName(String),
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
    pub outputs: BTreeMap<OutputId, OutputWorkspaceSet>,
    pub detached: Vec<Workspace>,
    pub primary_output: Option<OutputId>,
    pub last_active: BTreeMap<String, WorkspaceId>,
    next_workspace_id: u32,
    solver: RadialSolver,
}

impl Desktop {
    pub fn new(gap: i64) -> Self {
        Self {
            outputs: BTreeMap::new(),
            detached: Vec::new(),
            primary_output: None,
            last_active: BTreeMap::new(),
            next_workspace_id: 0,
            solver: RadialSolver::new(gap),
        }
    }

    pub fn connect_output(&mut self, output: Output) -> Result<DesktopEvent, DesktopError> {
        let mut working = self.clone();
        let output_id = output.id;
        if working.outputs.contains_key(&output.id) {
            return Err(DesktopError::UnknownOutput(output.id));
        }
        let key = output.stable_key.clone();
        let target_size = output.logical_size;
        let source_viewport = working.primary_output.and_then(|primary| {
            working
                .outputs
                .get(&primary)
                .map(|set| (set.output.stable_key.clone(), set.output.logical_size))
        });
        let mut workspaces = if working.outputs.is_empty() {
            std::mem::take(&mut working.detached)
        } else {
            let primary = working
                .primary_output
                .expect("normal layout has a primary output");
            let primary_set = working.outputs.get_mut(&primary).unwrap();
            let mut restored = Vec::new();
            let mut index = 0;
            while index < primary_set.workspaces.len() {
                if primary_set.workspaces[index].original_output.as_deref() == Some(&key) {
                    let workspace = primary_set.workspaces.remove(index);
                    if workspace.has_windows_or_name() {
                        restored.push(workspace);
                    }
                    if index <= primary_set.active {
                        primary_set.active = primary_set.active.saturating_sub(1);
                    }
                } else {
                    index += 1;
                }
            }
            restored
        };
        workspaces.retain(Workspace::has_windows_or_name);
        for workspace in &mut workspaces {
            migrate_workspace_viewport(
                workspace,
                source_viewport
                    .as_ref()
                    .map(|(key, size)| (key.as_str(), *size)),
                &key,
                target_size,
            );
        }
        let active_id = working.last_active.remove(&key);
        let active = active_id
            .and_then(|id| workspaces.iter().position(|workspace| workspace.id == id))
            .unwrap_or(0);
        working.outputs.insert(
            output.id,
            OutputWorkspaceSet {
                output,
                workspaces,
                active,
            },
        );
        working
            .primary_output
            .get_or_insert_with(|| *working.outputs.keys().next().expect("output was inserted"));
        working.normalize_all();
        working.validate()?;
        *self = working;
        Ok(DesktopEvent::OutputConnected { output: output_id })
    }

    pub fn disconnect_output(&mut self, output: OutputId) -> Result<DesktopEvent, DesktopError> {
        let mut working = self.clone();
        let mut removed = working
            .outputs
            .remove(&output)
            .ok_or(DesktopError::UnknownOutput(output))?;
        if let Some(active) = removed.active_workspace() {
            working
                .last_active
                .insert(removed.output.stable_key.clone(), active.id);
        }
        removed.workspaces.retain(Workspace::has_windows_or_name);
        if working.outputs.is_empty() {
            working.detached.extend(removed.workspaces);
            working.primary_output = None;
        } else {
            if working.primary_output == Some(output) {
                working.primary_output = working.outputs.keys().next().copied();
            }
            let primary = working.primary_output.unwrap();
            let target_key = working.outputs[&primary].output.stable_key.clone();
            let target_size = working.outputs[&primary].output.logical_size;
            for workspace in &mut removed.workspaces {
                migrate_workspace_viewport(
                    workspace,
                    Some((
                        removed.output.stable_key.as_str(),
                        removed.output.logical_size,
                    )),
                    &target_key,
                    target_size,
                );
            }
            let target = working.outputs.get_mut(&primary).unwrap();
            let insert = target.workspaces.len().saturating_sub(1);
            target.workspaces.splice(insert..insert, removed.workspaces);
        }
        working.normalize_all();
        working.validate()?;
        *self = working;
        Ok(DesktopEvent::OutputDisconnected { output })
    }

    pub fn apply(
        &mut self,
        transaction: WorkspaceTransaction,
    ) -> Result<DesktopEvent, DesktopError> {
        let mut working = self.clone();
        let event = working.apply_working(transaction)?;
        working.normalize_all();
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
        let updates_original_output = matches!(transaction, WindowTransaction::InsertTiled { .. });
        let viewport_size = working.workspace_viewport_size(workspace);
        let solver = working.solver.clone();
        solver.apply(working.workspace_mut(workspace)?, transaction)?;
        working.store_workspace_viewport(workspace);
        if let Some(viewport_size) = viewport_size {
            working
                .workspace_mut(workspace)?
                .follow_focus(viewport_size);
        }
        if updates_original_output {
            working.reset_original_output_for_ordinary(workspace);
        }
        working.normalize_all();
        working.validate()?;
        *self = working;
        Ok(())
    }

    pub fn workspace(&self, id: WorkspaceId) -> Result<&Workspace, DesktopError> {
        let location = self.workspace_location(id)?;
        Ok(match location.output {
            Some(output) => &self.outputs[&output].workspaces[location.index],
            None => &self.detached[location.index],
        })
    }

    pub fn workspace_mut(&mut self, id: WorkspaceId) -> Result<&mut Workspace, DesktopError> {
        let location = self.workspace_location(id)?;
        Ok(match location.output {
            Some(output) => &mut self.outputs.get_mut(&output).unwrap().workspaces[location.index],
            None => &mut self.detached[location.index],
        })
    }

    pub fn workspace_location(&self, id: WorkspaceId) -> Result<WorkspaceLocation, DesktopError> {
        for (output, set) in &self.outputs {
            if let Some(index) = set
                .workspaces
                .iter()
                .position(|workspace| workspace.id == id)
            {
                return Ok(WorkspaceLocation {
                    output: Some(*output),
                    index,
                });
            }
        }
        self.detached
            .iter()
            .position(|workspace| workspace.id == id)
            .map(|index| WorkspaceLocation {
                output: None,
                index,
            })
            .ok_or(DesktopError::UnknownWorkspace(id))
    }

    pub fn workspaces(&self) -> impl Iterator<Item = &Workspace> {
        self.outputs
            .values()
            .flat_map(|set| set.workspaces.iter())
            .chain(self.detached.iter())
    }

    pub fn workspace_for_output(&self, output: OutputId) -> Option<&Workspace> {
        self.outputs.get(&output)?.active_workspace()
    }

    pub fn active_workspace_id(&self, output: OutputId) -> Option<WorkspaceId> {
        Some(self.workspace_for_output(output)?.id)
    }

    pub fn workspace_by_local_index(
        &self,
        output: OutputId,
        one_based_index: usize,
    ) -> Option<&Workspace> {
        one_based_index
            .checked_sub(1)
            .and_then(|index| self.outputs.get(&output)?.workspaces.get(index))
    }

    pub fn workspace_local_index(&self, workspace: WorkspaceId) -> Option<usize> {
        let location = self.workspace_location(workspace).ok()?;
        location.output.map(|_| location.index + 1)
    }

    pub fn workspace_by_name(&self, name: &str) -> Option<&Workspace> {
        self.workspaces()
            .find(|workspace| workspace.name.as_deref() == Some(name))
    }

    pub fn output(&self, output: OutputId) -> Option<&Output> {
        self.outputs.get(&output).map(|set| &set.output)
    }

    pub fn output_mut(&mut self, output: OutputId) -> Option<&mut Output> {
        self.outputs.get_mut(&output).map(|set| &mut set.output)
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
        let set = working
            .outputs
            .get_mut(&output)
            .ok_or(DesktopError::UnknownOutput(output))?;
        let old_size = set.output.logical_size;
        let key = set.output.stable_key.clone();
        set.output.physical_size = physical_size;
        set.output.logical_size = logical_size;
        set.output.native_scale = native_scale;
        set.output.transform = transform;
        for workspace in &mut set.workspaces {
            remap_floating(workspace, &key, old_size, logical_size);
        }
        working.validate()?;
        *self = working;
        Ok(())
    }

    pub fn reconfigure_layout(
        &mut self,
        gap: i64,
        camera_policy: crate::CameraPolicy,
    ) -> Result<(), DesktopError> {
        let mut working = self.clone();
        working.solver = RadialSolver::new(gap);
        let ids = working
            .workspaces()
            .map(|workspace| workspace.id)
            .collect::<Vec<_>>();
        for id in ids {
            let solver = working.solver.clone();
            let viewport = working.workspace_viewport_size(id);
            let workspace = working.workspace_mut(id)?;
            workspace.camera.policy = camera_policy;
            solver.reflow(workspace)?;
            if let Some(viewport) = viewport {
                workspace.follow_focus(viewport);
            }
        }
        working.validate()?;
        *self = working;
        Ok(())
    }

    pub fn find_window(&self, window: WindowId) -> Result<WorkspaceId, DesktopError> {
        let mut found = None;
        for workspace in self.workspaces() {
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
        let workspace = working.find_window(window)?;
        let viewport = working.workspace_viewport_size(workspace);
        working.workspace_mut(workspace)?.focus(window);
        if let Some(viewport) = viewport {
            working.workspace_mut(workspace)?.follow_focus(viewport);
        }
        if let Ok(location) = working.workspace_location(workspace)
            && let Some(output) = location.output
        {
            working.outputs.get_mut(&output).unwrap().active = location.index;
        }
        working.validate()?;
        *self = working;
        Ok(workspace)
    }

    pub fn focus_direction(
        &mut self,
        workspace: WorkspaceId,
        direction: crate::Direction,
    ) -> Result<Option<WindowId>, DesktopError> {
        let mut working = self.clone();
        let state = working.workspace(workspace)?;
        let Some(focused) = state.focused_window else {
            return Ok(None);
        };
        let Some(mode) = state.window_mode(focused) else {
            return Ok(None);
        };
        if mode == WindowMode::Fullscreen {
            return Ok(None);
        }
        let origin = match mode {
            WindowMode::Tiled => state.tiled[&focused].geometry.center(),
            WindowMode::Floating => state.floating[&focused].viewport.rect.center(),
            WindowMode::Fullscreen => unreachable!(),
        };
        let direction = direction.normalized();
        let candidates: Box<dyn Iterator<Item = (WindowId, Point)> + '_> = match mode {
            WindowMode::Tiled => Box::new(
                state
                    .tiled
                    .iter()
                    .map(|(id, window)| (*id, window.geometry.center())),
            ),
            WindowMode::Floating => Box::new(
                state
                    .floating
                    .iter()
                    .map(|(id, window)| (*id, window.viewport.rect.center())),
            ),
            WindowMode::Fullscreen => unreachable!(),
        };
        let target = candidates
            .filter(|(id, _)| *id != focused)
            .filter_map(|(id, center)| {
                let dx = (center.x - origin.x) as f64;
                let dy = (center.y - origin.y) as f64;
                let forward = dx * direction.x + dy * direction.y;
                (forward > f64::EPSILON).then(|| {
                    let distance = dx.hypot(dy);
                    let angle = (dx * direction.y - dy * direction.x).abs() / distance;
                    (id, angle, distance)
                })
            })
            .min_by(|left, right| {
                left.1
                    .total_cmp(&right.1)
                    .then_with(|| left.2.total_cmp(&right.2))
                    .then_with(|| left.0.cmp(&right.0))
            })
            .map(|(id, _, _)| id);
        let Some(target) = target else {
            return Ok(None);
        };
        let viewport = working.workspace_viewport_size(workspace);
        let state = working.workspace_mut(workspace)?;
        state.focus(target);
        if mode == WindowMode::Tiled {
            state.layout_direction_hint = direction;
        }
        if let Some(viewport) = viewport {
            state.follow_focus(viewport);
        }
        working.validate()?;
        *self = working;
        Ok(Some(target))
    }

    fn apply_working(
        &mut self,
        transaction: WorkspaceTransaction,
    ) -> Result<DesktopEvent, DesktopError> {
        match transaction {
            WorkspaceTransaction::Focus { output, workspace } => {
                let location = self.workspace_location(workspace)?;
                if location.output != Some(output) {
                    return Err(DesktopError::UnknownWorkspace(workspace));
                }
                self.outputs.get_mut(&output).unwrap().active = location.index;
                Ok(DesktopEvent::WorkspaceFocused { workspace, output })
            }
            WorkspaceTransaction::Move {
                workspace,
                target_output,
                target_index,
                activate,
            } => self.move_workspace(workspace, target_output, target_index, activate),
            WorkspaceTransaction::SetName { workspace, name } => {
                if let Some(name) = name.as_deref()
                    && self.workspaces().any(|candidate| {
                        candidate.id != workspace && candidate.name.as_deref() == Some(name)
                    })
                {
                    return Err(DesktopError::DuplicateWorkspaceName(name.to_owned()));
                }
                self.workspace_mut(workspace)?.name = name.clone();
                Ok(DesktopEvent::WorkspaceNamed { workspace, name })
            }
            WorkspaceTransaction::SendWindow {
                window,
                target,
                activate,
            } => self.send_window(window, target, activate),
        }
    }

    fn move_workspace(
        &mut self,
        workspace: WorkspaceId,
        target_output: OutputId,
        target_index: Option<usize>,
        activate: bool,
    ) -> Result<DesktopEvent, DesktopError> {
        if !self.outputs.contains_key(&target_output) {
            return Err(DesktopError::UnknownOutput(target_output));
        }
        let source = self.workspace_location(workspace)?;
        let source_viewport = source.output.map(|output| {
            (
                self.outputs[&output].output.stable_key.clone(),
                self.outputs[&output].output.logical_size,
            )
        });
        let mut value = match source.output {
            Some(output) => {
                let set = self.outputs.get_mut(&output).unwrap();
                let value = set.workspaces.remove(source.index);
                if source.index < set.active {
                    set.active -= 1;
                } else if source.index == set.active {
                    set.active = source
                        .index
                        .saturating_sub(1)
                        .min(set.workspaces.len().saturating_sub(1));
                }
                value
            }
            None => self.detached.remove(source.index),
        };
        let key = self.outputs[&target_output].output.stable_key.clone();
        let target_size = self.outputs[&target_output].output.logical_size;
        migrate_workspace_viewport(
            &mut value,
            source_viewport
                .as_ref()
                .map(|(key, size)| (key.as_str(), *size)),
            &key,
            target_size,
        );
        value.original_output = Some(key);
        let target = self.outputs.get_mut(&target_output).unwrap();
        let limit = target.workspaces.len().saturating_sub(1);
        let index = target_index.unwrap_or(target.active + 1).min(limit);
        target.workspaces.insert(index, value);
        if activate {
            target.active = index;
        } else if index <= target.active {
            // Preserve the same active workspace when insertion shifts its vector index.
            target.active += 1;
        }
        Ok(DesktopEvent::WorkspaceMoved {
            workspace,
            source: source.output,
            target: target_output,
        })
    }

    fn send_window(
        &mut self,
        window: WindowId,
        target: WorkspaceId,
        activate: bool,
    ) -> Result<DesktopEvent, DesktopError> {
        let source = self.find_window(window)?;
        self.workspace(target)?;
        if source == target {
            return Ok(DesktopEvent::WindowSent {
                window,
                source,
                target,
            });
        }
        let source_viewport = self.workspace_location(source)?.output.map(|output| {
            (
                self.outputs[&output].output.stable_key.clone(),
                self.outputs[&output].output.logical_size,
            )
        });
        let target_viewport = self.workspace_location(target)?.output.map(|output| {
            (
                self.outputs[&output].output.stable_key.clone(),
                self.outputs[&output].output.logical_size,
            )
        });
        let mode = self.workspace(source)?.window_mode(window).unwrap();
        match mode {
            WindowMode::Tiled => {
                let size = self.workspace(source)?.tiled[&window].geometry.size;
                let solver = self.solver.clone();
                solver.apply(
                    self.workspace_mut(source)?,
                    WindowTransaction::Remove { id: window },
                )?;
                let target_workspace = self.workspace_mut(target)?;
                let anchor = target_workspace
                    .focused_window
                    .and_then(|focused| target_workspace.tiled.get(&focused))
                    .map(|focused| focused.geometry.center())
                    .unwrap_or(Point::ORIGIN);
                let direction = target_workspace.layout_direction_hint;
                solver.apply(
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
                let mut placement = self
                    .workspace_mut(source)?
                    .floating
                    .remove(&window)
                    .unwrap();
                self.workspace_mut(source)?.remove_focus(window);
                placement.window = window;
                if let Some((target_key, target_size)) = &target_viewport {
                    migrate_viewport_placement(
                        &mut placement.viewport,
                        source_viewport
                            .as_ref()
                            .map(|(key, size)| (key.as_str(), *size)),
                        target_key,
                        *target_size,
                    );
                }
                self.workspace_mut(target)?
                    .floating
                    .insert(window, placement);
                self.workspace_mut(target)?.focus(window);
            }
            WindowMode::Fullscreen => {
                if let Some(fullscreen) = &self.workspace(target)?.fullscreen {
                    return Err(LayoutError::FullscreenOccupied(fullscreen.window).into());
                }
                let mut fullscreen = self.workspace_mut(source)?.fullscreen.take().unwrap();
                if let (RestorePlacement::Floating { viewport }, Some((target_key, target_size))) =
                    (&mut fullscreen.restore, &target_viewport)
                {
                    migrate_viewport_placement(
                        viewport,
                        source_viewport
                            .as_ref()
                            .map(|(key, size)| (key.as_str(), *size)),
                        target_key,
                        *target_size,
                    );
                }
                self.workspace_mut(source)?.remove_focus(window);
                self.workspace_mut(target)?.fullscreen = Some(fullscreen);
                self.workspace_mut(target)?.focus(window);
            }
        }
        self.reset_original_output_for_ordinary(target);
        if activate
            && let Ok(location) = self.workspace_location(target)
            && let Some(output) = location.output
        {
            self.outputs.get_mut(&output).unwrap().active = location.index;
        }
        Ok(DesktopEvent::WindowSent {
            window,
            source,
            target,
        })
    }

    fn reset_original_output_for_ordinary(&mut self, workspace: WorkspaceId) {
        let Ok(location) = self.workspace_location(workspace) else {
            return;
        };
        let Some(output) = location.output else {
            return;
        };
        if self
            .workspace(workspace)
            .ok()
            .is_some_and(|workspace| workspace.name.is_none())
        {
            let key = self.outputs[&output].output.stable_key.clone();
            self.workspace_mut(workspace).unwrap().original_output = Some(key);
        }
    }

    fn workspace_viewport_size(&self, workspace: WorkspaceId) -> Option<Size> {
        let location = self.workspace_location(workspace).ok()?;
        let output = location.output?;
        Some(self.outputs[&output].output.logical_size)
    }

    fn store_workspace_viewport(&mut self, workspace: WorkspaceId) {
        let Ok(location) = self.workspace_location(workspace) else {
            return;
        };
        let Some(output) = location.output else {
            return;
        };
        let key = self.outputs[&output].output.stable_key.clone();
        let size = self.outputs[&output].output.logical_size;
        store_workspace_viewport(self.workspace_mut(workspace).unwrap(), &key, size);
    }

    fn allocate_workspace(&mut self, original_output: Option<String>) -> Workspace {
        let id = WorkspaceId(self.next_workspace_id);
        self.next_workspace_id = self.next_workspace_id.wrapping_add(1);
        let mut workspace = Workspace::new(id);
        workspace.original_output = original_output;
        workspace
    }

    fn normalize_all(&mut self) {
        let outputs = self.outputs.keys().copied().collect::<Vec<_>>();
        for output in outputs {
            self.normalize_output(output);
        }
        self.detached.retain(Workspace::has_windows_or_name);
    }

    fn normalize_output(&mut self, output: OutputId) {
        let key = self.outputs[&output].output.stable_key.clone();
        let set = self.outputs.get_mut(&output).unwrap();
        let mut index = 0;
        while index < set.workspaces.len() {
            let last = index + 1 == set.workspaces.len();
            if !set.workspaces[index].has_windows_or_name() && index != set.active && !last {
                set.workspaces.remove(index);
                if index < set.active {
                    set.active -= 1;
                }
            } else {
                index += 1;
            }
        }
        let needs_placeholder = set
            .workspaces
            .last()
            .is_none_or(Workspace::has_windows_or_name);
        if needs_placeholder {
            let workspace = self.allocate_workspace(Some(key));
            self.outputs
                .get_mut(&output)
                .unwrap()
                .workspaces
                .push(workspace);
        }
        let set = self.outputs.get_mut(&output).unwrap();
        set.active = set.active.min(set.workspaces.len() - 1);
    }

    fn validate(&self) -> Result<(), DesktopError> {
        if self.outputs.is_empty() != self.primary_output.is_none() {
            return Err(DesktopError::UnknownOutput(
                self.primary_output.unwrap_or(OutputId(u32::MAX)),
            ));
        }
        if let Some(primary) = self.primary_output
            && !self.outputs.contains_key(&primary)
        {
            return Err(DesktopError::UnknownOutput(primary));
        }
        let mut ids = BTreeMap::new();
        let mut names = BTreeMap::new();
        let mut windows = BTreeMap::new();
        for (output, set) in &self.outputs {
            if set.output.id != *output || set.active >= set.workspaces.len() {
                return Err(DesktopError::UnknownOutput(*output));
            }
            if set
                .workspaces
                .last()
                .is_none_or(Workspace::has_windows_or_name)
            {
                return Err(DesktopError::UnknownOutput(*output));
            }
        }
        for workspace in self.workspaces() {
            if ids.insert(workspace.id, ()).is_some() {
                return Err(DesktopError::UnknownWorkspace(workspace.id));
            }
            if let Some(name) = &workspace.name
                && names.insert(name, workspace.id).is_some()
            {
                return Err(DesktopError::DuplicateWorkspaceName(name.clone()));
            }
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

impl Workspace {
    pub fn has_windows_or_name(&self) -> bool {
        self.name.is_some()
            || !self.tiled.is_empty()
            || !self.floating.is_empty()
            || self.fullscreen.is_some()
    }
}

fn store_workspace_viewport(workspace: &mut Workspace, key: &str, size: Size) {
    for placement in workspace.floating.values_mut() {
        placement.viewport.store_for_output(key, size);
    }
    if let Some(fullscreen) = &mut workspace.fullscreen
        && let RestorePlacement::Floating { viewport } = &mut fullscreen.restore
    {
        viewport.store_for_output(key, size);
    }
}

fn migrate_workspace_viewport(
    workspace: &mut Workspace,
    source: Option<(&str, Size)>,
    target_key: &str,
    target_size: Size,
) {
    for placement in workspace.floating.values_mut() {
        migrate_viewport_placement(&mut placement.viewport, source, target_key, target_size);
    }
    if let Some(fullscreen) = &mut workspace.fullscreen
        && let RestorePlacement::Floating { viewport } = &mut fullscreen.restore
    {
        migrate_viewport_placement(viewport, source, target_key, target_size);
    }
}

fn migrate_viewport_placement(
    placement: &mut crate::ViewportPlacement,
    source: Option<(&str, Size)>,
    target_key: &str,
    target_size: Size,
) {
    if let Some((source_key, source_size)) = source {
        placement.store_for_output(source_key, source_size);
    }
    placement.rect = placement
        .output_rects
        .get(target_key)
        .copied()
        .map(|rect| crate::layout::clamp_to_viewport(rect, target_size))
        .unwrap_or_else(|| {
            let center = placement.normalized_center.center_in(target_size);
            crate::layout::clamp_to_viewport(
                crate::Rect::new(
                    center.x - placement.rect.size.width / 2,
                    center.y - placement.rect.size.height / 2,
                    placement.rect.size.width,
                    placement.rect.size.height,
                ),
                target_size,
            )
        });
    placement.store_for_output(target_key, target_size);
}

fn remap_floating(workspace: &mut Workspace, key: &str, old: Size, new: Size) {
    for placement in workspace.floating.values_mut() {
        remap_viewport(&mut placement.viewport, key, old, new);
    }
    if let Some(fullscreen) = &mut workspace.fullscreen
        && let RestorePlacement::Floating { viewport } = &mut fullscreen.restore
    {
        remap_viewport(viewport, key, old, new);
    }
}

fn remap_viewport(placement: &mut crate::ViewportPlacement, key: &str, old: Size, new: Size) {
    placement.store_for_output(key, old);
    let center = placement.normalized_center.center_in(new);
    placement.rect = crate::layout::clamp_to_viewport(
        crate::Rect::new(
            center.x - placement.rect.size.width / 2,
            center.y - placement.rect.size.height / 2,
            placement.rect.size.width,
            placement.rect.size.height,
        ),
        new,
    );
    placement.store_for_output(key, new);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(id: u32, key: &str) -> Output {
        Output::new(OutputId(id), key, Size::new(1920, 1080))
    }

    #[test]
    fn every_output_has_one_trailing_placeholder() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        let set = &desktop.outputs[&OutputId(1)];
        assert_eq!(set.workspaces.len(), 1);
        assert!(!set.workspaces[0].has_windows_or_name());
    }

    #[test]
    fn naming_placeholder_creates_another_placeholder() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        let id = desktop.outputs[&OutputId(1)].workspaces[0].id;
        desktop
            .apply(WorkspaceTransaction::SetName {
                workspace: id,
                name: Some("chat".into()),
            })
            .unwrap();
        let set = &desktop.outputs[&OutputId(1)];
        assert_eq!(set.workspaces.len(), 2);
        assert_eq!(set.workspaces[0].name.as_deref(), Some("chat"));
        assert!(!set.workspaces[1].has_windows_or_name());
    }

    #[test]
    fn disconnected_workspaces_move_to_primary_and_restore() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        desktop.connect_output(output(2, "B")).unwrap();
        let workspace = desktop.outputs[&OutputId(2)].workspaces[0].id;
        desktop
            .apply(WorkspaceTransaction::SetName {
                workspace,
                name: Some("code".into()),
            })
            .unwrap();
        desktop.disconnect_output(OutputId(2)).unwrap();
        assert_eq!(
            desktop.workspace_location(workspace).unwrap().output,
            Some(OutputId(1))
        );
        desktop.connect_output(output(2, "B")).unwrap();
        assert_eq!(
            desktop.workspace_location(workspace).unwrap().output,
            Some(OutputId(2))
        );
    }

    #[test]
    fn duplicate_names_roll_back() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        let first = desktop.outputs[&OutputId(1)].workspaces[0].id;
        desktop
            .apply(WorkspaceTransaction::SetName {
                workspace: first,
                name: Some("one".into()),
            })
            .unwrap();
        let second = desktop.outputs[&OutputId(1)].workspaces[1].id;
        let before = desktop.clone();
        assert_eq!(
            desktop.apply(WorkspaceTransaction::SetName {
                workspace: second,
                name: Some("one".into()),
            }),
            Err(DesktopError::DuplicateWorkspaceName("one".into()))
        );
        assert_eq!(
            desktop.outputs[&OutputId(1)].workspaces.len(),
            before.outputs[&OutputId(1)].workspaces.len()
        );
    }

    #[test]
    fn floating_placement_restores_per_output_geometry() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        desktop
            .connect_output(Output::new(OutputId(2), "B", Size::new(1280, 720)))
            .unwrap();
        let workspace = desktop.active_workspace_id(OutputId(1)).unwrap();
        let window = WindowId(10);
        desktop
            .apply_window(
                workspace,
                WindowTransaction::InsertTiled {
                    id: window,
                    size: Size::new(400, 300),
                    anchor: Point::ORIGIN,
                    seed_direction: crate::Direction::RIGHT,
                },
            )
            .unwrap();
        desktop
            .apply_window(
                workspace,
                WindowTransaction::SetMode {
                    id: window,
                    mode: WindowMode::Floating,
                    viewport_size: Size::new(1920, 1080),
                },
            )
            .unwrap();
        desktop
            .apply_window(
                workspace,
                WindowTransaction::MoveFloating {
                    id: window,
                    target: crate::Rect::new(1300, 600, 400, 300),
                    viewport_size: Size::new(1920, 1080),
                },
            )
            .unwrap();
        let original = desktop.workspace(workspace).unwrap().floating[&window]
            .viewport
            .rect;

        desktop
            .apply(WorkspaceTransaction::Move {
                workspace,
                target_output: OutputId(2),
                target_index: None,
                activate: true,
            })
            .unwrap();
        assert_ne!(
            desktop.workspace(workspace).unwrap().floating[&window]
                .viewport
                .rect,
            original
        );
        desktop
            .apply(WorkspaceTransaction::Move {
                workspace,
                target_output: OutputId(1),
                target_index: None,
                activate: true,
            })
            .unwrap();
        assert_eq!(
            desktop.workspace(workspace).unwrap().floating[&window]
                .viewport
                .rect,
            original
        );
    }

    #[test]
    fn closing_window_does_not_change_original_output() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        desktop.connect_output(output(2, "B")).unwrap();
        let workspace = desktop.active_workspace_id(OutputId(2)).unwrap();
        desktop
            .apply(WorkspaceTransaction::SetName {
                workspace,
                name: Some("persistent".into()),
            })
            .unwrap();
        let window = WindowId(20);
        desktop
            .apply_window(
                workspace,
                WindowTransaction::InsertTiled {
                    id: window,
                    size: Size::new(400, 300),
                    anchor: Point::ORIGIN,
                    seed_direction: crate::Direction::RIGHT,
                },
            )
            .unwrap();
        desktop.workspace_mut(workspace).unwrap().original_output = Some("A".into());
        desktop
            .apply_window(workspace, WindowTransaction::Remove { id: window })
            .unwrap();
        assert_eq!(
            desktop
                .workspace(workspace)
                .unwrap()
                .original_output
                .as_deref(),
            Some("A")
        );
    }

    #[test]
    fn empty_ordinary_workspace_is_removed_after_focus_leaves() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        let first = desktop.active_workspace_id(OutputId(1)).unwrap();
        let window = WindowId(30);
        desktop
            .apply_window(
                first,
                WindowTransaction::InsertTiled {
                    id: window,
                    size: Size::new(400, 300),
                    anchor: Point::ORIGIN,
                    seed_direction: crate::Direction::RIGHT,
                },
            )
            .unwrap();
        desktop
            .apply_window(first, WindowTransaction::Remove { id: window })
            .unwrap();
        let placeholder = desktop.outputs[&OutputId(1)].workspaces.last().unwrap().id;
        desktop
            .apply(WorkspaceTransaction::Focus {
                output: OutputId(1),
                workspace: placeholder,
            })
            .unwrap();
        assert!(desktop.workspace(first).is_err());
        assert_eq!(desktop.outputs[&OutputId(1)].workspaces.len(), 1);
    }

    #[test]
    fn last_output_detaches_and_reconnect_restores_active_workspace() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        let first = desktop.active_workspace_id(OutputId(1)).unwrap();
        desktop
            .apply(WorkspaceTransaction::SetName {
                workspace: first,
                name: Some("one".into()),
            })
            .unwrap();
        let second = desktop.outputs[&OutputId(1)].workspaces.last().unwrap().id;
        desktop
            .apply(WorkspaceTransaction::SetName {
                workspace: second,
                name: Some("two".into()),
            })
            .unwrap();
        desktop
            .apply(WorkspaceTransaction::Focus {
                output: OutputId(1),
                workspace: second,
            })
            .unwrap();

        desktop.disconnect_output(OutputId(1)).unwrap();
        assert!(desktop.outputs.is_empty());
        assert_eq!(desktop.detached.len(), 2);
        desktop.connect_output(output(9, "A")).unwrap();
        assert_eq!(desktop.active_workspace_id(OutputId(9)), Some(second));
        assert_eq!(
            desktop.workspace_location(first).unwrap().output,
            Some(OutputId(9))
        );
    }

    #[test]
    fn directional_focus_is_mode_local_and_only_tiled_updates_hint() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        let workspace = desktop.active_workspace_id(OutputId(1)).unwrap();
        let left = WindowId(40);
        let right = WindowId(41);
        let float_left = WindowId(42);
        let float_right = WindowId(43);
        let state = desktop.workspace_mut(workspace).unwrap();
        state.tiled.insert(
            left,
            crate::TiledWindow {
                id: left,
                geometry: crate::Rect::new(0, 0, 100, 100),
            },
        );
        state.tiled.insert(
            right,
            crate::TiledWindow {
                id: right,
                geometry: crate::Rect::new(500, 0, 100, 100),
            },
        );
        state.floating.insert(
            float_left,
            crate::FloatingPlacement {
                window: float_left,
                viewport: crate::ViewportPlacement::new(
                    crate::Rect::new(0, 300, 100, 100),
                    Size::new(1920, 1080),
                ),
            },
        );
        state.floating.insert(
            float_right,
            crate::FloatingPlacement {
                window: float_right,
                viewport: crate::ViewportPlacement::new(
                    crate::Rect::new(500, 300, 100, 100),
                    Size::new(1920, 1080),
                ),
            },
        );
        state.focus(left);
        desktop.normalize_all();

        assert_eq!(
            desktop
                .focus_direction(workspace, crate::Direction::RIGHT)
                .unwrap(),
            Some(right)
        );
        assert_eq!(
            desktop.workspace(workspace).unwrap().layout_direction_hint,
            crate::Direction::RIGHT
        );
        desktop.workspace_mut(workspace).unwrap().focus(float_left);
        let old_hint = desktop.workspace(workspace).unwrap().layout_direction_hint;
        assert_eq!(
            desktop
                .focus_direction(workspace, crate::Direction::RIGHT)
                .unwrap(),
            Some(float_right)
        );
        assert_eq!(
            desktop.workspace(workspace).unwrap().layout_direction_hint,
            old_hint
        );
    }

    #[test]
    fn invalid_output_and_workspace_transactions_do_not_mutate_state() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        let workspace = desktop.active_workspace_id(OutputId(1)).unwrap();
        let before_count = desktop.workspaces().count();

        assert_eq!(
            desktop.connect_output(output(1, "duplicate")),
            Err(DesktopError::UnknownOutput(OutputId(1)))
        );
        assert_eq!(
            desktop.disconnect_output(OutputId(99)),
            Err(DesktopError::UnknownOutput(OutputId(99)))
        );
        assert_eq!(
            desktop.apply(WorkspaceTransaction::Focus {
                output: OutputId(99),
                workspace,
            }),
            Err(DesktopError::UnknownWorkspace(workspace))
        );
        assert_eq!(
            desktop.apply(WorkspaceTransaction::Move {
                workspace,
                target_output: OutputId(99),
                target_index: None,
                activate: false,
            }),
            Err(DesktopError::UnknownOutput(OutputId(99)))
        );
        assert_eq!(desktop.workspaces().count(), before_count);
        assert_eq!(desktop.active_workspace_id(OutputId(1)), Some(workspace));
    }

    #[test]
    fn moving_workspace_honors_index_and_activation() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        desktop.connect_output(output(2, "B")).unwrap();
        let source = desktop.active_workspace_id(OutputId(1)).unwrap();
        desktop
            .apply(WorkspaceTransaction::SetName {
                workspace: source,
                name: Some("moved".into()),
            })
            .unwrap();
        let old_active = desktop.active_workspace_id(OutputId(2)).unwrap();

        let event = desktop
            .apply(WorkspaceTransaction::Move {
                workspace: source,
                target_output: OutputId(2),
                target_index: Some(0),
                activate: false,
            })
            .unwrap();
        assert_eq!(
            event,
            DesktopEvent::WorkspaceMoved {
                workspace: source,
                source: Some(OutputId(1)),
                target: OutputId(2),
            }
        );
        assert_eq!(desktop.workspace_local_index(source), Some(1));
        assert_eq!(desktop.active_workspace_id(OutputId(2)), Some(old_active));

        desktop
            .apply(WorkspaceTransaction::Move {
                workspace: source,
                target_output: OutputId(2),
                target_index: Some(usize::MAX),
                activate: true,
            })
            .unwrap();
        assert_eq!(desktop.active_workspace_id(OutputId(2)), Some(source));
    }

    #[test]
    fn sending_tiled_window_moves_ownership_and_focus_atomically() {
        let mut desktop = Desktop::new(8);
        desktop.connect_output(output(1, "A")).unwrap();
        let source = desktop.active_workspace_id(OutputId(1)).unwrap();
        desktop
            .apply(WorkspaceTransaction::SetName {
                workspace: source,
                name: Some("source".into()),
            })
            .unwrap();
        let target = desktop.outputs[&OutputId(1)].workspaces.last().unwrap().id;
        let window = WindowId(80);
        desktop
            .apply_window(
                source,
                WindowTransaction::InsertTiled {
                    id: window,
                    size: Size::new(300, 200),
                    anchor: Point::new(100, 100),
                    seed_direction: crate::Direction::RIGHT,
                },
            )
            .unwrap();
        desktop
            .apply(WorkspaceTransaction::SendWindow {
                window,
                target,
                activate: true,
            })
            .unwrap();

        assert!(!desktop.workspace(source).unwrap().contains_window(window));
        assert_eq!(desktop.find_window(window), Ok(target));
        assert_eq!(
            desktop.workspace(target).unwrap().focused_window,
            Some(window)
        );
        assert_eq!(desktop.active_workspace_id(OutputId(1)), Some(target));
    }
}
