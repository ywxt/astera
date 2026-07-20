use std::collections::{BTreeSet, VecDeque};

use thiserror::Error;

use crate::{Direction, Placement, Point, Rect, Size, Window, WindowId, Workspace};

#[derive(Clone, Debug)]
pub enum Transaction {
    Insert {
        id: WindowId,
        size: Size,
        anchor: Point,
        seed_direction: Direction,
    },
    MoveFinished {
        id: WindowId,
        target: Point,
        seed_direction: Direction,
    },
    SetPlacement {
        id: WindowId,
        placement: Placement,
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
    #[error("layout did not converge after {0} operations")]
    DidNotConverge(usize),
    #[error("solver produced an overlapping layout")]
    UnstableResult,
}

#[derive(Clone, Debug)]
pub struct RadialSolver {
    gap: i64,
    operation_limit: usize,
}

impl RadialSolver {
    pub fn new(gap: i64) -> Self {
        Self {
            gap: gap.max(0),
            operation_limit: 16_384,
        }
    }

    pub fn with_operation_limit(mut self, limit: usize) -> Self {
        self.operation_limit = limit.max(1);
        self
    }

    pub const fn gap(&self) -> i64 {
        self.gap
    }

    /// Applies a transaction atomically. The workspace is unchanged on failure.
    pub fn apply(
        &self,
        workspace: &mut Workspace,
        transaction: Transaction,
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
        transaction: Transaction,
    ) -> Result<(Option<WindowId>, Direction, Vec<Movement>), LayoutError> {
        match transaction {
            Transaction::Insert {
                id,
                size,
                anchor,
                seed_direction,
            } => {
                if !size.is_valid() {
                    return Err(LayoutError::InvalidSize);
                }
                if workspace.windows.contains_key(&id) {
                    return Err(LayoutError::DuplicateWindow(id));
                }
                workspace
                    .windows
                    .insert(id, Window::tiled(id, Rect::centered_at(anchor, size)));
                workspace.focus = Some(id);
                workspace.focus_direction = seed_direction.normalized();
                Ok((Some(id), seed_direction, Vec::new()))
            }
            Transaction::MoveFinished {
                id,
                target,
                seed_direction,
            } => {
                let window = workspace
                    .windows
                    .get_mut(&id)
                    .ok_or(LayoutError::UnknownWindow(id))?;
                let from = window.geometry.origin;
                window.geometry.origin = target;
                workspace.focus = Some(id);
                workspace.focus_direction = seed_direction.normalized();
                let movement = Movement {
                    window: id,
                    from,
                    to: target,
                };
                let source = (window.placement == Placement::Tiled).then_some(id);
                Ok((source, seed_direction, vec![movement]))
            }
            Transaction::SetPlacement { id, placement } => {
                let window = workspace
                    .windows
                    .get_mut(&id)
                    .ok_or(LayoutError::UnknownWindow(id))?;
                window.placement = placement;
                let source = (placement == Placement::Tiled).then_some(id);
                Ok((source, workspace.focus_direction, Vec::new()))
            }
            Transaction::Remove { id } => {
                workspace
                    .windows
                    .remove(&id)
                    .ok_or(LayoutError::UnknownWindow(id))?;
                if workspace.focus == Some(id) {
                    workspace.focus = workspace.windows.keys().next_back().copied();
                }
                Ok((None, workspace.focus_direction, Vec::new()))
            }
        }
    }

