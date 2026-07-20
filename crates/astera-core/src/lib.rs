//! Compositor-independent state and layout engine for Astera.

mod desktop;
mod geometry;
mod layout;
mod model;

pub use desktop::{Desktop, DesktopError, DesktopEvent, WorkspaceTransaction};
pub use geometry::{Direction, Point, Rect, Size};
pub use layout::{LayoutDelta, LayoutError, Movement, RadialSolver, WindowTransaction};
pub use model::{
    CameraPolicy, CameraState, FloatingPlacement, FullscreenPlacement, Output, OutputId,
    OutputTransform, RestorePlacement, Scale120, TiledWindow, WindowId, WindowMode, Workspace,
    WorkspaceId,
};
