use std::{
    env,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use astera_ipc::{
    BOOTSTRAP_VERSION, CURRENT_VERSION, Command as IpcCommand, DesktopSnapshot, MIN_VERSION,
    Request, RequestKind, Response, Success, decode_frame, encode_frame, wire,
};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "astrology",
    version,
    about = "Inspect and control a running Astera compositor"
)]
struct Args {
    #[command(subcommand)]
    command: Option<AstrologyCommand>,
}

#[derive(Clone, Debug, Subcommand)]
enum AstrologyCommand {
    /// Print outputs, workspaces, focus, and window counts.
    Overview,
    /// Print the complete authoritative desktop snapshot.
    State {
        /// Emit compact RON instead of pretty-printed RON.
        #[arg(long)]
        raw: bool,
    },
    /// Print an initial snapshot followed by one RON EventEnvelope per line.
    #[command(alias = "event-stream")]
    Events,
    /// Send any v1 Command encoded as RON.
    Command {
        /// For example: 'FocusWindow((42))'.
        ron: String,
    },
    FocusWindow {
        window: u64,
    },
    FocusDirection {
        x: f64,
        y: f64,
    },
    FocusOutput {
        /// Output ID, stable key, or "active"; omitted means the active output.
        output: Option<String>,
    },
    FocusWorkspace {
        #[command(flatten)]
        workspace: WorkspaceArgs,
    },
    MoveWindow {
        window: u64,
        #[command(flatten)]
        workspace: WorkspaceArgs,
        #[arg(long)]
        activate: bool,
    },
    SetWindowMode {
        window: u64,
        mode: WindowModeArg,
    },
    PanCamera {
        workspace: u32,
        dx: i64,
        dy: i64,
    },
}

#[derive(Clone, Debug, ClapArgs)]
struct WorkspaceArgs {
    #[command(flatten)]
    selector: WorkspaceSelectorArgs,
    /// Output ID, stable key, or "active"; only used with --index.
    #[arg(long, requires = "index")]
    output: Option<String>,
}

