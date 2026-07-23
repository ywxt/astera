use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use ::winit::platform::pump_events::PumpStatus;
use anyhow::{Context, Result, anyhow};
use astera_config::Config;
use astera_ipc::Command;
use smithay::{
    backend::{
        renderer::{
            Color32F, ImportDma,
            damage::OutputDamageTracker,
            element::{
                Kind,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
        },
        winit::{self, WinitEvent},
    },
    desktop::PopupManager,
    reexports::wayland_server::{Display, ListeningSocket},
    utils::{Physical, Point, Transform},
};

use crate::{
    ipc_server::IpcServer,
    state::{Astera, ClientState, send_frames_surface_tree},
};

const NESTED_REFRESH_HZ: u32 = 60;
const IPC_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Paces the polling winit backend without coupling compositor progress to host redraw events.
///
/// Smithay intentionally pumps winit with a zero timeout. Without an explicit deadline the outer
/// loop busy-spins even when no surface is changing. Missing a deadline resets from `now` rather
/// than trying to catch up, which prevents a slow frame from causing a burst of unpaced frames.
struct FramePacer {
    interval: Duration,
    deadline: Instant,
}

impl FramePacer {
    fn new(refresh_hz: u32) -> Self {
        let interval = Duration::from_secs_f64(1.0 / f64::from(refresh_hz));
        Self {
            interval,
            deadline: Instant::now() + interval,
        }
    }

    fn delay_at(&mut self, now: Instant) -> Duration {
        if now >= self.deadline {
            self.deadline = now + self.interval;
            Duration::ZERO
        } else {
            let delay = self.deadline - now;
            self.deadline += self.interval;
            delay
        }
    }

    fn wait_with(&mut self, mut service: impl FnMut()) {
        let now = Instant::now();
        let delay = self.delay_at(now);
        let target = now + delay;
        while let Some(remaining) = target.checked_duration_since(Instant::now()) {
            if remaining.is_zero() {
                break;
            }
            let delay = remaining.min(IPC_POLL_INTERVAL);
            thread::sleep(delay);
            service();
        }
    }
}

pub fn run(config: Config, config_path: std::path::PathBuf) -> Result<()> {
    let mut display: Display<Astera> = Display::new()?;
    let mut state = Astera::new(&display.handle(), config);
    state.watch_config(config_path);
    tracing::debug!(state = ?state.execute_command(Command::GetState), "initial desktop state");
    let listener = ListeningSocket::bind_auto("astera", 1..32)?;
    let socket_name = listener
        .socket_name()
        .context("Wayland listening socket has no name")?
        .to_string_lossy()
        .into_owned();
    let (mut backend, mut event_loop) =
        winit::init::<GlesRenderer>().map_err(|error| anyhow!(error.to_string()))?;
    {
        let (renderer, _) = backend.bind()?;
        state.enable_dmabuf(renderer.dmabuf_formats());
    }
    let mut ipc = IpcServer::bind(&socket_name)?;
    let started = Instant::now();
    let mut tracked_size = backend.window_size();
    let mut damage_tracker = OutputDamageTracker::new(tracked_size, 1.0, Transform::Flipped180);
    let mut frame_pacer = FramePacer::new(NESTED_REFRESH_HZ);

    tracing::info!(wayland_display = %socket_name, "nested compositor is ready");
    println!("WAYLAND_DISPLAY={socket_name}");

    loop {
        match event_loop.dispatch_new_events(|event| match event {
            WinitEvent::Resized { .. } => {}
            WinitEvent::Input(event) => state.process_input(event),
            WinitEvent::Focus(_) | WinitEvent::Redraw | WinitEvent::CloseRequested => {}
        }) {
            PumpStatus::Continue => {}
            PumpStatus::Exit(_) => return Ok(()),
        }

        while let Some(stream) = listener.accept()? {
            display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))?;
        }
        display.dispatch_clients(&mut state)?;
        // Initial xdg configure and other protocol replies must not depend on render damage.
        // Waiting until submit() creates a cycle: the client cannot attach its first buffer until
        // it receives configure, while the compositor sees no surface damage until that attach.
        display.flush_clients()?;
        ipc.dispatch(&mut state);
        state.poll_config();
        if state.should_quit() {
            tracing::info!("quit action requested");
            return Ok(());
        }
        state.process_key_repeats();
        state.remove_dead_windows();

        let size = backend.window_size();
        state.update_output_size(i64::from(size.w), i64::from(size.h));
        ipc.finish_tick(&mut state);
        if size != tracked_size {
            tracked_size = size;
            damage_tracker = OutputDamageTracker::new(size, 1.0, Transform::Flipped180);
        }
        let buffer_age = backend.buffer_age().unwrap_or(0);
        let submitted_damage = {
            let (renderer, mut framebuffer) = backend.bind()?;
            state.validate_dmabuf_imports(renderer);
            // Scene construction includes layout transforms, popup discovery and sorting; retain
            // it for frame callbacks instead of rebuilding the same scene twice per frame.
            let roots = state.render_roots();
            let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = roots
                .iter()
                .flat_map(|(surface, location, scale)| {
                    let mut elements = Vec::new();
                    for (popup, popup_offset) in PopupManager::popups_for_surface(surface) {
                        let geometry = popup.geometry();
                        let offset: Point<i32, Physical> = (
                            ((popup_offset.x - geometry.loc.x) as f64 * scale).round() as i32,
                            ((popup_offset.y - geometry.loc.y) as f64 * scale).round() as i32,
                        )
                            .into();
                        elements.extend(render_elements_from_surface_tree(
                            renderer,
                            popup.wl_surface(),
                            *location + offset,
                            *scale,
                            1.0,
                            Kind::Unspecified,
                        ));
                    }
                    elements.extend(render_elements_from_surface_tree(
                        renderer,
                        surface,
                        *location,
                        *scale,
                        1.0,
                        Kind::Unspecified,
                    ));
                    elements
                })
                .collect();

            let result = damage_tracker.render_output(
                renderer,
                &mut framebuffer,
                buffer_age,
                &elements,
                Color32F::new(0.025, 0.035, 0.06, 1.0),
            )?;
            let submitted_damage = result.damage.cloned();

            if submitted_damage.is_some() {
                let frame_time = started.elapsed().as_millis() as u32;
                for (surface, _, _) in roots {
                    send_frames_surface_tree(&surface, frame_time);
                    for (popup, _) in PopupManager::popups_for_surface(&surface) {
                        send_frames_surface_tree(popup.wl_surface(), frame_time);
                    }
                }
                display.flush_clients()?;
            }
            submitted_damage
        };
        if let Some(damage) = submitted_damage {
            backend.submit(Some(&damage))?;
        }
        // Keep the expensive render loop at 60 Hz, but service the cooperative IPC executor
        // between frame deadlines. Bootstrap negotiation and command execution therefore do not
        // each accumulate another full-frame delay.
        frame_pacer.wait_with(|| {
            ipc.dispatch(&mut state);
            ipc.finish_tick(&mut state);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_pacer_waits_until_deadline_and_drops_missed_frames() {
        let interval = Duration::from_millis(16);
        let start = Instant::now();
        let mut pacer = FramePacer {
            interval,
            deadline: start + interval,
        };

        assert_eq!(pacer.delay_at(start), interval);
        assert_eq!(pacer.deadline, start + interval * 2);

        let late = start + interval * 4;
        assert_eq!(pacer.delay_at(late), Duration::ZERO);
        assert_eq!(pacer.deadline, late + interval);
    }
}
