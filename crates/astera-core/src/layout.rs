use std::collections::{BTreeSet, VecDeque};

use thiserror::Error;

use crate::{
    Direction, FloatingPlacement, FullscreenPlacement, Point, Rect, RestorePlacement, Size,
    TiledWindow, WindowId, WindowMode, Workspace,
};

#[derive(Clone, Debug)]
pub enum WindowTransaction {
    InsertTiled {
        id: WindowId,
        size: Size,
        anchor: Point,
        seed_direction: Direction,
    },
    MoveTiledFinished {
        id: WindowId,
        target: Point,
        seed_direction: Direction,
    },
    MoveFloating {
        id: WindowId,
        target: Rect,
        viewport_size: Size,
    },
    SetMode {
        id: WindowId,
        mode: WindowMode,
        viewport_size: Size,
    },
    Remove {
        id: WindowId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Movement {
    pub window: WindowId,
    pub from: Point,
    pub to: Point,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutDelta {
    pub generation: u64,
    pub source: Option<WindowId>,
    pub movements: Vec<Movement>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum LayoutError {
    #[error("window {0:?} already exists")]
    DuplicateWindow(WindowId),
    #[error("window {0:?} does not exist")]
    UnknownWindow(WindowId),
    #[error("window size must be positive")]
    InvalidSize,
    #[error("workspace already has fullscreen window {0:?}")]
    FullscreenOccupied(WindowId),
    #[error("layout did not converge after {0} operations")]
    DidNotConverge(usize),
    #[error("solver produced an overlapping layout")]
    UnstableResult,
}

#[derive(Clone, Debug)]
pub struct RadialSolver {
    gap: i64,
    snap_distance: i64,
    operation_limit: usize,
}

impl RadialSolver {
    pub fn new(gap: i64) -> Self {
        Self {
            gap: gap.max(0),
            snap_distance: 24,
            operation_limit: 16_384,
        }
    }

    pub fn with_operation_limit(mut self, limit: usize) -> Self {
        self.operation_limit = limit.max(1);
        self
    }

    pub fn reflow(&self, workspace: &mut Workspace) -> Result<(), LayoutError> {
        let mut working = workspace.clone();
        let windows = working.tiled.keys().copied().collect::<Vec<_>>();
        for window in windows {
            let direction = working.layout_direction_hint;
            self.solve(&mut working, window, direction)?;
        }
        if !working.tiled_windows_are_stable(self.gap) {
            return Err(LayoutError::UnstableResult);
        }
        working.generation = workspace.generation.wrapping_add(1);
        *workspace = working;
        Ok(())
    }

    pub fn with_snap_distance(mut self, distance: i64) -> Self {
        self.snap_distance = distance.max(0);
        self
    }

    pub const fn gap(&self) -> i64 {
        self.gap
    }

    /// Applies a window transaction atomically.
    pub fn apply(
        &self,
        workspace: &mut Workspace,
        transaction: WindowTransaction,
    ) -> Result<LayoutDelta, LayoutError> {
        let mut working = workspace.clone();
        let (source, seed, mut movements) = self.apply_to_working(&mut working, transaction)?;
        if let Some(source) = source {
            movements.extend(self.solve(&mut working, source, seed)?);
        }
        if !working.tiled_windows_are_stable(self.gap) {
            return Err(LayoutError::UnstableResult);
        }

        working.generation = workspace.generation.wrapping_add(1);
        let generation = working.generation;
        movements.sort_by_key(|movement| movement.window);
        movements.dedup_by_key(|movement| movement.window);
        *workspace = working;
        Ok(LayoutDelta {
            generation,
            source,
            movements,
        })
    }

    fn apply_to_working(
        &self,
        workspace: &mut Workspace,
        transaction: WindowTransaction,
    ) -> Result<(Option<WindowId>, Direction, Vec<Movement>), LayoutError> {
        match transaction {
            WindowTransaction::InsertTiled {
                id,
                size,
                anchor,
                seed_direction,
            } => {
                if !size.is_valid() {
                    return Err(LayoutError::InvalidSize);
                }
                if workspace.contains_window(id) {
                    return Err(LayoutError::DuplicateWindow(id));
                }
                workspace.tiled.insert(
                    id,
                    TiledWindow {
                        id,
                        geometry: Rect::centered_at(anchor, size),
                    },
                );
                workspace.layout_direction_hint = seed_direction.normalized();
                workspace.focus(id);
                Ok((Some(id), seed_direction, Vec::new()))
            }
            WindowTransaction::MoveTiledFinished {
                id,
                target,
                seed_direction,
            } => {
                let size = workspace
                    .tiled
                    .get(&id)
                    .ok_or(LayoutError::UnknownWindow(id))?
                    .geometry
                    .size;
                let target = self.snap_target(
                    workspace,
                    id,
                    Rect {
                        origin: target,
                        size,
                    },
                );
                let window = workspace.tiled.get_mut(&id).unwrap();
                let from = window.geometry.origin;
                window.geometry = target;
                workspace.layout_direction_hint = seed_direction.normalized();
                workspace.focus(id);
                Ok((
                    Some(id),
                    seed_direction,
                    vec![Movement {
                        window: id,
                        from,
                        to: target.origin,
                    }],
                ))
            }
            WindowTransaction::MoveFloating {
                id,
                target,
                viewport_size,
            } => {
                let placement = workspace
                    .floating
                    .get_mut(&id)
                    .ok_or(LayoutError::UnknownWindow(id))?;
                let from = placement.viewport.rect.origin;
                placement.viewport.rect = clamp_to_viewport(target, viewport_size);
                placement.viewport.normalized_center =
                    crate::NormalizedPoint::from_rect(placement.viewport.rect, viewport_size);
                let to = placement.viewport.rect.origin;
                workspace.focus(id);
                Ok((
                    None,
                    workspace.layout_direction_hint,
                    vec![Movement {
                        window: id,
                        from,
                        to,
                    }],
                ))
            }
            WindowTransaction::SetMode {
                id,
                mode,
                viewport_size,
            } => self.set_mode(workspace, id, mode, viewport_size),
            WindowTransaction::Remove { id } => {
                let removed = workspace.tiled.remove(&id).is_some()
                    || workspace.floating.remove(&id).is_some()
                    || workspace
                        .fullscreen
                        .as_ref()
                        .is_some_and(|full| full.window == id)
                        && workspace.fullscreen.take().is_some();
                if !removed {
                    return Err(LayoutError::UnknownWindow(id));
                }
                workspace.remove_focus(id);
                Ok((None, workspace.layout_direction_hint, Vec::new()))
            }
        }
    }

    fn snap_target(&self, workspace: &Workspace, id: WindowId, rect: Rect) -> Rect {
        let mut best_x: Option<(i64, WindowId, i64)> = None;
        let mut best_y: Option<(i64, WindowId, i64)> = None;
        for other in workspace.tiled.values().filter(|other| other.id != id) {
            let vertical_overlap = rect.origin.y
                < other.geometry.origin.y + other.geometry.size.height
                && rect.origin.y + rect.size.height > other.geometry.origin.y;
            if vertical_overlap {
                for candidate in [
                    other.geometry.origin.x - rect.size.width - self.gap,
                    other.geometry.origin.x + other.geometry.size.width + self.gap,
                ] {
                    let distance = (candidate - rect.origin.x).abs();
                    let value = (distance, other.id, candidate);
                    if distance <= self.snap_distance && best_x.is_none_or(|best| value < best) {
                        best_x = Some(value);
                    }
                }
            }
            let horizontal_overlap = rect.origin.x
                < other.geometry.origin.x + other.geometry.size.width
                && rect.origin.x + rect.size.width > other.geometry.origin.x;
            if horizontal_overlap {
                for candidate in [
                    other.geometry.origin.y - rect.size.height - self.gap,
                    other.geometry.origin.y + other.geometry.size.height + self.gap,
                ] {
                    let distance = (candidate - rect.origin.y).abs();
                    let value = (distance, other.id, candidate);
                    if distance <= self.snap_distance && best_y.is_none_or(|best| value < best) {
                        best_y = Some(value);
                    }
                }
            }
        }
        rect.translated(
            best_x.map_or(rect.origin.x, |(_, _, value)| value),
            best_y.map_or(rect.origin.y, |(_, _, value)| value),
        )
    }

    fn set_mode(
        &self,
        workspace: &mut Workspace,
        id: WindowId,
        target: WindowMode,
        viewport_size: Size,
    ) -> Result<(Option<WindowId>, Direction, Vec<Movement>), LayoutError> {
        let current = workspace
            .window_mode(id)
            .ok_or(LayoutError::UnknownWindow(id))?;
        if current == target {
            return Ok((None, workspace.layout_direction_hint, Vec::new()));
        }
        if target == WindowMode::Fullscreen {
            if let Some(fullscreen) = &workspace.fullscreen {
                return Err(LayoutError::FullscreenOccupied(fullscreen.window));
            }
            let restore = match current {
                WindowMode::Tiled => RestorePlacement::Tiled {
                    world_rect: workspace.tiled.remove(&id).unwrap().geometry,
                },
                WindowMode::Floating => RestorePlacement::Floating {
                    viewport: workspace.floating.remove(&id).unwrap().viewport,
                },
                WindowMode::Fullscreen => unreachable!(),
            };
            workspace.fullscreen = Some(FullscreenPlacement {
                window: id,
                restore,
            });
            workspace.focus(id);
            return Ok((None, workspace.layout_direction_hint, Vec::new()));
        }

        let (rect, geometry_mode, saved_viewport) = match current {
            WindowMode::Tiled => (
                workspace.tiled.remove(&id).unwrap().geometry,
                WindowMode::Tiled,
                None,
            ),
            WindowMode::Floating => {
                let viewport = workspace.floating.remove(&id).unwrap().viewport;
                (viewport.rect, WindowMode::Floating, Some(viewport))
            }
            WindowMode::Fullscreen => match workspace.fullscreen.take().unwrap().restore {
                RestorePlacement::Tiled { world_rect } => (world_rect, WindowMode::Tiled, None),
                RestorePlacement::Floating { viewport } => {
                    (viewport.rect, WindowMode::Floating, Some(viewport))
                }
            },
        };

        let source = match target {
            WindowMode::Tiled => {
                let world_rect = if geometry_mode == WindowMode::Floating {
                    viewport_rect_to_world(rect, workspace, viewport_size)
                } else {
                    rect
                };
                workspace.tiled.insert(
                    id,
                    TiledWindow {
                        id,
                        geometry: world_rect,
                    },
                );
                Some(id)
            }
            WindowMode::Floating => {
                let viewport_rect = if geometry_mode == WindowMode::Tiled {
                    world_rect_to_viewport(rect, workspace, viewport_size)
                } else {
                    rect
                };
                workspace.floating.insert(
                    id,
                    FloatingPlacement {
                        window: id,
                        viewport: saved_viewport.unwrap_or_else(|| {
                            crate::ViewportPlacement::new(
                                clamp_to_viewport(viewport_rect, viewport_size),
                                viewport_size,
                            )
                        }),
                    },
                );
                None
            }
            WindowMode::Fullscreen => unreachable!(),
        };
        workspace.focus(id);
        Ok((source, workspace.layout_direction_hint, Vec::new()))
    }

    fn solve(
        &self,
        workspace: &mut Workspace,
        source: WindowId,
        fallback: Direction,
    ) -> Result<Vec<Movement>, LayoutError> {
        let source_rect = workspace
            .tiled
            .get(&source)
            .ok_or(LayoutError::UnknownWindow(source))?
            .geometry;
        let mut locked = BTreeSet::from([source]);
        let mut queued = BTreeSet::new();
        let mut queue = VecDeque::new();
        let mut movements = Vec::new();
        self.enqueue_conflicts(workspace, source, &locked, &mut queued, &mut queue);

        let mut operations = 0;
        while let Some(id) = queue.pop_front() {
            queued.remove(&id);
            if locked.contains(&id) {
                continue;
            }
            operations += 1;
            if operations > self.operation_limit {
                return Err(LayoutError::DidNotConverge(operations));
            }
            let original = workspace.tiled[&id].geometry;
            let direction = Direction::between(source_rect.center(), original.center(), fallback);
            let obstacles: Vec<_> = locked
                .iter()
                .map(|locked_id| workspace.tiled[locked_id].geometry)
                .collect();
            let moved = self.first_clear_position(original, direction, &obstacles)?;
            workspace.tiled.get_mut(&id).unwrap().geometry = moved;
            locked.insert(id);
            if moved.origin != original.origin {
                movements.push(Movement {
                    window: id,
                    from: original.origin,
                    to: moved.origin,
                });
            }
            self.enqueue_conflicts(workspace, id, &locked, &mut queued, &mut queue);
        }
        Ok(movements)
    }

    fn enqueue_conflicts(
        &self,
        workspace: &Workspace,
        pivot: WindowId,
        locked: &BTreeSet<WindowId>,
        queued: &mut BTreeSet<WindowId>,
        queue: &mut VecDeque<WindowId>,
    ) {
        let pivot_rect = workspace.tiled[&pivot].geometry;
        let pivot_center = pivot_rect.center();
        let mut conflicts: Vec<_> = workspace
            .tiled
            .values()
            .filter(|window| {
                !locked.contains(&window.id)
                    && !queued.contains(&window.id)
                    && pivot_rect.conflicts(window.geometry, self.gap)
            })
            .map(|window| {
                let center = window.geometry.center();
                let dx = i128::from(center.x - pivot_center.x);
                let dy = i128::from(center.y - pivot_center.y);
                (dx * dx + dy * dy, window.id)
            })
            .collect();
        conflicts.sort_unstable();
        for (_, id) in conflicts {
            queued.insert(id);
            queue.push_back(id);
        }
    }

    fn first_clear_position(
        &self,
        rect: Rect,
        direction: Direction,
        obstacles: &[Rect],
    ) -> Result<Rect, LayoutError> {
        let clear = |candidate: Rect| {
            obstacles
                .iter()
                .all(|obstacle| !candidate.conflicts(*obstacle, self.gap))
        };
        if clear(rect) {
            return Ok(rect);
        }
        let at = |distance: f64| {
            rect.translated(
                rect.origin.x + (direction.x * distance).round() as i64,
                rect.origin.y + (direction.y * distance).round() as i64,
            )
        };
        let mut high = (rect.size.width.max(rect.size.height) + self.gap + 1) as f64;
        for _ in 0..62 {
            if clear(at(high)) {
                let mut low = 0.0;
                for _ in 0..64 {
                    let middle = (low + high) / 2.0;
                    if clear(at(middle)) {
                        high = middle;
                    } else {
                        low = middle;
                    }
                }
                let mut candidate = at(high.ceil() + 1.0);
                while !clear(candidate) {
                    high += 1.0;
                    candidate = at(high);
                }
                return Ok(candidate);
            }
            high *= 2.0;
        }
        Err(LayoutError::DidNotConverge(self.operation_limit))
    }
}

pub(crate) fn clamp_to_viewport(rect: Rect, viewport: Size) -> Rect {
    let max_x = (viewport.width - rect.size.width).max(0);
    let max_y = (viewport.height - rect.size.height).max(0);
    rect.translated(rect.origin.x.clamp(0, max_x), rect.origin.y.clamp(0, max_y))
}

fn world_rect_to_viewport(rect: Rect, workspace: &Workspace, viewport: Size) -> Rect {
    let left = workspace.camera.center.x as f64 - viewport.width as f64 / 2.0;
    let top = workspace.camera.center.y as f64 - viewport.height as f64 / 2.0;
    Rect::new(
        (rect.origin.x as f64 - left).round() as i64,
        (rect.origin.y as f64 - top).round() as i64,
        rect.size.width,
        rect.size.height,
    )
}

fn viewport_rect_to_world(rect: Rect, workspace: &Workspace, viewport: Size) -> Rect {
    let left = workspace.camera.center.x as f64 - viewport.width as f64 / 2.0;
    let top = workspace.camera.center.y as f64 - viewport.height as f64 / 2.0;
    Rect::new(
        (left + rect.origin.x as f64).round() as i64,
        (top + rect.origin.y as f64).round() as i64,
        rect.size.width,
        rect.size.height,
    )
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
