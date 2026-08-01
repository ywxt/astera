use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use astera_config::Config;
use astera_ipc::Command;
use smithay::{
    backend::{
        renderer::{
            Color32F, ImportDma,
            damage::OutputDamageTracker,
            element::{Kind, surface::WaylandSurfaceRenderElement},
            gles::GlesRenderer,
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    desktop::PopupManager,
    reexports::{calloop::EventLoop, wayland_server::Display},
    utils::{Physical, Point, Transform},
};

use crate::{
    backend::{
        render::surface_tree_snapshot,
        sources::{ReadableFdSource, WaylandSocketSource},
    },
    ipc_server::IpcServer,
    state::{Astera, ClientState, complete_frame_callbacks},
};

const MAX_EVENTS_PER_BATCH: usize = 256;
const MAX_QUEUED_SOURCE_EVENTS: usize = 4096;
const SOURCE_WARN_AFTER: Duration = Duration::from_millis(8);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const FRAME_RETRY_INITIAL: Duration = Duration::from_millis(16);
const FRAME_RETRY_MAX: Duration = Duration::from_secs(1);

enum RuntimeEvent {
    Winit(WinitEvent),
    WaylandClient(std::os::unix::net::UnixStream),
    WaylandReady,
    IpcReady,
    ConfigChanged,
}

struct WinitLoop {
    display: Display<Astera>,
    state: Astera,
    ipc: IpcServer,
    backend: WinitGraphicsBackend<GlesRenderer>,
    events: VecDeque<RuntimeEvent>,
    damage_tracker: OutputDamageTracker,
    tracked_size: smithay::utils::Size<i32, Physical>,
    started: Instant,
    present_ready: bool,
    exiting: bool,
    shutdown_deadline: Option<Instant>,
    frame_retry_deadline: Option<Instant>,
    frame_retry_delay: Duration,
    host_configured: bool,
    has_presented: bool,
}

impl WinitLoop {
    fn enqueue(&mut self, event: RuntimeEvent) {
        let duplicate = self.events.iter().any(|queued| {
            matches!(
                (&event, queued),
                (
                    RuntimeEvent::Winit(WinitEvent::Redraw),
                    RuntimeEvent::Winit(WinitEvent::Redraw)
                ) | (RuntimeEvent::WaylandReady, RuntimeEvent::WaylandReady)
                    | (RuntimeEvent::IpcReady, RuntimeEvent::IpcReady)
                    | (RuntimeEvent::ConfigChanged, RuntimeEvent::ConfigChanged)
            )
        });
        if duplicate {
            return;
        }
        if self.events.len() >= MAX_QUEUED_SOURCE_EVENTS {
            // Preserve lifecycle/readiness events by sacrificing the oldest
            // high-rate motion sample. Press/release and touch lifecycle events
            // must never be discarded because that can leave compositor state stuck.
            if let Some(index) = self.events.iter().position(is_lossy_motion) {
                self.events.remove(index);
            } else if is_lossy_motion(&event) {
                tracing::warn!("runtime source queue is full; dropping a lossy source event");
                return;
            } else {
                // The bound is deliberately soft for discrete input. Preserving a
                // release/up event is more important than strict memory accounting;
                // the 256-event reducer budget drains this exceptional backlog.
                tracing::warn!("runtime source queue overflowed with lifecycle events");
            }
        }
        self.events.push_back(event);
    }

    fn process_events(&mut self) -> Result<()> {
        let now = Instant::now();
        let mut scene_changed = self
            .state
            .next_timer_deadline()
            .is_some_and(|deadline| deadline <= now);
        if self
            .frame_retry_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            self.frame_retry_deadline = None;
            scene_changed = true;
        }
        for _ in 0..MAX_EVENTS_PER_BATCH {
            let Some(event) = self.events.pop_front() else {
                break;
            };
            match event {
                RuntimeEvent::Winit(WinitEvent::Input(event)) => {
                    self.state.process_input(event);
                    scene_changed = true;
                }
                RuntimeEvent::Winit(WinitEvent::Resized { size, .. }) => {
                    // A Wayland-hosted winit EGL surface is not valid before its
                    // first non-zero configure. Rendering earlier causes
                    // EGL_BAD_SURFACE during buffer-age queries.
                    self.host_configured = size.w > 0 && size.h > 0;
                    self.has_presented = false;
                    self.present_ready = false;
                    scene_changed |= self.host_configured;
                }
                RuntimeEvent::Winit(WinitEvent::Redraw) => {
                    // Treat Redraw as the host presentation grant. This also
                    // forces the first frame after an unreported occlusion to be
                    // complete; Smithay 0.7 does not forward Occluded itself.
                    if self.host_configured {
                        self.reset_damage_tracker();
                        self.present_ready = true;
                    } else {
                        tracing::debug!("ignoring redraw before host window configure");
                    }
                }
                RuntimeEvent::Winit(WinitEvent::CloseRequested) => self.exiting = true,
                RuntimeEvent::Winit(WinitEvent::Focus(focused)) => {
                    if focused {
                        // Smithay 0.7 does not expose winit's Occluded event. Focus
                        // restoration is therefore a conservative full-repaint signal.
                        self.reset_damage_tracker();
                        scene_changed = true;
                    }
                }
                RuntimeEvent::WaylandClient(stream) => {
                    if let Err(error) = self
                        .display
                        .handle()
                        .insert_client(stream, Arc::new(ClientState::default()))
                    {
                        tracing::warn!(%error, "could not insert Wayland client");
                    } else {
                        self.dispatch_wayland_clients()?;
                        scene_changed = true;
                    }
                }
                RuntimeEvent::WaylandReady => {
                    self.dispatch_wayland_clients()?;
                    scene_changed = true;
                }
                RuntimeEvent::IpcReady => {
                    self.ipc.dispatch(&mut self.state);
                    scene_changed = true;
                }
                RuntimeEvent::ConfigChanged => self.state.notify_config_changed(),
            }
        }

        self.ipc.expire(now);
        self.state.poll_config();
        self.state.process_key_repeats();
        self.state.remove_dead_windows();
        let size = self.backend.window_size();
        self.state
            .update_output_size(i64::from(size.w), i64::from(size.h));
        if size != self.tracked_size {
            self.tracked_size = size;
            self.reset_damage_tracker();
            scene_changed = true;
        }
        self.ipc.finish_tick(&mut self.state);
        // Protocol replies, especially the initial xdg configure, are independent of visual
        // damage and must leave the compositor in this transaction.
        if let Err(error) = self.display.flush_clients() {
            tracing::warn!(%error, "could not flush Wayland clients");
        }

        if self.host_configured
            && scene_changed
            && !self.present_ready
            && self.shutdown_deadline.is_none()
        {
            // request_redraw is coalesced by winit itself. We intentionally do
            // not retain a local latch: a host that suppressed a redraw while
            // hidden must be able to accept a later request after restoration.
            self.backend.window().request_redraw();
        }
        if self.present_ready {
            self.present_ready = false;
            if let Err(error) = self.render_and_present() {
                // A transient EGL/window error must not take down IPC and all
                // clients. The next state change retries from a full frame.
                tracing::warn!(%error, "nested frame submission failed");
                self.reset_damage_tracker();
                self.has_presented = false;
                self.frame_retry_deadline = Some(Instant::now() + self.frame_retry_delay);
                self.frame_retry_delay = (self.frame_retry_delay * 2).min(FRAME_RETRY_MAX);
            } else {
                self.frame_retry_deadline = None;
                self.frame_retry_delay = FRAME_RETRY_INITIAL;
            }
        }
        Ok(())
    }

    fn reset_damage_tracker(&mut self) {
        self.damage_tracker =
            OutputDamageTracker::new(self.tracked_size, 1.0, Transform::Flipped180);
    }

    fn dispatch_wayland_clients(&mut self) -> Result<()> {
        let started = Instant::now();
        self.display.dispatch_clients(&mut self.state)?;
        let elapsed = started.elapsed();
        if elapsed >= SOURCE_WARN_AFTER {
            // wayland-server 0.31 exposes an all-clients dispatch operation but
            // no preemptible budget. Keep the readiness callback lightweight and
            // make pathological client batches visible to operators.
            tracing::warn!(?elapsed, "Wayland client dispatch exceeded source budget");
        }
        Ok(())
    }

    fn render_and_present(&mut self) -> Result<()> {
        // EGL buffer age is only defined after this surface has completed a
        // swap. Querying it on the first frame produces EGL_BAD_SURFACE on
        // some Wayland/Mesa combinations.
        let buffer_age = if self.has_presented {
            self.backend.buffer_age().unwrap_or(0)
        } else {
            0
        };
        let roots = self.state.render_roots();
        let mut frame_callbacks = Vec::new();
        let damage = {
            let (renderer, mut framebuffer) = self.backend.bind()?;
            self.state.validate_dmabuf_imports(renderer);
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
                        let popup_elements = surface_tree_snapshot(
                            renderer,
                            popup.wl_surface(),
                            *location + offset,
                            *scale,
                            1.0,
                            Kind::Unspecified,
                            &mut frame_callbacks,
                        );
                        elements.extend(popup_elements);
                    }
                    let root_elements = surface_tree_snapshot(
                        renderer,
                        surface,
                        *location,
                        *scale,
                        1.0,
                        Kind::Unspecified,
                        &mut frame_callbacks,
                    );
                    elements.extend(root_elements);
                    elements
                })
                .collect();
            self.damage_tracker
                .render_output(
                    renderer,
                    &mut framebuffer,
                    buffer_age,
                    &elements,
                    Color32F::new(0.025, 0.035, 0.06, 1.0),
                )?
                .damage
                .cloned()
        };

        // A frame callback without visual damage still needs a real host presentation
        // opportunity. Swap even when the damage tracker returns None.
        self.backend.submit(damage.as_deref())?;
        self.has_presented = true;
        let frame_time = self.started.elapsed().as_millis() as u32;
        complete_frame_callbacks(&frame_callbacks, frame_time);
        self.display.flush_clients()?;
        Ok(())
    }

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.ipc.next_timeout(),
            self.state.next_timer_deadline(),
            self.shutdown_deadline,
            self.frame_retry_deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn begin_shutdown(&mut self) {
        if self.shutdown_deadline.is_none() {
            self.shutdown_deadline = Some(Instant::now() + SHUTDOWN_GRACE);
            tracing::info!("draining IPC replies before nested compositor shutdown");
        }
    }

    fn shutdown_complete(&self) -> bool {
        self.shutdown_deadline
            .is_some_and(|deadline| !self.ipc.has_pending_output() || Instant::now() >= deadline)
    }
}

