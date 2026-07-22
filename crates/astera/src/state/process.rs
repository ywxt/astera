pub(super) fn spawn(argv: Vec<String>) -> Result<(), String> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| "Spawn requires a non-empty argv".to_owned())?;
    let mut child = std::process::Command::new(program)
        .args(arguments)
        .spawn()
        .map_err(|error| format!("could not spawn {program:?}: {error}"))?;
    let program = program.clone();
    // Never wait on the compositor thread. The detached waiter prevents zombie processes while
    // leaving the child independent from the compositor's shutdown lifecycle.
    std::thread::spawn(move || match child.wait() {
        Ok(status) => tracing::debug!(%program, ?status, "spawned process exited"),
        Err(error) => tracing::warn!(%program, %error, "could not reap spawned process"),
    });
    Ok(())
}