#[derive(Clone, Debug, ClapArgs)]
#[group(id = "workspace", required = true, multiple = false)]
struct WorkspaceSelectorArgs {
    /// Globally unique workspace ID.
    #[arg(long)]
    id: Option<u32>,
    /// Unique workspace name.
    #[arg(long)]
    name: Option<String>,
    /// One-based workspace index local to an output.
    #[arg(long)]
    index: Option<u32>,
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

fn main() -> Result<()> {
    let args = Args::parse();
    let display = env::var("WAYLAND_DISPLAY")?;
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .context("XDG_RUNTIME_DIR is required")?;
    let socket = runtime.join("astera").join(format!("{display}.ipc"));
    run(args, &socket, &mut std::io::stdout())
}

fn run(args: Args, socket: &Path, output: &mut impl Write) -> Result<()> {
    let version = negotiate(socket)?;
    match args.command.unwrap_or(AstrologyCommand::Overview) {
        AstrologyCommand::Overview => {
            let (_, snapshot) = get_state(socket, version)?;
            write!(output, "{}", format_overview(&snapshot))?;
        }
        AstrologyCommand::State { raw } => {
            let (sequence, snapshot) = get_state(socket, version)?;
            if raw {
                writeln!(
                    output,
                    "{}",
                    ron::to_string(&Success::State { sequence, snapshot })?
                )?;
            } else {
                writeln!(
                    output,
                    "{}",
                    ron::ser::to_string_pretty(
                        &Success::State { sequence, snapshot },
                        ron::ser::PrettyConfig::default(),
                    )?
                )?;
            }
        }
        AstrologyCommand::Events => stream_events(socket, version, output)?,
        command => {
            let command = typed_command(command)?;
            let response = request(socket, version, RequestKind::Command(command))?;
            print_command_response(response, output)?;
        }
    }
    Ok(())
}

fn typed_command(command: AstrologyCommand) -> Result<IpcCommand> {
    Ok(match command {
        AstrologyCommand::Command { ron } => ron::from_str(&ron).context("invalid RON Command")?,
        AstrologyCommand::FocusWindow { window } => {
            IpcCommand::FocusWindow(wire::v1::WindowId(window))
        }
        AstrologyCommand::FocusDirection { x, y } => {
            IpcCommand::FocusDirection(wire::v1::Direction { x, y })
        }
        AstrologyCommand::FocusOutput { output } => {
            IpcCommand::FocusOutput(parse_output(output.as_deref())?)
        }
        AstrologyCommand::FocusWorkspace { workspace } => IpcCommand::FocusWorkspace {
            workspace: parse_workspace(workspace)?,
        },
        AstrologyCommand::MoveWindow {
            window,
            workspace,
            activate,
        } => IpcCommand::MoveWindow {
            window: wire::v1::WindowId(window),
            target: parse_workspace(workspace)?,
            activate,
        },
        AstrologyCommand::SetWindowMode { window, mode } => IpcCommand::SetWindowMode {
            window: wire::v1::WindowId(window),
            mode: mode.into(),
        },
        AstrologyCommand::PanCamera { workspace, dx, dy } => IpcCommand::PanCamera {
            workspace: wire::v1::WorkspaceId(workspace),
            dx,
            dy,
        },
        AstrologyCommand::Overview | AstrologyCommand::State { .. } | AstrologyCommand::Events => {
            bail!("command does not map to a mutation")
        }
    })
}

fn parse_output(value: Option<&str>) -> Result<wire::v1::OutputSelector> {
    match value {
        None | Some("active") => Ok(wire::v1::OutputSelector::Active),
        Some(value) => match value.parse::<u32>() {
            Ok(id) => Ok(wire::v1::OutputSelector::Id(wire::v1::OutputId(id))),
            Err(_) if !value.is_empty() => Ok(wire::v1::OutputSelector::Key(value.to_owned())),
            Err(_) => bail!("output selector cannot be empty"),
        },
    }
}

fn parse_workspace(args: WorkspaceArgs) -> Result<wire::v1::WorkspaceSelector> {
    if let Some(id) = args.selector.id {
        return Ok(wire::v1::WorkspaceSelector::Id(wire::v1::WorkspaceId(id)));
    }
    if let Some(name) = args.selector.name {
        return Ok(wire::v1::WorkspaceSelector::Name(name));
    }
    let index = args
        .selector
        .index
        .context("workspace selector is required")?;
    if index == 0 {
        bail!("workspace index is one-based and must be greater than zero");
    }
    Ok(wire::v1::WorkspaceSelector::LocalIndex {
        output: parse_output(args.output.as_deref())?,
        index,
    })
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
    let mut stream = UnixStream::connect(path)?;
    stream.write_all(frame.as_bytes())?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    if response.is_empty() {
        bail!("IPC server closed without a response");
    }
    Ok(response)
}

fn get_state(path: &Path, version: u16) -> Result<(u64, DesktopSnapshot)> {
    match request(path, version, RequestKind::Command(IpcCommand::GetState))? {
        Response::Success(Success::State { sequence, snapshot }) => Ok((sequence, snapshot)),
        Response::Success(_) => bail!("unexpected IPC success response"),
        Response::Error(error) => command_error(error),
    }
}

fn print_command_response(response: Response, output: &mut impl Write) -> Result<()> {
    match response {
        Response::Success(Success::Handled { sequence }) => {
            writeln!(output, "handled at sequence {sequence}")?;
            Ok(())
        }
        Response::Success(Success::State { sequence, snapshot }) => {
            writeln!(
                output,
                "{}",
                ron::to_string(&Success::State { sequence, snapshot })?
            )?;
            Ok(())
        }
        Response::Success(Success::EventStream { .. }) => {
            bail!("unexpected event-stream response")
        }
        Response::Error(error) => command_error(error),
    }
}

fn command_error<T>(error: wire::v1::Error) -> Result<T> {
    bail!(
        "{:?}: {} (sequence {})",
        error.code,
        error.message,
        error.sequence
    )
}

fn stream_events(path: &Path, version: u16, output: &mut impl Write) -> Result<()> {
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
    let mut expected_sequence = match decode_frame::<Response>(&line, version)? {
        Response::Success(Success::EventStream { sequence, snapshot }) => {
            writeln!(
                output,
                "{}",
                ron::to_string(&Success::State { sequence, snapshot })?
            )?;
            output.flush()?;
            sequence.checked_add(1)
        }
        Response::Success(_) => bail!("unexpected IPC success response"),
        Response::Error(error) => return command_error(error),
    };
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            bail!("event stream disconnected");
        }
        let event = decode_frame::<wire::v1::EventEnvelope>(&line, version)?;
        let Some(expected) = expected_sequence else {
            bail!(
                "event stream sequence overflow: expected no event after {}, received {}",
                u64::MAX,
                event.sequence
            );
        };
        if event.sequence != expected {
            bail!(
                "event stream sequence gap: expected {expected}, received {}",
                event.sequence
            );
        }
        expected_sequence = event.sequence.checked_add(1);
        writeln!(output, "{}", ron::to_string(&event)?)?;
        output.flush()?;
    }
}

fn format_overview(snapshot: &DesktopSnapshot) -> String {
    let mut text = String::new();
    text.push_str("Outputs\n");
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
            output.transform,
        ));
    }
    text.push_str("Workspaces\n");
    for workspace in &snapshot.workspaces {
        text.push_str(&format_workspace(workspace));
    }
    text
}

