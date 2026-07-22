use anyhow::{Context, Result};

pub(super) fn spawn(argv: Vec<String>) -> Result<()> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("Spawn requires a non-empty argv"))?;
    let mut child = std::process::Command::new(program)
        .args(arguments)
        .spawn()
        .with_context(|| format!("could not spawn {program:?}"))?;
    let program = program.clone();
    // Never wait on the compositor thread. The detached waiter prevents zombie processes while
    // leaving the child independent from the compositor's shutdown lifecycle.
    std::thread::spawn(move || match child.wait() {
        Ok(status) => tracing::debug!(%program, ?status, "spawned process exited"),
        Err(error) => tracing::warn!(%program, %error, "could not reap spawned process"),
    });
    Ok(())
}
