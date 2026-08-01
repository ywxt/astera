use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result, anyhow, bail};
use astera_config::Config;
use astera_ipc::{
    BOOTSTRAP_VERSION, CURRENT_VERSION, Command as IpcCommand, DesktopSnapshot, MIN_VERSION,
    Request, RequestKind, Response, Success, decode_frame, encode_frame, wire,
};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "astrology", version, about = "Inspect and control Astera")]
struct Args {
    /// Suppress successful mutation messages.
    #[arg(long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    command: Option<TopCommand>,
}

#[derive(Debug, Subcommand)]
enum TopCommand {
    /// Print the concise desktop overview.
    Overview,
    /// Print the authoritative desktop state.
    State {
        /// Emit stable JSON instead of the human overview.
        #[arg(long)]
        json: bool,
    },
    /// Follow compositor events.
    Events {
        /// Emit one JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// Validate, format, or generate configuration without a running compositor.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Output {
        #[command(subcommand)]
        command: OutputCommand,
    },
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    Window {
        #[command(subcommand)]
        command: WindowCommand,
    },
    Camera {
        #[command(subcommand)]
        command: CameraCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Check {
        path: Option<PathBuf>,
    },
    Format {
        path: Option<PathBuf>,
        /// Check formatting without writing.
        #[arg(long)]
        check: bool,
    },
    Generate {
        path: Option<PathBuf>,
        /// Create the file; refuses to overwrite an existing path.
        #[arg(long)]
        write: bool,
    },
}

#[derive(Debug, Subcommand)]
enum OutputCommand {
    Focus { output: Option<String> },
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    Focus {
        workspace: String,
        #[arg(long)]
        output: Option<String>,
    },
    Rename {
        workspace: String,
        name: String,
        #[arg(long)]
        output: Option<String>,
    },
    ClearName {
        workspace: String,
        #[arg(long)]
        output: Option<String>,
    },
    Move {
        workspace: String,
        output: String,
        #[arg(long)]
        index: Option<u32>,
        #[arg(long)]
        activate: bool,
    },
}

#[derive(Debug, Subcommand)]
enum WindowCommand {
    /// Focus the nearest window in a cardinal direction.
    Focus {
        direction: DirectionArg,
    },
    /// Focus a window by ID.
    Activate {
        window: u64,
    },
    Close {
        window: Option<u64>,
    },
    Mode {
        mode: WindowModeArg,
        window: Option<u64>,
    },
    Move {
        workspace: String,
        window: Option<u64>,
        #[arg(long)]
        output: Option<String>,
        #[arg(long)]
        activate: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CameraCommand {
    Pan {
        dx: i64,
        dy: i64,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long, requires = "workspace")]
        output: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DirectionArg {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum WindowModeArg {
    Tiled,
    Floating,
    Maximized,
    Fullscreen,
}

impl From<WindowModeArg> for wire::v1::WindowMode {
    fn from(value: WindowModeArg) -> Self {
        match value {
            WindowModeArg::Tiled => Self::Tiled,
            WindowModeArg::Floating => Self::Floating,
            WindowModeArg::Maximized => Self::Maximized,
            WindowModeArg::Fullscreen => Self::Fullscreen,
        }
    }
}

#[derive(Debug)]
struct ExitError {
    code: u8,
    error: anyhow::Error,
}

impl ExitError {
    fn usage(error: impl Into<anyhow::Error>) -> Self {
        Self {
            code: 2,
            error: error.into(),
        }
    }
    fn ipc(error: impl Into<anyhow::Error>) -> Self {
        Self {
            code: 3,
            error: error.into(),
        }
    }
    fn server(error: wire::v1::Error) -> Self {
        Self {
            code: 4,
            error: anyhow!(
                "{:?}: {} (sequence {})",
                error.code,
                error.message,
                error.sequence
            ),
        }
    }
    fn stream(error: impl Into<anyhow::Error>) -> Self {
        Self {
            code: 5,
            error: error.into(),
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args, &mut std::io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {:#}", error.error);
            ExitCode::from(error.code)
        }
    }
}

fn run(args: Args, output: &mut impl Write) -> Result<(), ExitError> {
    let command = args.command.unwrap_or(TopCommand::Overview);
    if let TopCommand::Config { command } = command {
        return run_config(command, args.quiet, output).map_err(ExitError::usage);
    }

    let socket = resolve_socket().map_err(ExitError::ipc)?;
    let version = negotiate(&socket).map_err(ExitError::ipc)?;
    match command {
        TopCommand::Overview => {
            let (_, snapshot) = get_state(&socket, version)?;
            write!(output, "{}", format_overview(&snapshot)).map_err(ExitError::ipc)?;
        }
        TopCommand::State { json } => {
            let (sequence, snapshot) = get_state(&socket, version)?;
            if json {
                serde_json::to_writer_pretty(&mut *output, &Success::State { sequence, snapshot })
                    .map_err(ExitError::ipc)?;
                writeln!(output).map_err(ExitError::ipc)?;
            } else {
                write!(output, "{}", format_overview(&snapshot)).map_err(ExitError::ipc)?;
            }
        }
        TopCommand::Events { json } => {
            stream_events(&socket, version, json, output).map_err(ExitError::stream)?;
        }
        TopCommand::Output { command } => {
            let command = match command {
                OutputCommand::Focus { output } => IpcCommand::FocusOutput(
                    parse_output(output.as_deref()).map_err(ExitError::usage)?,
                ),
            };
            send_command(&socket, version, command, args.quiet, output)?;
        }
        TopCommand::Workspace { command } => {
            let command = workspace_command(&socket, version, command)?;
            send_command(&socket, version, command, args.quiet, output)?;
        }
        TopCommand::Window { command } => {
            let command = window_command(&socket, version, command)?;
            send_command(&socket, version, command, args.quiet, output)?;
        }
        TopCommand::Camera { command } => {
            let command = camera_command(&socket, version, command)?;
            send_command(&socket, version, command, args.quiet, output)?;
        }
        TopCommand::Config { .. } => unreachable!(),
    }
    Ok(())
}

fn run_config(command: ConfigCommand, quiet: bool, output: &mut impl Write) -> Result<()> {
    match command {
        ConfigCommand::Check { path } => {
            let path = path.unwrap_or(default_config_path()?);
            Config::load(&path)
                .with_context(|| format!("invalid configuration {}", path.display()))?;
            if !quiet {
                writeln!(output, "configuration is valid: {}", path.display())?;
            }
        }
        ConfigCommand::Format { path, check } => {
            let path = path.unwrap_or(default_config_path()?);
            let source = fs::read_to_string(&path)?;
            let formatted = Config::format_kdl(&source)?;
            if check {
                if formatted != source {
                    bail!("configuration is not formatted: {}", path.display());
                }
                if !quiet {
                    writeln!(output, "configuration is formatted: {}", path.display())?;
                }
            } else {
                atomic_write(&path, formatted.as_bytes(), true)?;
                if !quiet {
                    writeln!(output, "formatted {}", path.display())?;
                }
            }
        }
        ConfigCommand::Generate { path, write } => {
            let generated = Config::generated_kdl();
            if write {
                let path = path.unwrap_or(default_config_path()?);
                atomic_write(&path, generated.as_bytes(), false)?;
                if !quiet {
                    writeln!(output, "created {}", path.display())?;
                }
            } else {
                write!(output, "{generated}")?;
            }
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, contents: &[u8], overwrite: bool) -> Result<()> {
    if !overwrite && path.exists() {
        bail!("refusing to overwrite existing {}", path.display());
    }
    let parent = path.parent().context("configuration path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".config.kdl.{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&temporary)?;
        if overwrite && path.exists() {
            file.set_permissions(fs::metadata(path)?.permissions())?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn workspace_command(
    path: &Path,
    version: u16,
    command: WorkspaceCommand,
) -> Result<IpcCommand, ExitError> {
    match command {
        WorkspaceCommand::Focus { workspace, output } => Ok(IpcCommand::FocusWorkspace {
            workspace: parse_workspace(&workspace, output.as_deref()).map_err(ExitError::usage)?,
        }),
        WorkspaceCommand::Rename {
            workspace,
            name,
            output,
        } => {
            let (_, snapshot) = get_state(path, version)?;
            Ok(IpcCommand::SetWorkspaceName {
                workspace: resolve_workspace_id(&snapshot, &workspace, output.as_deref())
                    .map_err(ExitError::usage)?,
                name: Some(name),
            })
        }
        WorkspaceCommand::ClearName { workspace, output } => {
            let (_, snapshot) = get_state(path, version)?;
            Ok(IpcCommand::SetWorkspaceName {
                workspace: resolve_workspace_id(&snapshot, &workspace, output.as_deref())
                    .map_err(ExitError::usage)?,
                name: None,
            })
        }
        WorkspaceCommand::Move {
            workspace,
            output,
            index,
            activate,
        } => {
            let (_, snapshot) = get_state(path, version)?;
            Ok(IpcCommand::MoveWorkspace {
                workspace: resolve_workspace_id(&snapshot, &workspace, None)
                    .map_err(ExitError::usage)?,
                target_output: parse_output(Some(&output)).map_err(ExitError::usage)?,
                target_index: index,
                activate,
            })
        }
    }
}

fn window_command(
    path: &Path,
    version: u16,
    command: WindowCommand,
) -> Result<IpcCommand, ExitError> {
    match command {
        WindowCommand::Focus { direction } => {
            Ok(IpcCommand::FocusDirection(direction_value(direction)))
        }
        WindowCommand::Activate { window } => {
            Ok(IpcCommand::FocusWindow(wire::v1::WindowId(window)))
        }
        WindowCommand::Close { window } => Ok(IpcCommand::CloseWindow(resolve_window(
            path, version, window,
        )?)),
        WindowCommand::Mode { mode, window } => Ok(IpcCommand::SetWindowMode {
            window: resolve_window(path, version, window)?,
            mode: mode.into(),
        }),
        WindowCommand::Move {
            workspace,
            window,
            output,
            activate,
        } => Ok(IpcCommand::MoveWindow {
            window: resolve_window(path, version, window)?,
            target: parse_workspace(&workspace, output.as_deref()).map_err(ExitError::usage)?,
            activate,
        }),
    }
}

fn camera_command(
    path: &Path,
    version: u16,
    command: CameraCommand,
) -> Result<IpcCommand, ExitError> {
    match command {
        CameraCommand::Pan {
            dx,
            dy,
            workspace,
            output,
        } => {
            let (_, snapshot) = get_state(path, version)?;
            let workspace = match workspace {
                Some(selector) => resolve_workspace_id(&snapshot, &selector, output.as_deref())
                    .map_err(ExitError::usage)?,
                None => active_workspace(&snapshot)
                    .ok_or_else(|| ExitError::usage(anyhow!("active output has no workspace")))?,
            };
            Ok(IpcCommand::PanCamera { workspace, dx, dy })
        }
    }
}

fn resolve_window(
    path: &Path,
    version: u16,
    window: Option<u64>,
) -> Result<wire::v1::WindowId, ExitError> {
    if let Some(window) = window {
        return Ok(wire::v1::WindowId(window));
    }
    let (_, snapshot) = get_state(path, version)?;
    snapshot
        .focused_window
        .ok_or_else(|| ExitError::usage(anyhow!("there is no focused window")))
}

fn parse_workspace(value: &str, output: Option<&str>) -> Result<wire::v1::WorkspaceSelector> {
    if let Some(id) = value.strip_prefix("id:") {
        if output.is_some() {
            bail!("id workspace selector cannot use --output");
        }
        return Ok(wire::v1::WorkspaceSelector::Id(wire::v1::WorkspaceId(
            id.parse().context("invalid workspace ID")?,
        )));
    }
    if let Ok(index) = value.parse::<u32>() {
        if index == 0 {
            bail!("workspace index is one-based");
        }
        return Ok(wire::v1::WorkspaceSelector::LocalIndex {
            output: parse_output(output)?,
            index,
        });
    }
    if output.is_some() {
        bail!("named workspace selector cannot use --output");
    }
    if value.is_empty() {
        bail!("workspace selector cannot be empty");
    }
    Ok(wire::v1::WorkspaceSelector::Name(value.to_owned()))
}

fn resolve_workspace_id(
    snapshot: &DesktopSnapshot,
    selector: &str,
    output: Option<&str>,
) -> Result<wire::v1::WorkspaceId> {
    let selector = parse_workspace(selector, output)?;
    match selector {
        wire::v1::WorkspaceSelector::Id(id) => snapshot
            .workspaces
            .iter()
            .any(|workspace| workspace.id == id)
            .then_some(id),
        wire::v1::WorkspaceSelector::Name(name) => snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.name.as_deref() == Some(&name))
            .map(|workspace| workspace.id),
        wire::v1::WorkspaceSelector::LocalIndex { output, index } => {
            let output = resolve_output_id(snapshot, output)?;
            snapshot
                .workspaces
                .iter()
                .find(|workspace| {
                    workspace.output == Some(output) && workspace.local_index == Some(index)
                })
                .map(|workspace| workspace.id)
        }
    }
    .context("unknown workspace")
}

fn parse_output(value: Option<&str>) -> Result<wire::v1::OutputSelector> {
    match value {
        None | Some("active") => Ok(wire::v1::OutputSelector::Active),
        Some("") => bail!("output selector cannot be empty"),
        Some(value) => Ok(value
            .parse::<u32>()
            .map(|id| wire::v1::OutputSelector::Id(wire::v1::OutputId(id)))
            .unwrap_or_else(|_| wire::v1::OutputSelector::Key(value.to_owned()))),
    }
}

fn resolve_output_id(
    snapshot: &DesktopSnapshot,
    selector: wire::v1::OutputSelector,
) -> Result<wire::v1::OutputId> {
    match selector {
        wire::v1::OutputSelector::Active => snapshot.active_output,
        wire::v1::OutputSelector::Id(id) => snapshot
            .outputs
            .iter()
            .any(|output| output.id == id)
            .then_some(id),
        wire::v1::OutputSelector::Key(key) => snapshot
            .outputs
            .iter()
            .find(|output| output.stable_key == key)
            .map(|output| output.id),
    }
    .context("unknown output")
}

fn active_workspace(snapshot: &DesktopSnapshot) -> Option<wire::v1::WorkspaceId> {
    let output = snapshot.active_output?;
    snapshot
        .outputs
        .iter()
        .find(|candidate| candidate.id == output)
        .map(|output| output.active_workspace)
}

fn direction_value(direction: DirectionArg) -> wire::v1::Direction {
    match direction {
        DirectionArg::Left => wire::v1::Direction { x: -1.0, y: 0.0 },
        DirectionArg::Right => wire::v1::Direction { x: 1.0, y: 0.0 },
        DirectionArg::Up => wire::v1::Direction { x: 0.0, y: -1.0 },
        DirectionArg::Down => wire::v1::Direction { x: 0.0, y: 1.0 },
    }
}

fn resolve_socket() -> Result<PathBuf> {
    let display = env::var("WAYLAND_DISPLAY")
        .context("WAYLAND_DISPLAY is not set; use the Astera display printed at startup")?;
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .context("XDG_RUNTIME_DIR is required")?;
    Ok(runtime.join("astera").join(format!("{display}.ipc")))
}

fn default_config_path() -> Result<PathBuf> {
    if let Some(directory) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(directory).join("astera/config.kdl"));
    }
    Ok(
        PathBuf::from(env::var_os("HOME").context("HOME is not set")?)
            .join(".config/astera/config.kdl"),
    )
}

fn negotiate(path: &Path) -> Result<u16> {
    let response = request_line(
        path,
        &encode_frame(BOOTSTRAP_VERSION, &wire::v0::Request::Versions)?,
    )?;
    let wire::v0::Response::Versions { minimum, current } =
        decode_frame::<wire::v0::Response>(&response, BOOTSTRAP_VERSION)?
    else {
        bail!("server rejected version negotiation")
    };
    let minimum_common = minimum.max(MIN_VERSION);
    let maximum_common = current.min(CURRENT_VERSION);
    if minimum_common > maximum_common {
        bail!(
            "no common IPC version (server {minimum}..={current}, client {MIN_VERSION}..={CURRENT_VERSION})"
        );
    }
    Ok(maximum_common)
}

fn request(path: &Path, version: u16, kind: RequestKind) -> Result<Response> {
    let frame = request_line(path, &encode_frame(version, &Request { kind })?)?;
    Ok(decode_frame(&frame, version)?)
}

fn request_line(path: &Path, frame: &str) -> Result<String> {
    let mut stream = UnixStream::connect(path)
        .with_context(|| format!("could not connect to {}", path.display()))?;
    stream.write_all(frame.as_bytes())?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    if response.is_empty() {
        bail!("IPC server closed without a response");
    }
    Ok(response)
}

fn get_state(path: &Path, version: u16) -> Result<(u64, DesktopSnapshot), ExitError> {
    match request(path, version, RequestKind::Command(IpcCommand::GetState))
        .map_err(ExitError::ipc)?
    {
        Response::Success(Success::State { sequence, snapshot }) => Ok((sequence, snapshot)),
        Response::Success(_) => Err(ExitError::ipc(anyhow!("unexpected IPC response"))),
        Response::Error(error) => Err(ExitError::server(error)),
    }
}

fn send_command(
    path: &Path,
    version: u16,
    command: IpcCommand,
    quiet: bool,
    output: &mut impl Write,
) -> Result<(), ExitError> {
    match request(path, version, RequestKind::Command(command)).map_err(ExitError::ipc)? {
        Response::Success(Success::Handled { sequence }) => {
            if !quiet {
                writeln!(output, "ok (sequence {sequence})").map_err(ExitError::ipc)?;
            }
            Ok(())
        }
        Response::Success(_) => Err(ExitError::ipc(anyhow!("unexpected IPC response"))),
        Response::Error(error) => Err(ExitError::server(error)),
    }
}

fn stream_events(path: &Path, version: u16, json: bool, output: &mut impl Write) -> Result<()> {
    let mut stream = UnixStream::connect(path)?;
    stream.write_all(
        encode_frame(
            version,
            &Request {
                kind: RequestKind::EventStream,
            },
        )?
        .as_bytes(),
    )?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        bail!("event stream closed before its snapshot");
    }
    let mut expected = match decode_frame::<Response>(&line, version)? {
        Response::Success(Success::EventStream { sequence, snapshot }) => {
            if json {
                serde_json::to_writer(&mut *output, &Success::State { sequence, snapshot })?;
                writeln!(output)?;
            } else {
                write!(output, "{}", format_overview(&snapshot))?;
            }
            sequence.checked_add(1)
        }
        Response::Success(_) => bail!("unexpected IPC response"),
        Response::Error(error) => bail!("{:?}: {}", error.code, error.message),
    };
    output.flush()?;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            bail!("event stream disconnected");
        }
        let event = decode_frame::<wire::v1::EventEnvelope>(&line, version)?;
        let sequence = expected.context("event stream sequence overflow")?;
        if event.sequence != sequence {
            bail!(
                "event stream sequence gap: expected {sequence}, received {}",
                event.sequence
            );
        }
        expected = event.sequence.checked_add(1);
        if json {
            serde_json::to_writer(&mut *output, &event)?;
            writeln!(output)?;
        } else {
            writeln!(output, "{} {:?}", event.sequence, event.event)?;
        }
        output.flush()?;
    }
}

fn format_overview(snapshot: &DesktopSnapshot) -> String {
    let mut text = String::from("Outputs\n");
    for output in &snapshot.outputs {
        let active = if Some(output.id) == snapshot.active_output {
            " *"
        } else {
            ""
        };
        let scale = output.native_scale.0 as f64 / 120.0;
        text.push_str(&format!(
            "  {}{}: workspace {}, {} total, {}x{} logical, {:.2}x, {:?}\n",
            output.stable_key,
            active,
            output.active_workspace.0,
            output.workspaces.len(),
            output.logical_size.width,
            output.logical_size.height,
            scale,
            output.transform
        ));
    }
    text.push_str("Workspaces\n");
    for workspace in &snapshot.workspaces {
        let location = workspace
            .output
            .zip(workspace.local_index)
            .map(|(id, index)| format!("output {} index {}", id.0, index))
            .unwrap_or_else(|| "background".into());
        let label = workspace
            .name
            .as_deref()
            .map(|name| format!(" ({name})"))
            .unwrap_or_default();
        text.push_str(&format!(
            "  {}{}: {}, focus {:?}, tiled {}, floating {}, fullscreen {:?}\n",
            workspace.id.0,
            label,
            location,
            workspace.active_window.map(|id| id.0),
            workspace.tiled_count,
            workspace.floating_count,
            workspace.fullscreen.map(|id| id.0)
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use astera_ipc::wire::v1::{
        Event, EventEnvelope, OutputId, OutputTransform, Rect, Scale120, Size, WindowId,
        WorkspaceId,
    };
    use astera_ipc::{ConfigSnapshot, OutputSnapshot, WorkspaceSnapshot};
    use clap::CommandFactory;

    fn snapshot() -> DesktopSnapshot {
        DesktopSnapshot {
            active_output: Some(OutputId(2)),
            primary_output: Some(OutputId(2)),
            focused_window: Some(WindowId(9)),
            outputs: vec![OutputSnapshot {
                id: OutputId(2),
                stable_key: "DP-1".to_owned(),
                active_workspace: WorkspaceId(3),
                workspaces: vec![WorkspaceId(3), WorkspaceId(4)],
                physical_size: Size::new(3840, 2160),
                logical_size: Size::new(2560, 1440),
                native_scale: Scale120(180),
                transform: OutputTransform::Normal,
                viewport: Rect::new(0, 0, 2560, 1440),
                usable_area: Rect::new(0, 0, 2560, 1440),
            }],
            layers: vec![],
            workspaces: vec![
                WorkspaceSnapshot {
                    id: WorkspaceId(3),
                    name: Some("code".into()),
                    original_output: Some("DP-1".into()),
                    output: Some(OutputId(2)),
                    local_index: Some(1),
                    active_window: Some(WindowId(9)),
                    tiled_count: 2,
                    floating_count: 1,
                    fullscreen: None,
                },
                WorkspaceSnapshot {
                    id: WorkspaceId(4),
                    name: None,
                    original_output: Some("DP-1".into()),
                    output: None,
                    local_index: None,
                    active_window: None,
                    tiled_count: 1,
                    floating_count: 0,
                    fullscreen: None,
                },
            ],
            cameras: vec![],
            windows: vec![],
            config: ConfigSnapshot::default(),
        }
    }

    fn event_stream_fixture(snapshot_sequence: u64, events: &[u64]) -> (anyhow::Error, String) {
        let directory = std::env::temp_dir().join(format!(
            "astrology-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("ipc");
        let listener = UnixListener::bind(&path).unwrap();
        let events = events.to_vec();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert!(matches!(
                decode_frame::<Request>(&request, CURRENT_VERSION)
                    .unwrap()
                    .kind,
                RequestKind::EventStream
            ));
            stream
                .write_all(
                    encode_frame(
                        CURRENT_VERSION,
                        &Response::Success(Success::EventStream {
                            sequence: snapshot_sequence,
                            snapshot: snapshot(),
                        }),
                    )
                    .unwrap()
                    .as_bytes(),
                )
                .unwrap();
            for sequence in events {
                stream
                    .write_all(
                        encode_frame(
                            CURRENT_VERSION,
                            &EventEnvelope {
                                sequence,
                                event: Event::Unsupported {
                                    name: "future".into(),
                                },
                            },
                        )
                        .unwrap()
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });
        let mut output = Vec::new();
        let error = stream_events(&path, CURRENT_VERSION, true, &mut output).unwrap_err();
        server.join().unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
        (error, String::from_utf8(output).unwrap())
    }

    #[test]
    fn cli_schema_is_valid_and_resource_commands_parse() {
        Args::command().debug_assert();
        let args =
            Args::try_parse_from(["astrology", "workspace", "focus", "3", "--output", "DP-1"])
                .unwrap();
        assert!(matches!(args.command, Some(TopCommand::Workspace { .. })));
        let args = Args::try_parse_from(["astrology", "window", "mode", "fullscreen"]).unwrap();
        assert!(matches!(args.command, Some(TopCommand::Window { .. })));
    }

    #[test]
    fn friendly_workspace_selectors_are_unambiguous() {
        assert!(matches!(
            parse_workspace("3", None).unwrap(),
            wire::v1::WorkspaceSelector::LocalIndex { index: 3, .. }
        ));
        assert!(matches!(
            parse_workspace("code", None).unwrap(),
            wire::v1::WorkspaceSelector::Name(_)
        ));
        assert!(matches!(
            parse_workspace("id:7", None).unwrap(),
            wire::v1::WorkspaceSelector::Id(wire::v1::WorkspaceId(7))
        ));
        assert!(parse_workspace("code", Some("DP-1")).is_err());
    }

    #[test]
    fn config_commands_do_not_require_wayland_display() {
        let args = Args::try_parse_from(["astrology", "config", "generate"]).unwrap();
        let mut output = Vec::new();
        run(args, &mut output).unwrap();
        let generated = String::from_utf8(output).unwrap();
        assert!(generated.contains("spawn \"kitty\""));
        Config::from_kdl(&generated).unwrap();
    }

    #[test]
    fn overview_and_snapshot_resolution_cover_background_workspaces() {
        let snapshot = snapshot();
        let overview = format_overview(&snapshot);
        assert!(overview.contains("DP-1 *: workspace 3"));
        assert!(overview.contains("4: background"));
        assert_eq!(
            resolve_workspace_id(&snapshot, "code", None).unwrap(),
            WorkspaceId(3)
        );
        assert_eq!(
            resolve_workspace_id(&snapshot, "1", Some("DP-1")).unwrap(),
            WorkspaceId(3)
        );
    }

    #[test]
    fn event_stream_detects_eof_and_sequence_gaps() {
        let (error, output) = event_stream_fixture(8, &[9]);
        assert!(error.to_string().contains("disconnected"));
        assert_eq!(output.lines().count(), 2);

        let (error, output) = event_stream_fixture(8, &[10]);
        assert!(error.to_string().contains("expected 9, received 10"));
        assert_eq!(output.lines().count(), 1);

        let (error, output) = event_stream_fixture(8, &[9, 11]);
        assert!(error.to_string().contains("expected 10, received 11"));
        assert_eq!(output.lines().count(), 2);
    }
}
