use std::{
    env,
    io::{Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use astera_ipc::{
    BOOTSTRAP_VERSION, CURRENT_VERSION, Command as IpcCommand, DesktopSnapshot, MIN_VERSION,
    Request, RequestKind, Response, Success, decode_frame, encode_frame, wire,
};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "astrology",
    version,
    about = "Inspect a running Astera compositor"
)]
struct Args {
    #[command(subcommand)]
    command: Option<AstrologyCommand>,
}

#[derive(Clone, Debug, Subcommand)]
enum AstrologyCommand {
    /// Print outputs, workspaces, focus, and window counts.
    Overview,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let _command = args.command.unwrap_or(AstrologyCommand::Overview);
    let display = env::var("WAYLAND_DISPLAY")?;
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .context("XDG_RUNTIME_DIR is required")?;
    let socket = runtime.join(format!("{display}.ipc"));
    let versions = exchange(
        &socket,
        &encode_frame(BOOTSTRAP_VERSION, &wire::v0::Request::Versions)?,
    )?;
    let wire::v0::Response::Versions { minimum, current } =
        decode_frame::<wire::v0::Response>(&versions, BOOTSTRAP_VERSION)?
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
    let request = Request {
        kind: RequestKind::Command(IpcCommand::GetState),
    };
    let payload = exchange(&socket, &encode_frame(maximum_common, &request)?)?;
    match decode_frame::<Response>(&payload, maximum_common)? {
        Response::Success(Success::State { snapshot, .. }) => {
            print!("{}", format_overview(&snapshot))
        }
        Response::Success(_) => bail!("unexpected IPC success response"),
        Response::Error(error) => bail!("{:?}: {}", error.code, error.message),
    }
    Ok(())
}

fn exchange(path: &std::path::Path, frame: &str) -> Result<String> {
    let mut stream = UnixStream::connect(path)?;
    stream.write_all(frame.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;
    let mut payload = String::new();
    stream.read_to_string(&mut payload)?;
    Ok(payload)
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
    use astera_ipc::wire::v1::{
        OutputId, OutputTransform, Rect, Scale120, Size, WindowId, WorkspaceId,
    };
    use astera_ipc::{ConfigSnapshot, DesktopSnapshot, OutputSnapshot, WorkspaceSnapshot};

    use super::format_overview;

    #[test]
    fn overview_marks_active_output_and_background_workspace() {
        let snapshot = DesktopSnapshot {
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
        };

        let overview = format_overview(&snapshot);
        assert!(overview.contains("DP-1 *: workspace 3, 2 total, 2560x1440 logical, 1.50x"));
        assert!(overview.contains("4: background"));
    }
}
