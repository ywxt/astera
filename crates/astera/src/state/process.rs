use std::{
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
use smithay::reexports::{
    calloop::channel::{self, Channel, Sender},
    wayland_server::DisplayHandle,
};

use super::ClientState;

pub(super) fn spawn(argv: Vec<String>) -> Result<()> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("Spawn requires a non-empty argv"))?;
    let mut child = std::process::Command::new(program)
        .args(arguments)
        .spawn()
        .with_context(|| format!("could not spawn {program:?}"))?;
    let program = program.clone();
    std::thread::spawn(move || match child.wait() {
        Ok(status) => tracing::debug!(%program, ?status, "spawned process exited"),
        Err(error) => tracing::warn!(%program, %error, "could not reap spawned process"),
    });
    Ok(())
}

/// Owns the privileged input-service lifecycle. Child waiting happens off-thread, but every
/// Wayland client insertion and restart decision stays on the compositor event loop.
pub(crate) struct InputServiceExit {
    generation: u64,
    lifetime: Duration,
}

pub(crate) struct InputServiceSupervisor {
    argv: Vec<String>,
    exit_sender: Sender<InputServiceExit>,
    running: bool,
    stopping_for_lock: bool,
    child_pidfd: Option<std::os::fd::OwnedFd>,
    generation: u64,
    restart_at: Option<Instant>,
    failures: u32,
}

impl InputServiceSupervisor {
    pub(crate) fn new(argv: Vec<String>) -> (Self, Channel<InputServiceExit>) {
        let (exit_sender, exits) = channel::channel();
        (
            Self {
                argv,
                exit_sender,
                running: false,
                stopping_for_lock: false,
                child_pidfd: None,
                generation: 0,
                restart_at: None,
                failures: 0,
            },
            exits,
        )
    }

    pub(crate) fn start(&mut self, display: DisplayHandle) -> Result<()> {
        let (program, arguments) = self
            .argv
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("input-service requires a non-empty argv"))?;
        let (server, client) =
            UnixStream::pair().context("could not create private input socket")?;
        let original_flags = fcntl_getfd(&client)?;
        fcntl_setfd(&client, original_flags - FdFlags::CLOEXEC)?;
        let mut command = std::process::Command::new(program);
        command
            .args(arguments)
            .env_remove("WAYLAND_DISPLAY")
            .env("WAYLAND_SOCKET", client.as_raw_fd().to_string());
        let spawned = command.spawn();
        fcntl_setfd(&client, original_flags)?;
        let mut child =
            spawned.with_context(|| format!("could not spawn input service {program:?}"))?;
        let pid = rustix::process::Pid::from_raw(child.id() as i32)
            .ok_or_else(|| anyhow::anyhow!("input service returned an invalid process id"))?;
        let pidfd = match rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()) {
            Ok(pidfd) => pidfd,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("could not create a race-free input-service handle");
            }
        };
        drop(client);
        let mut insertion_display = display;
        if let Err(error) =
            insertion_display.insert_client(server, Arc::new(ClientState::trusted_input()))
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow::anyhow!(error)).context("could not register trusted input client");
        }
        let sender = self.exit_sender.clone();
        let program = program.clone();
        let reaper_program = program.clone();
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let started = Instant::now();
        self.child_pidfd = Some(pidfd);
        std::thread::spawn(move || {
            match child.wait() {
                Ok(status) => tracing::info!(%reaper_program, ?status, "input service exited"),
                Err(error) => {
                    tracing::warn!(%reaper_program, %error, "could not reap input service")
                }
            }
            let _ = sender.send(InputServiceExit {
                generation,
                lifetime: started.elapsed(),
            });
        });
        self.running = true;
        self.stopping_for_lock = false;
        self.restart_at = None;
        tracing::info!(%program, "spawned trusted input service");
        Ok(())
    }

    pub(crate) fn exited(&mut self, exit: InputServiceExit) {
        if exit.generation != self.generation {
            return;
        }
        self.running = false;
        self.child_pidfd = None;
        if self.stopping_for_lock {
            // Policy-driven revocation is not a crash. Keep crash history unchanged and make the
            // replacement immediately eligible once the lock state becomes unlocked.
            self.restart_at = Some(Instant::now());
            return;
        } else if exit.lifetime >= Duration::from_secs(30) {
            self.failures = 0;
        } else {
            self.failures = self.failures.saturating_add(1);
        }
        let shift = self.failures.min(5);
        self.restart_at = Some(Instant::now() + Duration::from_millis(250 * (1_u64 << shift)));
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.restart_at
    }

    pub(crate) fn poll(&mut self, display: DisplayHandle, locked: bool) {
        if locked && self.running && !self.stopping_for_lock {
            // This process is compositor-owned and holds an input-injection capability. Killing it
            // makes lock revocation deterministic even if the daemon ignores its Wayland error.
            if let Some(pidfd) = self.child_pidfd.as_ref()
                && let Err(error) =
                    rustix::process::pidfd_send_signal(pidfd, rustix::process::Signal::KILL)
            {
                tracing::warn!(%error, "could not stop input service for session lock");
            }
            self.stopping_for_lock = true;
        }
        if self.running || locked || self.restart_at.is_none_or(|at| at > Instant::now()) {
            return;
        }
        if let Err(error) = self.start(display) {
            tracing::error!(%error, "could not restart trusted input service");
            self.failures = self.failures.saturating_add(1);
            let shift = self.failures.min(5);
            self.restart_at = Some(Instant::now() + Duration::from_millis(250 * (1_u64 << shift)));
        }
    }
}
