pub mod native;
pub mod winit;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    Winit,
    Native,
}

impl BackendKind {
    pub fn from_args() -> Result<Self, String> {
        let mut args = std::env::args().skip(1);
        let Some(argument) = args.next() else {
            return Ok(Self::Winit);
        };
        if args.next().is_some() {
            return Err("usage: astera [--backend=winit|native]".to_owned());
        }
        match argument.as_str() {
            "--backend=winit" | "--nested" => Ok(Self::Winit),
            "--backend=native" => Ok(Self::Native),
            _ => Err(format!(
                "unknown argument {argument:?}; usage: astera [--backend=winit|native]"
            )),
        }
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
