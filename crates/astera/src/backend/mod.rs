pub mod native;
pub mod winit;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    Winit,
    Native,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchOptions {
    pub backend: BackendKind,
    pub config_path: Option<std::path::PathBuf>,
}

impl LaunchOptions {
    pub fn from_args() -> Result<Self, String> {
        let mut args = std::env::args().skip(1);
        let mut options = Self {
            backend: BackendKind::Winit,
            config_path: None,
        };
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--backend=winit" | "--nested" => options.backend = BackendKind::Winit,
                "--backend=native" => options.backend = BackendKind::Native,
                "--config" => {
                    options.config_path =
                        Some(args.next().ok_or("--config requires a path")?.into());
                }
                _ if argument.starts_with("--config=") => {
                    options.config_path = Some(argument["--config=".len()..].into());
                }
                _ => {
                    return Err(format!(
                        "unknown argument {argument:?}; usage: astera [--backend=winit|native] [--config PATH]"
                    ));
                }
            }
        }
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::BackendKind;

    #[test]
    fn backend_kind_has_distinct_native_and_nested_modes() {
        assert_ne!(BackendKind::Winit, BackendKind::Native);
    }
}
