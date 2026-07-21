use std::{
    env,
    error::Error,
    io::{Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::PathBuf,
};

use astera_ipc::{
    Command, DesktopSnapshot, PROTOCOL_VERSION, Request, Response, WorkspaceSnapshot,
};

fn main() -> Result<(), Box<dyn Error>> {
    let command = env::args().nth(1).unwrap_or_else(|| "overview".to_owned());
    if command != "overview" {
        return Err(format!("unknown command {command:?}; usage: astera-msg [overview]").into());
    }
    let display = env::var("WAYLAND_DISPLAY")?;
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    let mut stream = UnixStream::connect(runtime.join(format!("{display}.ipc")))?;
    let request = Request {
        version: PROTOCOL_VERSION,
        command: Command::GetState,
    };
    stream.write_all(ron::to_string(&request)?.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;
    let mut payload = String::new();
    stream.read_to_string(&mut payload)?;
    match ron::from_str::<Response<DesktopSnapshot>>(&payload)? {
        Response::Ok(snapshot) => print!("{}", format_overview(&snapshot)),
        Response::Error { code, message } => return Err(format!("{code:?}: {message}").into()),
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
        let workspace = output
            .workspace
            .map(|id| id.0.to_string())
            .unwrap_or_else(|| "none".to_owned());
        let scale = output.native_scale.0 as f64 / 120.0;
        text.push_str(&format!(
            "  {}{}: workspace {}, {}x{} logical, {:.2}x, {:?}\n",
            output.stable_key,
            active,
            workspace,
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
        .map(|id| format!("output {}", id.0))
        .unwrap_or_else(|| "background".to_owned());
    format!(
        "  {}: {}, focus {:?}, tiled {}, floating {}, fullscreen {:?}\n",
        workspace.id.0,
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
            outputs: vec![OutputSnapshot {
                id: OutputId(2),
                stable_key: "DP-1".to_owned(),
                workspace: Some(WorkspaceId(3)),
                physical_size: Size::new(3840, 2160),
                logical_size: Size::new(2560, 1440),
                native_scale: Scale120(180),
                transform: OutputTransform::Normal,
            }],
            workspaces: vec![
                WorkspaceSnapshot {
                    id: WorkspaceId(3),
                    output: Some(OutputId(2)),
                    focused_window: Some(WindowId(9)),
                    tiled_count: 2,
                    floating_count: 1,
                    fullscreen: None,
                },
                WorkspaceSnapshot {
                    id: WorkspaceId(4),
                    output: None,
                    focused_window: None,
                    tiled_count: 1,
                    floating_count: 0,
                    fullscreen: None,
                },
            ],
        };

        let overview = format_overview(&snapshot);
        assert!(overview.contains("DP-1 *: workspace 3, 2560x1440 logical, 1.50x"));
        assert!(overview.contains("4: background"));
    }
}
