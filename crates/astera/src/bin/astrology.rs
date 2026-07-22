use std::{
    env,
    io::{Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use astera_ipc::{
    Command as IpcCommand, DesktopSnapshot, PROTOCOL_VERSION, Request, Response, WorkspaceSnapshot,
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
    let mut stream = UnixStream::connect(runtime.join(format!("{display}.ipc")))?;
    let request = Request {
        version: PROTOCOL_VERSION,
        command: IpcCommand::GetState,
    };
    stream.write_all(ron::to_string(&request)?.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;
    let mut payload = String::new();
    stream.read_to_string(&mut payload)?;
    match ron::from_str::<Response<DesktopSnapshot>>(&payload)? {
        Response::Ok(snapshot) => print!("{}", format_overview(&snapshot)),
        Response::Error { code, message } => bail!("{code:?}: {message}"),
    }
    Ok(())
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

fn format_workspace(workspace: &WorkspaceSnapshot) -> String {
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
        workspace.focused_window.map(|id| id.0),
        workspace.tiled_count,
        workspace.floating_count,
        workspace.fullscreen.map(|id| id.0),
    )
}

#[cfg(test)]
mod tests {
    use astera_core::{OutputId, OutputTransform, Scale120, Size, WindowId, WorkspaceId};
    use astera_ipc::{DesktopSnapshot, OutputSnapshot, WorkspaceSnapshot};

    use super::format_overview;

    #[test]
    fn overview_marks_active_output_and_background_workspace() {
        let snapshot = DesktopSnapshot {
            active_output: Some(OutputId(2)),
            primary_output: Some(OutputId(2)),
            outputs: vec![OutputSnapshot {
                id: OutputId(2),
                stable_key: "DP-1".to_owned(),
                active_workspace: WorkspaceId(3),
                workspaces: vec![WorkspaceId(3), WorkspaceId(4)],
                physical_size: Size::new(3840, 2160),
                logical_size: Size::new(2560, 1440),
                native_scale: Scale120(180),
                transform: OutputTransform::Normal,
            }],
            workspaces: vec![
                WorkspaceSnapshot {
                    id: WorkspaceId(3),
                    name: Some("code".into()),
                    original_output: Some("DP-1".into()),
                    output: Some(OutputId(2)),
                    local_index: Some(1),
                    focused_window: Some(WindowId(9)),
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
                    focused_window: None,
                    tiled_count: 1,
                    floating_count: 0,
                    fullscreen: None,
                },
            ],
        };

        let overview = format_overview(&snapshot);
        assert!(overview.contains("DP-1 *: workspace 3, 2 total, 2560x1440 logical, 1.50x"));
        assert!(overview.contains("4: background"));
    }
}
