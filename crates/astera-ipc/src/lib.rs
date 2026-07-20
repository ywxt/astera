use astera_core::{CameraPolicy, Direction, Placement, WindowId, WorkspaceId};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u16,
    pub command: Command,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    GetState,
    Focus(WindowId),
    FocusDirection(Direction),
    SwitchWorkspace(WorkspaceId),
    SetPlacement {
        window: WindowId,
        placement: Placement,
    },
    SetCameraPolicy(CameraPolicy),
    PanViewport {
        output: u32,
        dx: i64,
        dy: i64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response<T> {
    Ok(T),
    Error { code: ErrorCode, message: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ErrorCode {
    VersionMismatch,
    InvalidCommand,
    NotFound,
    Internal,
}
