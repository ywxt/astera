use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

pub const BOOTSTRAP_VERSION: u16 = 0;
pub const CURRENT_VERSION: u16 = 1;
pub const MIN_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Frame<'a> {
    pub version: u16,
    pub payload: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum FramingError {
    #[error("IPC frame must be `<version> <RON>\\n`")]
    InvalidFrame,
    #[error("IPC frame version {actual} does not match expected version {expected}")]
    VersionMismatch { expected: u16, actual: u16 },
    #[error("invalid RON payload: {0}")]
    InvalidPayload(String),
}

/// Split one complete line without assigning a schema to its version.
pub fn parse_frame(frame: &str) -> Result<Frame<'_>, FramingError> {
    let line = frame.strip_suffix('\n').ok_or(FramingError::InvalidFrame)?;
    if line.contains(['\r', '\n']) {
        return Err(FramingError::InvalidFrame);
    }
    let (version, payload) = line.split_once(' ').ok_or(FramingError::InvalidFrame)?;
    if payload.trim().is_empty() || payload != payload.trim() {
        return Err(FramingError::InvalidFrame);
    }
    let version = version
        .parse::<u16>()
        .map_err(|_| FramingError::InvalidFrame)?;
    Ok(Frame { version, payload })
}

pub fn decode_payload<T: DeserializeOwned>(frame: Frame<'_>) -> Result<T, FramingError> {
    ron::from_str(frame.payload).map_err(|error| FramingError::InvalidPayload(error.to_string()))
}

pub fn decode_frame<T: DeserializeOwned>(
    frame: &str,
    expected_version: u16,
) -> Result<T, FramingError> {
    let frame = parse_frame(frame)?;
    if frame.version != expected_version {
        return Err(FramingError::VersionMismatch {
            expected: expected_version,
            actual: frame.version,
        });
    }
    decode_payload(frame)
}

pub fn encode_frame<T: Serialize>(version: u16, value: &T) -> Result<String, ron::Error> {
    Ok(format!("{version} {}\n", ron::to_string(value)?))
}

#[derive(Clone, Debug, PartialEq)]
pub enum VersionedRequest {
    V1(wire::v1::Request),
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum RequestDecodeError {
    #[error(transparent)]
    Framing(#[from] FramingError),
    #[error("IPC protocol version {requested} is unsupported (supported {minimum}..={current})")]
    UnsupportedVersion {
        requested: u16,
        minimum: u16,
        current: u16,
    },
}

pub fn decode_request(frame: &str) -> Result<VersionedRequest, RequestDecodeError> {
    let frame = parse_frame(frame)?;
    match frame.version {
        1 => Ok(VersionedRequest::V1(decode_payload(frame)?)),
        requested => Err(RequestDecodeError::UnsupportedVersion {
            requested,
            minimum: MIN_VERSION,
            current: CURRENT_VERSION,
        }),
    }
}

pub mod wire {
    /// Version zero is the permanently frozen bootstrap error/negotiation schema. It is never a
    /// compositor command schema, so an unknown command version can always receive a useful reply.
    pub mod v0 {
        use serde::{Deserialize, Serialize};

        #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum Request {
            Versions,
        }

        #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum Response {
            Versions {
                minimum: u16,
                current: u16,
            },
            UnsupportedVersion {
                requested: u16,
                minimum: u16,
                current: u16,
            },
            InvalidFrame {
                message: String,
            },
            InvalidRequest {
                message: String,
            },
        }
    }

    pub mod v1 {
        use astera_core::Desktop;
        use serde::{Deserialize, Serialize};

        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        pub struct WindowId(pub u64);
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        pub struct WorkspaceId(pub u32);
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        pub struct OutputId(pub u32);
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
        pub struct Point {
            pub x: i64,
            pub y: i64,
        }
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
        pub struct Size {
            pub width: i64,
            pub height: i64,
        }
        impl Size {
            pub const fn new(width: i64, height: i64) -> Self {
                Self { width, height }
            }
        }
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
        pub struct Rect {
            pub origin: Point,
            pub size: Size,
        }
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub struct Scale120(pub u32);
        #[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
        pub struct Direction {
            pub x: f64,
            pub y: f64,
        }
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum CameraPolicy {
            Centered,
            KeepVisible { margin: i64 },
        }
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum WindowMode {
            Tiled,
            Floating,
            Maximized,
            Fullscreen,
        }
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum OutputTransform {
            Normal,
            Rotate90,
            Rotate180,
            Rotate270,
            Flipped,
        }

