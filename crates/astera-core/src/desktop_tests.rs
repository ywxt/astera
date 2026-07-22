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

#[test]
fn duplicate_output_identity_and_invalid_geometry_are_rejected() {
    let mut desktop = Desktop::new(8);
    desktop.connect_output(output(1, "same")).unwrap();
    assert_eq!(
        desktop.connect_output(output(2, "same")),
        Err(DesktopError::DuplicateOutputStableKey("same".into()))
    );
    let mut invalid = output(2, "B");
    invalid.logical_size = Size::new(0, 1080);
    assert_eq!(
        desktop.connect_output(invalid),
        Err(DesktopError::InvalidOutputSize)
    );
    assert_eq!(
        desktop.configure_output(
            OutputId(1),
            Size::new(1920, 1080),
            Size::new(1920, 1080),
            Scale120(0),
            OutputTransform::Normal,
        ),
        Err(DesktopError::InvalidOutputScale)
    );
    assert_eq!(desktop.outputs.len(), 1);
}

#[test]
fn same_workspace_send_still_honors_activate() {
    let mut desktop = Desktop::new(8);
    desktop.connect_output(output(1, "A")).unwrap();
    let source = desktop.active_workspace_id(OutputId(1)).unwrap();
    desktop
        .apply(WorkspaceTransaction::SetName {
            workspace: source,
            name: Some("source".into()),
        })
        .unwrap();
    let other = desktop.outputs[&OutputId(1)].workspaces.last().unwrap().id;
    desktop
        .apply(WorkspaceTransaction::SetName {
            workspace: other,
            name: Some("other".into()),
        })
        .unwrap();
    desktop
        .apply(WorkspaceTransaction::Focus {
            output: OutputId(1),
            workspace: other,
        })
        .unwrap();
    let window = WindowId(90);
    desktop
        .apply_window(
            source,
            WindowTransaction::InsertTiled {
                id: window,
                size: Size::new(100, 100),
                anchor: Point::ORIGIN,
                seed_direction: crate::Direction::RIGHT,
            },
        )
        .unwrap();
    desktop
        .apply(WorkspaceTransaction::SendWindow {
            window,
            target: source,
            activate: true,
        })
        .unwrap();
    assert_eq!(desktop.active_workspace_id(OutputId(1)), Some(source));
}

#[test]
fn output_reconfigure_and_layout_reflow_commit_atomically() {
    let mut desktop = Desktop::new(8);
    desktop.connect_output(output(1, "A")).unwrap();
    let workspace = desktop.active_workspace_id(OutputId(1)).unwrap();
    let window = WindowId(91);
    desktop
        .apply_window(
            workspace,
            WindowTransaction::InsertTiled {
                id: window,
                size: Size::new(200, 100),
                anchor: Point::new(2_000, 1_000),
                seed_direction: crate::Direction::RIGHT,
            },
        )
        .unwrap();
    assert!(desktop.output(OutputId(1)).is_some());
    desktop.output_mut(OutputId(1)).unwrap().physical_size = Size::new(2560, 1440);
    desktop
        .configure_output(
            OutputId(1),
            Size::new(2560, 1440),
            Size::new(1280, 720),
            Scale120(240),
            OutputTransform::Rotate90,
        )
        .unwrap();
    assert_eq!(
        desktop.output(OutputId(1)).unwrap().native_scale,
        Scale120(240)
    );
    desktop
        .reconfigure_layout(24, crate::CameraPolicy::Centered)
        .unwrap();
    let state = desktop.workspace(workspace).unwrap();
    assert_eq!(state.camera.policy, crate::CameraPolicy::Centered);
    assert_eq!(state.camera.center, state.tiled[&window].geometry.center());
}
