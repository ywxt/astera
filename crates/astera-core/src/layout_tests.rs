use super::*;
use crate::WorkspaceId;
use proptest::prelude::*;

fn insert(solver: &RadialSolver, workspace: &mut Workspace, id: u64, anchor: Point) {
    solver
        .apply(
            workspace,
            WindowTransaction::InsertTiled {
                id: WindowId(id),
                size: Size::new(100, 80),
                anchor,
                seed_direction: Direction::RIGHT,
            },
        )
        .unwrap();
}

#[test]
fn same_center_pushes_old_window_in_seed_direction() {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    insert(&solver, &mut workspace, 1, Point::ORIGIN);
    insert(&solver, &mut workspace, 2, Point::ORIGIN);
    assert_eq!(
        workspace.tiled[&WindowId(2)].geometry.center(),
        Point::ORIGIN
    );
    assert!(workspace.tiled[&WindowId(1)].geometry.origin.x > 50);
    assert!(workspace.tiled_windows_are_stable(8));
}

#[test]
fn center_insertion_pushes_surrounding_windows_outward() {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    insert(&solver, &mut workspace, 1, Point::new(-40, 0));
    insert(&solver, &mut workspace, 2, Point::new(40, 0));

    let before_left = workspace.tiled[&WindowId(1)].geometry.center();
    let before_right = workspace.tiled[&WindowId(2)].geometry.center();
    insert(&solver, &mut workspace, 3, Point::ORIGIN);

    let left = workspace.tiled[&WindowId(1)].geometry.center();
    let right = workspace.tiled[&WindowId(2)].geometry.center();
    assert!(left.x < before_left.x);
    assert!(right.x > before_right.x);
    assert_eq!(
        workspace.tiled[&WindowId(3)].geometry.center(),
        Point::ORIGIN
    );
    assert!(workspace.tiled_windows_are_stable(8));
}

#[test]
fn floating_and_fullscreen_never_participate_in_solver() {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    insert(&solver, &mut workspace, 1, Point::ORIGIN);
    solver
        .apply(
            &mut workspace,
            WindowTransaction::SetMode {
                id: WindowId(1),
                mode: WindowMode::Floating,
                viewport_size: Size::new(1920, 1080),
            },
        )
        .unwrap();
    insert(&solver, &mut workspace, 2, Point::ORIGIN);
    solver
        .apply(
            &mut workspace,
            WindowTransaction::SetMode {
                id: WindowId(1),
                mode: WindowMode::Fullscreen,
                viewport_size: Size::new(1920, 1080),
            },
        )
        .unwrap();
    assert!(workspace.tiled_windows_are_stable(8));
    assert_eq!(workspace.fullscreen.as_ref().unwrap().window, WindowId(1));
}

#[test]
fn failed_transaction_rolls_back() {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    let before = workspace.clone();
    assert_eq!(
        solver.apply(
            &mut workspace,
            WindowTransaction::InsertTiled {
                id: WindowId(1),
                size: Size::new(0, 80),
                anchor: Point::ORIGIN,
                seed_direction: Direction::RIGHT,
            },
        ),
        Err(LayoutError::InvalidSize)
    );
    assert_eq!(workspace.tiled, before.tiled);
    assert_eq!(workspace.generation, before.generation);
}

#[test]
fn fullscreen_restores_original_floating_geometry() {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    insert(&solver, &mut workspace, 1, Point::ORIGIN);
    let viewport = Size::new(1920, 1080);
    solver
        .apply(
            &mut workspace,
            WindowTransaction::SetMode {
                id: WindowId(1),
                mode: WindowMode::Floating,
                viewport_size: viewport,
            },
        )
        .unwrap();
    let floating_rect = workspace.floating[&WindowId(1)].viewport.rect;
    solver
        .apply(
            &mut workspace,
            WindowTransaction::SetMode {
                id: WindowId(1),
                mode: WindowMode::Fullscreen,
                viewport_size: viewport,
            },
        )
        .unwrap();
    solver
        .apply(
            &mut workspace,
            WindowTransaction::SetMode {
                id: WindowId(1),
                mode: WindowMode::Floating,
                viewport_size: viewport,
            },
        )
        .unwrap();
    assert_eq!(
        workspace.floating[&WindowId(1)].viewport.rect,
        floating_rect
    );
}