        impl Rect {
            pub const fn new(x: i64, y: i64, width: i64, height: i64) -> Self {
                Self {
                    origin: Point { x, y },
                    size: Size { width, height },
                }
            }
        }
        impl From<astera_core::WindowId> for WindowId {
            fn from(value: astera_core::WindowId) -> Self {
                Self(value.0)
            }
        }
        impl From<astera_core::WorkspaceId> for WorkspaceId {
            fn from(value: astera_core::WorkspaceId) -> Self {
                Self(value.0)
            }
        }
        impl From<astera_core::OutputId> for OutputId {
            fn from(value: astera_core::OutputId) -> Self {
                Self(value.0)
            }
        }
        impl From<astera_core::Point> for Point {
            fn from(value: astera_core::Point) -> Self {
                Self {
                    x: value.x,
                    y: value.y,
                }
            }
        }
        impl From<astera_core::Size> for Size {
            fn from(value: astera_core::Size) -> Self {
                Self {
                    width: value.width,
                    height: value.height,
                }
            }
        }
        impl From<astera_core::Rect> for Rect {
            fn from(value: astera_core::Rect) -> Self {
                Self {
                    origin: value.origin.into(),
                    size: value.size.into(),
                }
            }
        }
        impl From<astera_core::Scale120> for Scale120 {
            fn from(value: astera_core::Scale120) -> Self {
                Self(value.0)
            }
        }
        impl From<astera_core::Direction> for Direction {
            fn from(value: astera_core::Direction) -> Self {
                Self {
                    x: value.x,
                    y: value.y,
                }
            }
        }
        impl From<astera_core::CameraPolicy> for CameraPolicy {
            fn from(value: astera_core::CameraPolicy) -> Self {
                match value {
                    astera_core::CameraPolicy::Centered => Self::Centered,
                    astera_core::CameraPolicy::KeepVisible { margin } => {
                        Self::KeepVisible { margin }
                    }
                }
            }
        }
        impl From<astera_core::WindowMode> for WindowMode {
            fn from(value: astera_core::WindowMode) -> Self {
                match value {
                    astera_core::WindowMode::Tiled => Self::Tiled,
                    astera_core::WindowMode::Floating => Self::Floating,
                    astera_core::WindowMode::Maximized => Self::Maximized,
                    astera_core::WindowMode::Fullscreen => Self::Fullscreen,
                }
            }
        }
        impl From<astera_core::OutputTransform> for OutputTransform {
            fn from(value: astera_core::OutputTransform) -> Self {
                match value {
                    astera_core::OutputTransform::Normal => Self::Normal,
                    astera_core::OutputTransform::Rotate90 => Self::Rotate90,
                    astera_core::OutputTransform::Rotate180 => Self::Rotate180,
                    astera_core::OutputTransform::Rotate270 => Self::Rotate270,
                    astera_core::OutputTransform::Flipped => Self::Flipped,
                }
            }
        }
        impl From<WindowId> for astera_core::WindowId {
            fn from(value: WindowId) -> Self {
                Self(value.0)
            }
        }
        impl From<WorkspaceId> for astera_core::WorkspaceId {
            fn from(value: WorkspaceId) -> Self {
                Self(value.0)
            }
        }
        impl From<OutputId> for astera_core::OutputId {
            fn from(value: OutputId) -> Self {
                Self(value.0)
            }
        }
        impl From<Size> for astera_core::Size {
            fn from(value: Size) -> Self {
                Self::new(value.width, value.height)
            }
        }
        impl From<Direction> for astera_core::Direction {
            fn from(value: Direction) -> Self {
                Self::new(value.x, value.y)
            }
        }
        impl From<Scale120> for astera_core::Scale120 {
            fn from(value: Scale120) -> Self {
                Self(value.0)
            }
        }
        impl From<CameraPolicy> for astera_core::CameraPolicy {
            fn from(value: CameraPolicy) -> Self {
                match value {
                    CameraPolicy::Centered => Self::Centered,
                    CameraPolicy::KeepVisible { margin } => Self::KeepVisible { margin },
                }
            }
        }
        impl From<WindowMode> for astera_core::WindowMode {
            fn from(value: WindowMode) -> Self {
                match value {
                    WindowMode::Tiled => Self::Tiled,
                    WindowMode::Floating => Self::Floating,
                    WindowMode::Maximized => Self::Maximized,
                    WindowMode::Fullscreen => Self::Fullscreen,
                }
            }
        }
        impl From<OutputTransform> for astera_core::OutputTransform {
            fn from(value: OutputTransform) -> Self {
                match value {
                    OutputTransform::Normal => Self::Normal,
                    OutputTransform::Rotate90 => Self::Rotate90,
                    OutputTransform::Rotate180 => Self::Rotate180,
                    OutputTransform::Rotate270 => Self::Rotate270,
                    OutputTransform::Flipped => Self::Flipped,
                }
            }
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct Request {
            pub kind: RequestKind,
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub enum RequestKind {
            Command(Command),
            EventStream,
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
            LocalIndex { output: OutputSelector, index: u32 },
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
                target_index: Option<u32>,
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

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub enum Success {
            State {
                sequence: u64,
                snapshot: DesktopSnapshot,
            },
            Handled {
                sequence: u64,
            },
            EventStream {
                sequence: u64,
                snapshot: DesktopSnapshot,
            },
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct Error {
            pub code: ErrorCode,
            pub message: String,
            pub sequence: u64,
        }

        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum ErrorCode {
            InvalidRequest,
            NotFound,
            Conflict,
            Internal,
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub enum Response {
            Success(Success),
            Error(Error),
        }

        #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
        pub struct DesktopSnapshot {
            pub active_output: Option<OutputId>,
            pub primary_output: Option<OutputId>,
            pub focused_window: Option<WindowId>,
            pub outputs: Vec<OutputSnapshot>,
            pub layers: Vec<LayerSnapshot>,
            pub workspaces: Vec<WorkspaceSnapshot>,
            pub cameras: Vec<CameraSnapshot>,
            pub windows: Vec<WindowSnapshot>,
            pub config: ConfigSnapshot,
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct OutputSnapshot {
            pub id: OutputId,
            pub stable_key: String,
            pub active_workspace: WorkspaceId,
            pub workspaces: Vec<WorkspaceId>,
            pub physical_size: Size,
            pub logical_size: Size,
            pub native_scale: Scale120,
            pub transform: OutputTransform,
            pub viewport: Rect,
            pub usable_area: Rect,
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct LayerSnapshot {
            pub id: u64,
            pub output: OutputId,
            pub namespace: String,
            pub layer: Layer,
            pub anchor: Anchor,
            pub exclusive_zone: i32,
            pub exclusive_contribution: ExclusiveContribution,
            pub keyboard_interactivity: KeyboardInteractivity,
            pub geometry: Rect,
        }

        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum Layer {
            Background,
            Bottom,
            Top,
            Overlay,
        }

        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
        pub struct Anchor {
            pub top: bool,
            pub bottom: bool,
            pub left: bool,
            pub right: bool,
        }

        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
        pub struct ExclusiveContribution {
            pub top: i64,
            pub right: i64,
            pub bottom: i64,
            pub left: i64,
        }

        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        pub enum KeyboardInteractivity {
            None,
            Exclusive,
            OnDemand,
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct WorkspaceSnapshot {
            pub id: WorkspaceId,
            pub name: Option<String>,
            pub original_output: Option<String>,
            pub output: Option<OutputId>,
            pub local_index: Option<u32>,
            pub active_window: Option<WindowId>,
            pub tiled_count: u64,
            pub floating_count: u64,
            pub fullscreen: Option<WindowId>,
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct CameraSnapshot {
            pub workspace: WorkspaceId,
            pub center: Point,
            pub policy: CameraPolicy,
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct WindowSnapshot {
            pub id: WindowId,
            pub workspace: WorkspaceId,
            pub mode: WindowMode,
            pub metadata: WindowMetadata,
            pub placement: WindowPlacement,
            pub visible_geometry: Option<Rect>,
        }

        #[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
        pub struct WindowMetadata {
            pub title: Option<String>,
            pub app_id: Option<String>,
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub enum WindowPlacement {
            Tiled { world_geometry: Rect },
            Floating { viewport_geometry: Rect },
            Maximized { restore: BaseRestore },
            Fullscreen { restore: FullscreenRestore },
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub enum BaseRestore {
            Tiled { world_geometry: Rect },
            Floating { viewport_geometry: Rect },
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub enum FullscreenRestore {
            Tiled { world_geometry: Rect },
            Floating { viewport_geometry: Rect },
            Maximized { restore: BaseRestore },
        }

        #[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
        pub struct ConfigSnapshot {
            pub source: Option<String>,
            pub generation: u64,
            pub failed: bool,
            pub error: Option<String>,
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct EventEnvelope {
            pub sequence: u64,
            pub event: Event,
        }

        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub enum Event {
            OutputOpened {
                output: OutputSnapshot,
            },
            OutputChanged {
                output: OutputSnapshot,
            },
            OutputClosed {
                output: OutputId,
            },
            LayerOpened {
                layer: LayerSnapshot,
            },
            LayerChanged {
                layer: LayerSnapshot,
            },
            LayerClosed {
                layer: u64,
            },
            WorkspaceOpened {
                workspace: WorkspaceSnapshot,
            },
            WorkspaceChanged {
                workspace: WorkspaceSnapshot,
            },
            WorkspaceClosed {
                workspace: WorkspaceId,
            },
            WindowOpened {
                window: WindowSnapshot,
            },
            WindowChanged {
                window: WindowSnapshot,
            },
            WindowClosed {
                window: WindowId,
            },
            WorkspaceActivated {
                output: OutputId,
                workspace: WorkspaceId,
                focused: bool,
            },
            WorkspaceActiveWindowChanged {
                workspace: WorkspaceId,
                window: Option<WindowId>,
            },
            WindowFocusChanged {
                id: Option<WindowId>,
            },
            ActiveOutputChanged {
                output: Option<OutputId>,
            },
            PrimaryOutputChanged {
                output: Option<OutputId>,
            },
            CameraChanged {
                camera: CameraSnapshot,
            },
            ConfigLoaded {
                generation: u64,
                failed: bool,
                error: Option<String>,
            },
            PlacementChanged {
                window: WindowId,
                placement: WindowPlacement,
            },
            Unsupported {
                name: String,
            },
        }

        impl From<&Desktop> for DesktopSnapshot {
            fn from(desktop: &Desktop) -> Self {
                let mut windows = Vec::new();
                let mut cameras = Vec::new();
                let workspaces = desktop
                    .workspaces()
                    .map(|workspace| {
                        cameras.push(CameraSnapshot {
                            workspace: workspace.id.into(),
                            center: workspace.camera.center.into(),
                            policy: workspace.camera.policy.into(),
                        });
                        windows.extend(workspace.tiled.values().map(|window| WindowSnapshot {
                            id: window.id.into(),
                            workspace: workspace.id.into(),
                            mode: WindowMode::Tiled,
                            metadata: WindowMetadata::default(),
                            placement: WindowPlacement::Tiled {
                                world_geometry: window.geometry.into(),
                            },
                            visible_geometry: None,
                        }));
                        windows.extend(workspace.floating.values().map(|window| WindowSnapshot {
                            id: window.window.into(),
                            workspace: workspace.id.into(),
                            mode: WindowMode::Floating,
                            metadata: WindowMetadata::default(),
                            placement: WindowPlacement::Floating {
                                viewport_geometry: window.viewport.rect.into(),
                            },
                            visible_geometry: None,
                        }));
                        if let Some(full) = &workspace.fullscreen {
                            let restore = match &full.restore {
                                astera_core::FullscreenRestorePlacement::Tiled { world_rect } => {
                                    FullscreenRestore::Tiled {
                                        world_geometry: (*world_rect).into(),
                                    }
                                }
                                astera_core::FullscreenRestorePlacement::Floating { viewport } => {
                                    FullscreenRestore::Floating {
                                        viewport_geometry: viewport.rect.into(),
                                    }
                                }
                                astera_core::FullscreenRestorePlacement::Maximized { restore } => {
                                    FullscreenRestore::Maximized {
                                        restore: match restore {
                                            astera_core::RestorePlacement::Tiled { world_rect } => {
                                                BaseRestore::Tiled {
                                                    world_geometry: (*world_rect).into(),
                                                }
                                            }
                                            astera_core::RestorePlacement::Floating {
                                                viewport,
                                            } => BaseRestore::Floating {
                                                viewport_geometry: viewport.rect.into(),
                                            },
                                        },
                                    }
                                }
                            };
                            windows.push(WindowSnapshot {
                                id: full.window.into(),
                                workspace: workspace.id.into(),
                                mode: WindowMode::Fullscreen,
                                metadata: WindowMetadata::default(),
                                placement: WindowPlacement::Fullscreen { restore },
                                visible_geometry: None,
                            });
                        }
                        let location = desktop
                            .workspace_location(workspace.id)
                            .expect("workspace came from desktop");
                        WorkspaceSnapshot {
                            id: workspace.id.into(),
                            name: workspace.name.clone(),
                            original_output: workspace.original_output.clone(),
                            output: location.output.map(Into::into),
                            local_index: location
                                .output
                                .map(|_| u32::try_from(location.index + 1).unwrap_or(u32::MAX)),
                            active_window: workspace.focused_window.map(Into::into),
                            tiled_count: workspace.tiled.len() as u64,
                            floating_count: workspace.floating.len() as u64,
                            fullscreen: workspace
                                .fullscreen
                                .as_ref()
                                .map(|full| full.window.into()),
                        }
                    })
                    .collect();
                let outputs = desktop
                    .outputs
                    .values()
                    .map(|set| {
                        let size = set.output.logical_size;
                        OutputSnapshot {
                            id: set.output.id.into(),
                            stable_key: set.output.stable_key.clone(),
                            active_workspace: set
                                .active_workspace()
                                .expect("normalized output")
                                .id
                                .into(),
                            workspaces: set
                                .workspaces
                                .iter()
                                .map(|workspace| workspace.id.into())
                                .collect(),
                            physical_size: set.output.physical_size.into(),
                            logical_size: size.into(),
                            native_scale: set.output.native_scale.into(),
                            transform: set.output.transform.into(),
                            viewport: Rect::new(0, 0, size.width, size.height),
                            usable_area: Rect::new(0, 0, size.width, size.height),
                        }
                    })
                    .collect();
                Self {
                    active_output: None,
                    primary_output: desktop.primary_output.map(Into::into),
                    focused_window: None,
                    outputs,
                    layers: Vec::new(),
                    workspaces,
                    cameras,
                    windows,
                    config: ConfigSnapshot::default(),
                }
            }
        }

        impl DesktopSnapshot {
            pub fn with_active_output(mut self, output: Option<astera_core::OutputId>) -> Self {
                self.active_output = output.map(Into::into);
                self.focused_window = output
                    .and_then(|output| {
                        self.outputs
                            .iter()
                            .find(|candidate| candidate.id == output.into())
                    })
                    .and_then(|output| {
                        self.workspaces
                            .iter()
                            .find(|workspace| workspace.id == output.active_workspace)
                    })
                    .and_then(|workspace| workspace.active_window);
                self
            }
        }
    }
}

pub use wire::v1::{
    CameraSnapshot, Command, ConfigSnapshot, DesktopSnapshot, Error, ErrorCode, OutputSelector,
    OutputSnapshot, Request, RequestKind, Response, Success, WorkspaceSelector, WorkspaceSnapshot,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> wire::v1::Request {
        wire::v1::Request {
            kind: wire::v1::RequestKind::Command(wire::v1::Command::FocusWorkspace {
                workspace: wire::v1::WorkspaceSelector::LocalIndex {
                    output: wire::v1::OutputSelector::Key("DP-1".into()),
                    index: 3,
                },
            }),
        }
    }

    #[test]
    fn frame_and_v1_wire_round_trip() {
        let encoded = encode_frame(1, &request()).unwrap();
        assert_eq!(
            decode_frame::<wire::v1::Request>(&encoded, 1).unwrap(),
            request()
        );
        assert_eq!(
            decode_request(&encoded).unwrap(),
            VersionedRequest::V1(request())
        );
    }

    #[test]
    fn parser_requires_exactly_one_complete_line() {
        assert_eq!(
            parse_frame("1 (kind:EventStream)"),
            Err(FramingError::InvalidFrame)
        );
        assert_eq!(
            parse_frame("1 (kind:EventStream)\nextra\n"),
            Err(FramingError::InvalidFrame)
        );
        assert_eq!(parse_frame("1  \n"), Err(FramingError::InvalidFrame));
    }

    #[test]
    fn bootstrap_v0_is_frozen_and_reports_version_bounds() {
        assert_eq!(
            encode_frame(BOOTSTRAP_VERSION, &wire::v0::Request::Versions).unwrap(),
            "0 Versions\n"
        );
        assert_eq!(
            encode_frame(
                BOOTSTRAP_VERSION,
                &wire::v0::Response::Versions {
                    minimum: 1,
                    current: 1,
                },
            )
            .unwrap(),
            "0 Versions(minimum:1,current:1)\n"
        );
        assert_eq!(
            encode_frame(
                BOOTSTRAP_VERSION,
                &wire::v0::Response::InvalidFrame {
                    message: "bad frame".into(),
                },
            )
            .unwrap(),
            "0 InvalidFrame(message:\"bad frame\")\n"
        );
        assert_eq!(
            encode_frame(
                BOOTSTRAP_VERSION,
                &wire::v0::Response::InvalidRequest {
                    message: "bad request".into(),
                },
            )
            .unwrap(),
            "0 InvalidRequest(message:\"bad request\")\n"
        );
        let response = wire::v0::Response::UnsupportedVersion {
            requested: 9,
            minimum: MIN_VERSION,
            current: CURRENT_VERSION,
        };
        let encoded = encode_frame(BOOTSTRAP_VERSION, &response).unwrap();
        assert_eq!(
            encoded,
            "0 UnsupportedVersion(requested:9,minimum:1,current:1)\n"
        );
        assert_eq!(
            decode_frame::<wire::v0::Response>(&encoded, 0).unwrap(),
            response
        );
        assert_eq!(
            decode_request("9 (kind:EventStream)\n"),
            Err(RequestDecodeError::UnsupportedVersion {
                requested: 9,
                minimum: 1,
                current: 1,
            })
        );
    }

    #[test]
    fn v1_textual_fixtures_are_stable() {
        assert_eq!(
            encode_frame(1, &request()).unwrap(),
            "1 (kind:Command(FocusWorkspace(workspace:LocalIndex(output:Key(\"DP-1\"),index:3))))\n"
        );
        assert_eq!(
            encode_frame(
                1,
                &wire::v1::Request {
                    kind: wire::v1::RequestKind::EventStream,
                },
            )
            .unwrap(),
            "1 (kind:EventStream)\n"
        );
        assert_eq!(
            encode_frame(
                1,
                &wire::v1::Response::Success(wire::v1::Success::Handled { sequence: 42 }),
            )
            .unwrap(),
            "1 Success(Handled(sequence:42))\n"
        );
        assert_eq!(
            encode_frame(
                1,
                &wire::v1::Response::Error(wire::v1::Error {
                    code: wire::v1::ErrorCode::NotFound,
                    message: "missing".into(),
                    sequence: 42,
                }),
            )
            .unwrap(),
            "1 Error((code:NotFound,message:\"missing\",sequence:42))\n"
        );
        assert_eq!(
            encode_frame(
                1,
                &wire::v1::EventEnvelope {
                    sequence: 43,
                    event: wire::v1::Event::WindowClosed {
                        window: wire::v1::WindowId(7),
                    },
                },
            )
            .unwrap(),
            "1 (sequence:43,event:WindowClosed(window:(7)))\n"
        );
    }

    #[test]
    fn finite_restore_and_precise_event_round_trip() {
        let event = wire::v1::EventEnvelope {
            sequence: 4,
            event: wire::v1::Event::PlacementChanged {
                window: wire::v1::WindowId(7),
                placement: wire::v1::WindowPlacement::Fullscreen {
                    restore: wire::v1::FullscreenRestore::Tiled {
                        world_geometry: wire::v1::Rect::new(1, 2, 3, 4),
                    },
                },
            },
        };
        let encoded = encode_frame(1, &event).unwrap();
        assert_eq!(
            decode_frame::<wire::v1::EventEnvelope>(&encoded, 1).unwrap(),
            event
        );

        let nested = wire::v1::WindowPlacement::Fullscreen {
            restore: wire::v1::FullscreenRestore::Maximized {
                restore: wire::v1::BaseRestore::Floating {
                    viewport_geometry: wire::v1::Rect::new(5, 6, 7, 8),
                },
            },
        };
        let encoded = encode_frame(1, &nested).unwrap();
        assert_eq!(
            decode_frame::<wire::v1::WindowPlacement>(&encoded, 1).unwrap(),
            nested
        );
    }
}
