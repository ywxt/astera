#![no_main]

use astera_core::{
    Direction, Point, RadialSolver, Size, WindowId, WindowTransaction, Workspace, WorkspaceId,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let solver = RadialSolver::new(8).with_operation_limit(32_768);
    let mut workspace = Workspace::new(WorkspaceId(1));
    for (index, bytes) in data.chunks_exact(8).take(64).enumerate() {
        let x = i16::from_le_bytes([bytes[0], bytes[1]]) as i64;
        let y = i16::from_le_bytes([bytes[2], bytes[3]]) as i64;
        let width = u16::from_le_bytes([bytes[4], bytes[5]]).clamp(1, 2_000) as i64;
        let height = u16::from_le_bytes([bytes[6], bytes[7]]).clamp(1, 2_000) as i64;
        let _ = solver.apply(
            &mut workspace,
            WindowTransaction::InsertTiled {
                id: WindowId(index as u64 + 1),
                size: Size::new(width, height),
                anchor: Point::new(x, y),
                seed_direction: if bytes[0] & 1 == 0 {
                    Direction::RIGHT
                } else {
                    Direction::new(0.0, 1.0)
                },
            },
        );
        assert!(workspace.tiled_windows_are_stable(8));
    }
});
