use astera_core::{
    CameraPolicy, Desktop, Direction, OutputId, OutputTransform, Scale120, Size, WindowId,
    WindowMode, WorkspaceId,
};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u16,
    pub command: Command,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    GetState,
    FocusWindow(WindowId),
    FocusDirection(Direction),
    FocusOutput(OutputId),
    ConfigureOutput {
        output: OutputId,
        physical_size: Size,
        logical_size: Size,
        native_scale: Scale120,
        transform: OutputTransform,
    },
    BindWorkspace {
        workspace: WorkspaceId,
        output: OutputId,
    },
    MoveWorkspaceToOutput {
        workspace: WorkspaceId,
        output: OutputId,
    },
    SwapWorkspaces {
        first: WorkspaceId,
        second: WorkspaceId,
    },
    SendWindowToWorkspace {
        window: WindowId,
        target: WorkspaceId,
    },
    SetWindowMode {
        window: WindowId,
        mode: WindowMode,
    },
    SetCameraPolicy {
        workspace: WorkspaceId,
        policy: CameraPolicy,
    },
    PanCamera {
        workspace: WorkspaceId,
        dx: i64,
        dy: i64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopSnapshot {
    pub active_output: Option<OutputId>,
    pub outputs: Vec<OutputSnapshot>,
    pub workspaces: Vec<WorkspaceSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputSnapshot {
    pub id: OutputId,
    pub stable_key: String,
    pub workspace: Option<WorkspaceId>,
    pub physical_size: Size,
    pub logical_size: Size,
    pub native_scale: Scale120,
    pub transform: OutputTransform,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub id: WorkspaceId,
    pub output: Option<OutputId>,
    pub focused_window: Option<WindowId>,
    pub tiled_count: usize,
    pub floating_count: usize,
    pub fullscreen: Option<WindowId>,
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
    Conflict,
    Internal,
}

impl From<&Desktop> for DesktopSnapshot {
    fn from(desktop: &Desktop) -> Self {
        Self {
            active_output: None,
            outputs: desktop
                .outputs
                .values()
                .map(|output| OutputSnapshot {
                    id: output.id,
                    stable_key: output.stable_key.clone(),
                    workspace: output.current_workspace,
                    physical_size: output.physical_size,
                    logical_size: output.logical_size,
                    native_scale: output.native_scale,
                    transform: output.transform,
                })
                .collect(),
            workspaces: desktop
                .workspaces
                .values()
                .map(|workspace| WorkspaceSnapshot {
                    id: workspace.id,
                    output: workspace.bound_output,
                    focused_window: workspace.focused_window,
                    tiled_count: workspace.tiled.len(),
                    floating_count: workspace.floating.len(),
                    fullscreen: workspace.fullscreen.as_ref().map(|full| full.window),
                })
                .collect(),
        }
    }
}

impl DesktopSnapshot {
    pub fn with_active_output(mut self, output: Option<OutputId>) -> Self {
        self.active_output = output;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_configuration_round_trips_through_ron() {
        let request = Request {
            version: PROTOCOL_VERSION,
            command: Command::ConfigureOutput {
                output: OutputId(7),
                physical_size: Size::new(3840, 2160),
                logical_size: Size::new(2560, 1440),
                native_scale: Scale120(180),
                transform: OutputTransform::Rotate90,
            },
        };
        let encoded = ron::to_string(&request).unwrap();
        let decoded: Request = ron::from_str(&encoded).unwrap();
        let Command::ConfigureOutput {
            output,
            physical_size,
            logical_size,
            native_scale,
            transform,
        } = decoded.command
        else {
            panic!("wrong command variant");
        };
        assert_eq!(output, OutputId(7));
        assert_eq!(physical_size, Size::new(3840, 2160));
        assert_eq!(logical_size, Size::new(2560, 1440));
        assert_eq!(native_scale, Scale120(180));
        assert_eq!(transform, OutputTransform::Rotate90);
    }
}
