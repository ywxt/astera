use astera_core::{
    CameraPolicy, Desktop, Direction, OutputId, OutputTransform, Scale120, Size, WindowId,
    WindowMode, WorkspaceId,
};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 4;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u16,
    pub command: Command,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OutputSelector {
    Id(OutputId),
    Key(String),
    Active,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum WorkspaceSelector {
    Id(WorkspaceId),
    Name(String),
    LocalIndex {
        output: OutputSelector,
        index: usize,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    GetState,
    FocusWindow(WindowId),
    FocusDirection(Direction),
    FocusOutput(OutputSelector),
    ConfigureOutput {
        output: OutputSelector,
        physical_size: Size,
        logical_size: Size,
        native_scale: Scale120,
        transform: OutputTransform,
    },
    FocusWorkspace {
        workspace: WorkspaceSelector,
    },
    MoveWorkspace {
        workspace: WorkspaceId,
        target_output: OutputSelector,
        target_index: Option<usize>,
        activate: bool,
    },
    SetWorkspaceName {
        workspace: WorkspaceId,
        name: Option<String>,
    },
    MoveWindow {
        window: WindowId,
        target: WorkspaceSelector,
        activate: bool,
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
    pub primary_output: Option<OutputId>,
    pub outputs: Vec<OutputSnapshot>,
    pub workspaces: Vec<WorkspaceSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputSnapshot {
    pub id: OutputId,
    pub stable_key: String,
    pub active_workspace: WorkspaceId,
    pub workspaces: Vec<WorkspaceId>,
    pub physical_size: Size,
    pub logical_size: Size,
    pub native_scale: Scale120,
    pub transform: OutputTransform,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub id: WorkspaceId,
    pub name: Option<String>,
    pub original_output: Option<String>,
    pub output: Option<OutputId>,
    pub local_index: Option<usize>,
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
            primary_output: desktop.primary_output,
            outputs: desktop
                .outputs
                .values()
                .map(|set| OutputSnapshot {
                    id: set.output.id,
                    stable_key: set.output.stable_key.clone(),
                    active_workspace: set.active_workspace().expect("normalized output").id,
                    workspaces: set
                        .workspaces
                        .iter()
                        .map(|workspace| workspace.id)
                        .collect(),
                    physical_size: set.output.physical_size,
                    logical_size: set.output.logical_size,
                    native_scale: set.output.native_scale,
                    transform: set.output.transform,
                })
                .collect(),
            workspaces: desktop
                .workspaces()
                .map(|workspace| {
                    let location = desktop
                        .workspace_location(workspace.id)
                        .expect("workspace came from desktop");
                    WorkspaceSnapshot {
                        id: workspace.id,
                        name: workspace.name.clone(),
                        original_output: workspace.original_output.clone(),
                        output: location.output,
                        local_index: location.output.map(|_| location.index + 1),
                        focused_window: workspace.focused_window,
                        tiled_count: workspace.tiled.len(),
                        floating_count: workspace.floating.len(),
                        fullscreen: workspace.fullscreen.as_ref().map(|full| full.window),
                    }
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
    use astera_core::{Output, Size, WorkspaceTransaction};

    use super::*;

    #[test]
    fn local_workspace_selector_round_trips_through_ron() {
        let request = Request {
            version: PROTOCOL_VERSION,
            command: Command::FocusWorkspace {
                workspace: WorkspaceSelector::LocalIndex {
                    output: OutputSelector::Key("DP-1".into()),
                    index: 3,
                },
            },
        };
        let encoded = ron::to_string(&request).unwrap();
        let decoded: Request = ron::from_str(&encoded).unwrap();
        let Command::FocusWorkspace { workspace } = decoded.command else {
            panic!("wrong command variant");
        };
        assert_eq!(
            workspace,
            WorkspaceSelector::LocalIndex {
                output: OutputSelector::Key("DP-1".into()),
                index: 3,
            }
        );
    }

    #[test]
    fn desktop_snapshot_preserves_output_and_workspace_identity() {
        let mut desktop = Desktop::new(8);
        desktop
            .connect_output(Output::new(OutputId(7), "DP-1", Size::new(2560, 1440)))
            .unwrap();
        let workspace = desktop.active_workspace_id(OutputId(7)).unwrap();
        desktop
            .apply(WorkspaceTransaction::SetName {
                workspace,
                name: Some("code".into()),
            })
            .unwrap();

        let snapshot = DesktopSnapshot::from(&desktop).with_active_output(Some(OutputId(7)));
        assert_eq!(snapshot.active_output, Some(OutputId(7)));
        assert_eq!(snapshot.primary_output, Some(OutputId(7)));
        assert_eq!(snapshot.outputs[0].stable_key, "DP-1");
        assert_eq!(snapshot.outputs[0].active_workspace, workspace);
        let workspace = snapshot
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .unwrap();
        assert_eq!(workspace.name.as_deref(), Some("code"));
        assert_eq!(workspace.output, Some(OutputId(7)));
        assert_eq!(workspace.local_index, Some(1));
    }
}
