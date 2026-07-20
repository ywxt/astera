//! Compositor-independent state and layout engine for Astera.

mod geometry;
mod layout;
mod model;

pub use geometry::{Direction, Point, Rect, Size};
pub use layout::{LayoutDelta, LayoutError, Movement, RadialSolver, Transaction};
pub use model::{CameraPolicy, Placement, Window, WindowId, Workspace, WorkspaceId};