fn format_workspace(workspace: &wire::v1::WorkspaceSnapshot) -> String {
    let location = workspace
        .output
        .zip(workspace.local_index)
        .map(|(id, index)| format!("output {} index {}", id.0, index))
        .unwrap_or_else(|| "background".to_owned());
    let label = workspace
        .name
        .as_deref()
        .map(|name| format!(" ({name})"))
        .unwrap_or_default();
    format!(
        "  {}{}: {}, focus {:?}, tiled {}, floating {}, fullscreen {:?}\n",
        workspace.id.0,
        label,
        location,
        workspace.active_window.map(|id| id.0),
        workspace.tiled_count,
        workspace.floating_count,
        workspace.fullscreen.map(|id| id.0),
    )
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

    use astera_ipc::wire::v1::{
        Event, EventEnvelope, OutputId, OutputSelector, OutputTransform, Rect, Scale120, Size,
        WindowId, WorkspaceId, WorkspaceSelector,
    };
    use astera_ipc::{ConfigSnapshot, DesktopSnapshot, OutputSnapshot, WorkspaceSnapshot};

    use super::*;

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

    fn run_event_stream_fixture(
        snapshot_sequence: u64,
        event_sequences: &[u64],
    ) -> (anyhow::Error, String) {
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
        let server_snapshot = snapshot();
        let event_sequences = event_sequences.to_vec();
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
                            snapshot: server_snapshot,
                        }),
                    )
                    .unwrap()
                    .as_bytes(),
                )
                .unwrap();
            for sequence in event_sequences {
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
        let error = stream_events(&path, CURRENT_VERSION, &mut output).unwrap_err();
        server.join().unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
        (error, String::from_utf8(output).unwrap())
    }

    #[test]
    fn overview_marks_active_output_and_background_workspace() {
        let overview = format_overview(&snapshot());
        assert!(overview.contains("DP-1 *: workspace 3, 2 total, 2560x1440 logical, 1.50x"));
        assert!(overview.contains("4: background"));
    }

    #[test]
    fn typed_parser_supports_active_output_and_local_workspace() {
        let args = Args::try_parse_from([
            "astrology",
            "move-window",
            "42",
            "--index",
            "3",
            "--output",
            "DP-1",
            "--activate",
        ])
        .unwrap();
        let command = typed_command(args.command.unwrap()).unwrap();
        assert_eq!(
            command,
            IpcCommand::MoveWindow {
                window: WindowId(42),
                target: WorkspaceSelector::LocalIndex {
                    output: OutputSelector::Key("DP-1".into()),
                    index: 3,
                },
                activate: true,
            }
        );

        let args = Args::try_parse_from(["astrology", "focus-output"]).unwrap();
        assert_eq!(
            typed_command(args.command.unwrap()).unwrap(),
            IpcCommand::FocusOutput(OutputSelector::Active)
        );
    }

    #[test]
    fn generic_command_parses_every_wire_command_shape() {
        let encoded = ron::to_string(&IpcCommand::SetWorkspaceName {
            workspace: WorkspaceId(7),
            name: Some("work".into()),
        })
        .unwrap();
        let args = Args::try_parse_from(["astrology", "command", &encoded]).unwrap();
        assert_eq!(
            typed_command(args.command.unwrap()).unwrap(),
            IpcCommand::SetWorkspaceName {
                workspace: WorkspaceId(7),
                name: Some("work".into()),
            }
        );
    }

    #[test]
    fn event_stream_prints_snapshot_then_envelopes_and_errors_on_eof() {
        let expected_snapshot = snapshot();
        let (error, output) = run_event_stream_fixture(8, &[9]);
        assert!(error.to_string().contains("disconnected"));
        let mut lines = output.lines();
        assert!(matches!(
            ron::from_str::<Success>(lines.next().unwrap()).unwrap(),
            Success::State {
                sequence: 8,
                snapshot
            } if snapshot == expected_snapshot
        ));
        assert!(matches!(
            ron::from_str::<EventEnvelope>(lines.next().unwrap()).unwrap(),
            EventEnvelope { sequence: 9, .. }
        ));
        assert_eq!(lines.next(), None);
    }

    #[test]
    fn event_stream_rejects_a_gap_after_the_snapshot() {
        let (error, output) = run_event_stream_fixture(8, &[10]);
        assert!(
            error
                .to_string()
                .contains("sequence gap: expected 9, received 10")
        );
        assert_eq!(output.lines().count(), 1);
    }

    #[test]
    fn event_stream_rejects_a_gap_between_events() {
        let (error, output) = run_event_stream_fixture(8, &[9, 11]);
        assert!(
            error
                .to_string()
                .contains("sequence gap: expected 10, received 11")
        );
        assert_eq!(output.lines().count(), 2);
    }
}