#[test]
fn maximized_and_fullscreen_restore_stack_is_finite() {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    insert(&solver, &mut workspace, 1, Point::ORIGIN);
    let original = workspace.tiled[&WindowId(1)].geometry;

    for mode in [
        WindowMode::Maximized,
        WindowMode::Fullscreen,
        WindowMode::Maximized,
        WindowMode::Tiled,
    ] {
        solver
            .apply(
                &mut workspace,
                WindowTransaction::SetMode {
                    id: WindowId(1),
                    mode,
                    viewport_size: Size::new(1920, 1080),
                },
            )
            .unwrap();
        assert_eq!(workspace.window_mode(WindowId(1)), Some(mode));
    }
    assert_eq!(workspace.tiled[&WindowId(1)].geometry, original);
    assert!(workspace.maximized.is_none());
    assert!(workspace.fullscreen.is_none());
}

#[test]
fn maximized_is_excluded_from_solver_and_occupancy_conflicts_are_atomic() {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    insert(&solver, &mut workspace, 1, Point::ORIGIN);
    insert(&solver, &mut workspace, 2, Point::new(400, 0));
    solver
        .apply(
            &mut workspace,
            WindowTransaction::SetMode {
                id: WindowId(1),
                mode: WindowMode::Maximized,
                viewport_size: Size::new(800, 600),
            },
        )
        .unwrap();
    let before = workspace.clone();
    assert_eq!(
        solver.apply(
            &mut workspace,
            WindowTransaction::SetMode {
                id: WindowId(2),
                mode: WindowMode::Maximized,
                viewport_size: Size::new(800, 600),
            },
        ),
        Err(LayoutError::MaximizedOccupied(WindowId(1)))
    );
    assert_eq!(workspace.maximized, before.maximized);
    assert_eq!(workspace.tiled, before.tiled);
    assert!(workspace.tiled_windows_are_stable(8));
}

#[test]
fn removing_focus_restores_most_recent_live_window() {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    insert(&solver, &mut workspace, 1, Point::ORIGIN);
    insert(&solver, &mut workspace, 2, Point::new(300, 0));
    assert_eq!(workspace.focused_window, Some(WindowId(2)));
    solver
        .apply(
            &mut workspace,
            WindowTransaction::Remove { id: WindowId(2) },
        )
        .unwrap();
    assert_eq!(workspace.focused_window, Some(WindowId(1)));
}

#[test]
fn finished_tiled_drag_snaps_to_nearby_edge_before_solving() {
    let solver = RadialSolver::new(8).with_snap_distance(24);
    let mut workspace = Workspace::new(WorkspaceId(1));
    insert(&solver, &mut workspace, 1, Point::new(50, 40));
    insert(&solver, &mut workspace, 2, Point::new(400, 40));
    solver
        .apply(
            &mut workspace,
            WindowTransaction::MoveTiledFinished {
                id: WindowId(2),
                target: Point::new(112, 0),
                seed_direction: Direction::RIGHT,
            },
        )
        .unwrap();
    assert_eq!(workspace.tiled[&WindowId(2)].geometry.origin.x, 108);
    assert!(workspace.tiled_windows_are_stable(8));
}

#[test]
fn duplicate_unknown_and_fullscreen_conflicts_are_atomic() {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    insert(&solver, &mut workspace, 1, Point::ORIGIN);
    insert(&solver, &mut workspace, 2, Point::new(300, 0));
    solver
        .apply(
            &mut workspace,
            WindowTransaction::SetMode {
                id: WindowId(1),
                mode: WindowMode::Fullscreen,
                viewport_size: Size::new(800, 600),
            },
        )
        .unwrap();
    let before = workspace.clone();

    assert_eq!(
        solver.apply(
            &mut workspace,
            WindowTransaction::SetMode {
                id: WindowId(2),
                mode: WindowMode::Fullscreen,
                viewport_size: Size::new(800, 600),
            },
        ),
        Err(LayoutError::FullscreenOccupied(WindowId(1)))
    );
    assert_eq!(workspace.tiled, before.tiled);
    assert_eq!(workspace.fullscreen, before.fullscreen);
    assert_eq!(workspace.generation, before.generation);
    assert_eq!(
        solver.apply(
            &mut workspace,
            WindowTransaction::Remove { id: WindowId(99) }
        ),
        Err(LayoutError::UnknownWindow(WindowId(99)))
    );
    assert_eq!(
        solver.apply(
            &mut workspace,
            WindowTransaction::InsertTiled {
                id: WindowId(2),
                size: Size::new(10, 10),
                anchor: Point::ORIGIN,
                seed_direction: Direction::RIGHT,
            },
        ),
        Err(LayoutError::DuplicateWindow(WindowId(2)))
    );
}

