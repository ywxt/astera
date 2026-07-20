mod backend;
mod ipc_server;
mod state;

use std::error::Error;

use astera_config::Config;

use crate::backend::BackendKind;

fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();
    match BackendKind::from_args()? {
        BackendKind::Winit => backend::winit::run(Config::default()),
        BackendKind::Native => backend::native::run(Config::default()),
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("astera=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
