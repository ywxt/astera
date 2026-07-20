use astera_config::Config;
use astera_core::{Workspace, WorkspaceId};

fn main() {
    let config = Config::default();
    let workspaces: Vec<_> = (0..config.workspace_count)
        .map(|id| Workspace::new(WorkspaceId(id)))
        .collect();

    println!(
        "Astera {} — initialized {} infinite workspaces (Wayland backend pending)",
        env!("CARGO_PKG_VERSION"),
        workspaces.len()
    );
}
