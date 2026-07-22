mod backend;
mod ipc_server;
mod state;

use std::error::Error;

use astera_config::Config;

use crate::backend::{BackendKind, LaunchOptions};

fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();
    let options = LaunchOptions::from_args()?;
    let explicit = options.config_path.is_some();
    let config_path = options.config_path.unwrap_or(default_config_path()?);
    let config = if config_path.exists() {
        Config::load(&config_path)?
    } else if explicit {
        return Err(format!("configuration file {:?} does not exist", config_path).into());
    } else {
        Config::default()
    };
    match options.backend {
        BackendKind::Winit => backend::winit::run(config, config_path),
        BackendKind::Native => backend::native::run(config, config_path),
    }
}

fn default_config_path() -> Result<std::path::PathBuf, Box<dyn Error>> {
    if let Some(directory) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(std::path::PathBuf::from(directory).join("astera/config.ron"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(std::path::PathBuf::from(home).join(".config/astera/config.ron"))
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("astera=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