    fn solve(
        &self,
        workspace: &mut Workspace,
        source: WindowId,
        fallback: Direction,
    ) -> Result<Vec<Movement>, LayoutError> {
        let source_rect = workspace
            .windows
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

            let original = workspace.windows[&id].geometry;
            let direction = Direction::between(source_rect.center(), original.center(), fallback);
            let obstacles: Vec<_> = locked
                .iter()
                .map(|locked_id| workspace.windows[locked_id].geometry)
                .collect();
            let moved = self.first_clear_position(original, direction, &obstacles)?;
            workspace
                .windows
                .get_mut(&id)
                .expect("queued window exists")
                .geometry = moved;
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
        let pivot_rect = workspace.windows[&pivot].geometry;
        let pivot_center = pivot_rect.center();
        let mut conflicts: Vec<_> = workspace
            .windows
            .values()
            .filter(|window| {
                window.placement == Placement::Tiled
                    && !locked.contains(&window.id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorkspaceId;

    fn insert(
        solver: &RadialSolver,
        workspace: &mut Workspace,
        id: u64,
        size: Size,
        anchor: Point,
        direction: Direction,
    ) -> LayoutDelta {
        solver
            .apply(
                workspace,
                Transaction::Insert {
                    id: WindowId(id),
                    size,
                    anchor,
                    seed_direction: direction,
                },
            )
            .unwrap()
    }

    #[test]
    fn first_window_is_centered_on_world_origin() {
        let solver = RadialSolver::new(8);
        let mut workspace = Workspace::new(WorkspaceId(1));
        insert(
            &solver,
            &mut workspace,
            1,
            Size::new(100, 80),
            Point::ORIGIN,
            Direction::RIGHT,
        );
        assert_eq!(
            workspace.window(WindowId(1)).unwrap().geometry,
            Rect::new(-50, -40, 100, 80)
        );
    }

    #[test]
    fn same_center_pushes_old_window_in_seed_direction() {
        let solver = RadialSolver::new(8);
        let mut workspace = Workspace::new(WorkspaceId(1));
        insert(
            &solver,
            &mut workspace,
            1,
            Size::new(100, 80),
            Point::ORIGIN,
            Direction::RIGHT,
        );
        insert(
            &solver,
            &mut workspace,
            2,
            Size::new(100, 80),
            Point::ORIGIN,
            Direction::RIGHT,
        );

        assert_eq!(
            workspace.window(WindowId(2)).unwrap().geometry.center(),
            Point::ORIGIN
        );
        assert!(workspace.window(WindowId(1)).unwrap().geometry.origin.x > 50);
        assert!(workspace.tiled_windows_are_stable(8));
    }

    #[test]
    fn collision_propagates_through_a_chain() {
        let solver = RadialSolver::new(8);
        let mut workspace = Workspace::new(WorkspaceId(1));
        for (id, x) in [(1, 0), (2, 108), (3, 216)] {
            workspace.windows.insert(
                WindowId(id),
                Window::tiled(WindowId(id), Rect::new(x, -40, 100, 80)),
            );
        }
        workspace.focus = Some(WindowId(1));

        let delta = insert(
            &solver,
            &mut workspace,
            4,
            Size::new(100, 80),
            Point::new(50, 0),
            Direction::RIGHT,
        );

        assert_eq!(delta.movements.len(), 3);
        assert!(workspace.tiled_windows_are_stable(8));
    }

    #[test]
    fn floating_windows_do_not_participate() {
        let solver = RadialSolver::new(8);
        let mut workspace = Workspace::new(WorkspaceId(1));
        let mut floating = Window::tiled(WindowId(1), Rect::new(-50, -40, 100, 80));
        floating.placement = Placement::Floating;
        workspace.windows.insert(floating.id, floating.clone());

        insert(
            &solver,
            &mut workspace,
            2,
            Size::new(100, 80),
            Point::ORIGIN,
            Direction::RIGHT,
        );
        assert_eq!(workspace.window(WindowId(1)).unwrap(), &floating);
    }

    #[test]
    fn failed_transaction_is_rolled_back() {
        let solver = RadialSolver::new(8);
        let mut workspace = Workspace::new(WorkspaceId(1));
        let before = workspace.clone();
        let error = solver
            .apply(
                &mut workspace,
                Transaction::Insert {
                    id: WindowId(1),
                    size: Size::new(0, 80),
                    anchor: Point::ORIGIN,
                    seed_direction: Direction::RIGHT,
                },
            )
            .unwrap_err();
        assert_eq!(error, LayoutError::InvalidSize);
        assert_eq!(workspace.windows, before.windows);
        assert_eq!(workspace.generation, before.generation);
    }
}