fn is_lossy_motion(event: &RuntimeEvent) -> bool {
    matches!(
        event,
        RuntimeEvent::Winit(WinitEvent::Input(
            smithay::backend::input::InputEvent::PointerMotion { .. }
                | smithay::backend::input::InputEvent::PointerMotionAbsolute { .. }
        ))
    )
}

pub fn run(config: Config, config_path: std::path::PathBuf) -> Result<()> {
    let mut event_loop: EventLoop<WinitLoop> = EventLoop::try_new()?;
    let display: Display<Astera> = Display::new()?;
    let mut state = Astera::new(&display.handle(), config);
    state.watch_config(config_path);
    tracing::debug!(state = ?state.execute_command(Command::GetState), "initial desktop state");
    let listener = WaylandSocketSource::bind_auto("astera", 1..32)?;
    let socket_name = listener
        .socket_name()
        .context("Wayland listening socket has no name")?
        .to_string_lossy()
        .into_owned();
    let (mut backend, winit_events) =
        winit::init::<GlesRenderer>().map_err(|error| anyhow!(error.to_string()))?;
    {
        let (renderer, _) = backend.bind()?;
        state.enable_dmabuf(renderer.dmabuf_formats());
    }
    let ipc = IpcServer::bind(&socket_name)?;
    let tracked_size = backend.window_size();
    let damage_tracker = OutputDamageTracker::new(tracked_size, 1.0, Transform::Flipped180);

    event_loop
        .handle()
        .insert_source(winit_events, |event, _, runtime| {
            runtime.enqueue(RuntimeEvent::Winit(event));
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(listener, |stream, _, runtime| {
            runtime.enqueue(RuntimeEvent::WaylandClient(stream));
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(ReadableFdSource::duplicate(&display)?, |(), _, runtime| {
            runtime.enqueue(RuntimeEvent::WaylandReady);
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(ipc.event_source(), |(), _, runtime| {
            runtime.enqueue(RuntimeEvent::IpcReady);
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    if let Some(config_fd) = state.config_watch_fd()? {
        event_loop
            .handle()
            .insert_source(ReadableFdSource::new(config_fd), |(), _, runtime| {
                runtime.enqueue(RuntimeEvent::ConfigChanged);
            })
            .map_err(|error| anyhow!(error.to_string()))?;
    }

    let mut runtime = WinitLoop {
        display,
        state,
        ipc,
        backend,
        events: VecDeque::new(),
        damage_tracker,
        tracked_size,
        started: Instant::now(),
        present_ready: false,
        exiting: false,
        shutdown_deadline: None,
        frame_retry_deadline: None,
        frame_retry_delay: FRAME_RETRY_INITIAL,
        host_configured: false,
        has_presented: false,
    };

    tracing::info!(wayland_display = %socket_name, "nested compositor is ready");
    println!("WAYLAND_DISPLAY={socket_name}");

    while !runtime.exiting && !runtime.shutdown_complete() {
        let timeout = if runtime.events.is_empty() {
            runtime
                .next_deadline()
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
        } else {
            Some(Duration::ZERO)
        };
        event_loop.dispatch(timeout, &mut runtime)?;
        runtime.process_events()?;
        if runtime.state.should_quit() {
            runtime.begin_shutdown();
        }
    }
    tracing::info!("nested compositor is shutting down");
    Ok(())
}
