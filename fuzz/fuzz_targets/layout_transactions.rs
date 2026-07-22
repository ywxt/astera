#![no_main]

use astera_core::{
    Direction, Point, RadialSolver, Rect, Size, WindowId, WindowMode, WindowTransaction, Workspace,
    WorkspaceId,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let solver = RadialSolver::new(8).with_operation_limit(32_768);
    let mut workspace = Workspace::new(WorkspaceId(1));
    for (index, bytes) in data.chunks_exact(9).take(64).enumerate() {
        let x = i16::from_le_bytes([bytes[1], bytes[2]]) as i64;
        let y = i16::from_le_bytes([bytes[3], bytes[4]]) as i64;
        let width = u16::from_le_bytes([bytes[5], bytes[6]]).clamp(1, 2_000) as i64;
        let height = u16::from_le_bytes([bytes[7], bytes[8]]).clamp(1, 2_000) as i64;
        let live = workspace
            .tiled
            .keys()
            .chain(workspace.floating.keys())
            .chain(workspace.fullscreen.iter().map(|full| &full.window))
            .copied()
            .collect::<Vec<_>>();
        let selected = live
            .get(usize::from(bytes[1]) % live.len().max(1))
            .copied()
            .unwrap_or(WindowId(index as u64 + 1));
        let transaction = match bytes[0] % 5 {
            0 => WindowTransaction::InsertTiled {
                id: WindowId(index as u64 + 1),
                size: Size::new(width, height),
                anchor: Point::new(x, y),
                seed_direction: Direction::new(x as f64, y as f64),
            },
            1 => WindowTransaction::MoveTiledFinished {
                id: selected,
                target: Point::new(x, y),
                seed_direction: Direction::new(x as f64, y as f64),
            },
            2 => WindowTransaction::MoveFloating {
                id: selected,
                target: Rect::new(x, y, width, height),
                viewport_size: Size::new(1920, 1080),
            },
            3 => WindowTransaction::SetMode {
                id: selected,
                mode: match bytes[2] % 3 {
                    0 => WindowMode::Tiled,
                    1 => WindowMode::Floating,
                    _ => WindowMode::Fullscreen,
                },
                viewport_size: Size::new(1920, 1080),
            },
            _ => WindowTransaction::Remove { id: selected },
        };
        let before = workspace.clone();
        let result = solver.apply(&mut workspace, transaction);
        if result.is_err() {
            assert_eq!(workspace.tiled, before.tiled);
            assert_eq!(workspace.floating, before.floating);
            assert_eq!(workspace.fullscreen, before.fullscreen);
            assert_eq!(workspace.generation, before.generation);
        }
        assert!(workspace.tiled_windows_are_stable(8));
    }
});
