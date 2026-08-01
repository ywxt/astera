use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum BackendKind {
    #[default]
    Winit,
    Native,
}

#[derive(Clone, Debug, Parser)]
#[command(version, about = "An infinite-canvas Wayland compositor")]
pub struct LaunchOptions {
    #[arg(long, value_enum, default_value_t)]
    pub backend: BackendKind,

    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[arg(long, hide = true)]
    nested: bool,
}

impl LaunchOptions {
    pub fn effective_backend(&self) -> BackendKind {
        if self.nested {
            BackendKind::Winit
        } else {
            self.backend
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_backend_config_and_legacy_nested_alias() {
        let options = LaunchOptions::try_parse_from([
            "astera",
            "--backend=native",
            "--config",
            "/tmp/astera.kdl",
        ])
        .unwrap();
        assert_eq!(options.effective_backend(), BackendKind::Native);
        assert_eq!(options.config, Some(PathBuf::from("/tmp/astera.kdl")));

        let nested = LaunchOptions::try_parse_from(["astera", "--nested"]).unwrap();
        assert_eq!(nested.effective_backend(), BackendKind::Winit);
    }
}
