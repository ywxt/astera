pub mod backend;
mod cli;
mod ipc_server;
mod state;

use anyhow::{Context, Result, bail};
use astera_config::Config;
use clap::Parser;

use crate::cli::{BackendKind, LaunchOptions};

fn main() -> Result<()> {
    init_tracing();
    let options = LaunchOptions::parse();
    let explicit = options.config.is_some();
    let config_path = options.config.clone().unwrap_or(default_config_path()?);
    // An explicit missing path is a user error; an absent conventional path starts with defaults
    // and remains watchable so creating the file later activates it without a restart.
    let config = if config_path.exists() {
        Config::load(&config_path)?
    } else if explicit {
        bail!("configuration file {:?} does not exist", config_path);
    } else {
        Config::default()
    };
    match options.effective_backend() {
        BackendKind::Winit => backend::winit::run(config, config_path),
        BackendKind::Native => backend::native::run(config, config_path),
    }
}

fn default_config_path() -> Result<std::path::PathBuf> {
    if let Some(directory) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(std::path::PathBuf::from(directory).join("astera/config.kdl"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(std::path::PathBuf::from(home).join(".config/astera/config.kdl"))
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("astera=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
