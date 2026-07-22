use std::{sync::Arc, time::Instant};

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
    let ipc = IpcServer::bind(&socket_name)?;
    let started = Instant::now();
    let mut tracked_size = backend.window_size();
    let mut damage_tracker = OutputDamageTracker::new(tracked_size, 1.0, Transform::Flipped180);

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
    }
}
