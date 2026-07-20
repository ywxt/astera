mod ipc_server;
mod state;

use std::{error::Error, sync::Arc, time::Instant};

use ::winit::platform::pump_events::PumpStatus;
use astera_config::Config;
use astera_ipc::Command;
use smithay::{
    backend::{
        renderer::{
            Color32F, Frame, Renderer,
            element::{
                Kind,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
            utils::draw_render_elements,
        },
        winit::{self, WinitEvent},
    },
    desktop::PopupManager,
    reexports::wayland_server::{Display, ListeningSocket},
    utils::{Physical, Point, Rectangle, Transform},
};

use crate::ipc_server::IpcServer;
use crate::state::{Astera, ClientState, send_frames_surface_tree};

fn main() -> Result<(), Box<dyn Error>> {
    init_tracing();
    run_nested(Config::default())
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("astera=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn run_nested(config: Config) -> Result<(), Box<dyn Error>> {
    let mut display: Display<Astera> = Display::new()?;
    let mut state = Astera::new(&display.handle(), config);
    tracing::debug!(state = ?state.execute_command(Command::GetState), "initial desktop state");
    let listener = ListeningSocket::bind_auto("astera", 1..32)?;
    let socket_name = listener
        .socket_name()
        .ok_or("Wayland listening socket has no name")?
        .to_string_lossy()
        .into_owned();
    let (mut backend, mut event_loop) = winit::init::<GlesRenderer>()?;
    let ipc = IpcServer::bind(&socket_name)?;
    let started = Instant::now();

    tracing::info!(
        wayland_display = %socket_name,
        "nested compositor is ready"
    );
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
        state.remove_dead_windows();

        let size = backend.window_size();
        state.update_output_size(i64::from(size.w), i64::from(size.h));
        let damage = Rectangle::from_size(size);
        {
            let (renderer, mut framebuffer) = backend.bind()?;
            let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = state
                .render_roots()
                .into_iter()
                .flat_map(|(surface, location, scale)| {
                    let mut elements = Vec::new();
                    for (popup, popup_offset) in PopupManager::popups_for_surface(&surface) {
                        let geometry = popup.geometry();
                        let offset: Point<i32, Physical> = (
                            ((popup_offset.x - geometry.loc.x) as f64 * scale).round() as i32,
                            ((popup_offset.y - geometry.loc.y) as f64 * scale).round() as i32,
                        )
                            .into();
                        elements.extend(render_elements_from_surface_tree(
                            renderer,
                            popup.wl_surface(),
                            location + offset,
                            scale,
                            1.0,
                            Kind::Unspecified,
                        ));
                    }
                    elements.extend(render_elements_from_surface_tree(
                        renderer,
                        &surface,
                        location,
                        scale,
                        1.0,
                        Kind::Unspecified,
                    ));
                    elements
                })
                .collect();

            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(Color32F::new(0.025, 0.035, 0.06, 1.0), &[damage])?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])?;
            let _sync = frame.finish()?;

            let frame_time = started.elapsed().as_millis() as u32;
            for (surface, _, _) in state.render_roots() {
                send_frames_surface_tree(&surface, frame_time);
                for (popup, _) in PopupManager::popups_for_surface(&surface) {
                    send_frames_surface_tree(popup.wl_surface(), frame_time);
                }
            }
            display.flush_clients()?;
        }
        backend.submit(Some(&[damage]))?;
    }
}