#[test]
fn tiled_floating_round_trip_uses_camera_transform_and_clamps() {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    workspace.camera.center = Point::new(1_000, 500);
    let viewport = Size::new(800, 600);
    insert(&solver, &mut workspace, 1, Point::new(1_000, 500));
    let world = workspace.tiled[&WindowId(1)].geometry;

    solver
        .apply(
            &mut workspace,
            WindowTransaction::SetMode {
                id: WindowId(1),
                mode: WindowMode::Floating,
                viewport_size: viewport,
            },
        )
        .unwrap();
    assert_eq!(
        workspace.floating[&WindowId(1)].viewport.rect.center(),
        Point::new(400, 300)
    );
    solver
        .apply(
            &mut workspace,
            WindowTransaction::MoveFloating {
                id: WindowId(1),
                target: Rect::new(-500, 900, 100, 80),
                viewport_size: viewport,
            },
        )
        .unwrap();
    assert_eq!(
        workspace.floating[&WindowId(1)].viewport.rect.origin,
        Point::new(0, 520)
    );
    solver
        .apply(
            &mut workspace,
            WindowTransaction::SetMode {
                id: WindowId(1),
                mode: WindowMode::Tiled,
                viewport_size: viewport,
            },
        )
        .unwrap();
    assert_ne!(workspace.tiled[&WindowId(1)].geometry, world);
    assert_eq!(workspace.window_mode(WindowId(1)), Some(WindowMode::Tiled));
}

#[test]
fn reflow_repairs_manual_overlap_and_updates_generation() {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    for id in 1..=3 {
        workspace.tiled.insert(
            WindowId(id),
            TiledWindow {
                id: WindowId(id),
                geometry: Rect::new(0, 0, 100, 80),
            },
        );
    }
    let generation = workspace.generation;
    solver.reflow(&mut workspace).unwrap();
    assert!(workspace.tiled_windows_are_stable(8));
    assert_eq!(workspace.generation, generation + 1);
}

#[test]
fn failed_reflow_is_atomic() {
    let solver = RadialSolver::new(8).with_operation_limit(1);
    let mut workspace = Workspace::new(WorkspaceId(1));
    for id in 1..=4 {
        workspace.tiled.insert(
            WindowId(id),
            TiledWindow {
                id: WindowId(id),
                geometry: Rect::new(0, 0, 100, 80),
            },
        );
    }
    let before = workspace.clone();
    assert!(matches!(
        solver.reflow(&mut workspace),
        Err(LayoutError::DidNotConverge(_))
    ));
    assert_eq!(workspace.tiled, before.tiled);
    assert_eq!(workspace.generation, before.generation);
}

fn apply_fixture(fixture: &[(i16, i16, u16, u16)]) -> Result<Workspace, LayoutError> {
    let solver = RadialSolver::new(8);
    let mut workspace = Workspace::new(WorkspaceId(1));
    for (index, &(x, y, width, height)) in fixture.iter().enumerate() {
        solver.apply(
            &mut workspace,
            WindowTransaction::InsertTiled {
                id: WindowId(index as u64 + 1),
                size: Size::new(i64::from(width), i64::from(height)),
                anchor: Point::new(i64::from(x), i64::from(y)),
                seed_direction: if index % 2 == 0 {
                    Direction::RIGHT
                } else {
                    Direction::new(0.0, 1.0)
                },
            },
        )?;
    }
    Ok(workspace)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        max_shrink_iters: 4096,
        ..ProptestConfig::default()
    })]

    #[test]
    fn arbitrary_insertions_are_stable_and_deterministic(
        fixture in prop::collection::vec(
            (-2_000_i16..=2_000, -2_000_i16..=2_000, 1_u16..=600, 1_u16..=400),
            0..20,
        )
    ) {
        let first = apply_fixture(&fixture);
        let second = apply_fixture(&fixture);
        prop_assert!(first.is_ok(), "first solve failed: {first:?}");
        prop_assert!(second.is_ok(), "second solve failed: {second:?}");
        let first = first.unwrap();
        let second = second.unwrap();
        prop_assert!(first.tiled_windows_are_stable(8));
        prop_assert_eq!(first.tiled, second.tiled);
        prop_assert_eq!(first.generation, fixture.len() as u64);
    }

    #[test]
    fn failed_insert_is_atomic(
        x in -10_000_i64..=10_000,
        y in -10_000_i64..=10_000,
        invalid_width in -100_i64..=0,
    ) {
        let solver = RadialSolver::new(8);
        let mut workspace = Workspace::new(WorkspaceId(1));
        insert(&solver, &mut workspace, 1, Point::new(x, y));
        let before = workspace.clone();
        let result = solver.apply(
            &mut workspace,
            WindowTransaction::InsertTiled {
                id: WindowId(2),
                size: Size::new(invalid_width, 100),
                anchor: Point::new(x, y),
                seed_direction: Direction::new(-1.0, 0.0),
            },
        );
        prop_assert_eq!(result, Err(LayoutError::InvalidSize));
        prop_assert_eq!(workspace.tiled, before.tiled);
        prop_assert_eq!(workspace.generation, before.generation);
    }
}
