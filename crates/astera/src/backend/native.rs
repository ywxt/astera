use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use astera_config::Config;
use astera_core::{Output, OutputId, Size};
use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode,
            compositor::{FrameFlags, PrimaryPlaneElement},
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::context::ContextPriority,
        input::InputEvent as BackendInputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            Color32F, ImportDma,
            element::{Kind, surface::WaylandSurfaceRenderElement},
            gles::GlesRenderer,
            multigpu::{GpuManager, MultiRenderer, gbm::GbmGlesBackend},
            sync::SyncPoint,
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent},
    },
    desktop::PopupManager,
    reexports::{
        calloop::{EventLoop, LoopHandle, RegistrationToken},
        drm::control::{Device as ControlDevice, ModeTypeFlags, connector, crtc, property::Value},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::Display,
    },
    utils::{DeviceFd, Physical, Point},
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use crate::{
    backend::{
        native_policy::{ConnectorSnapshot, ModeCandidate, SnapshotSource, scan_outputs},
        render::{FrameCallback, complete_frame_callbacks, surface_tree_snapshot},
        runtime::{RenderRequest, RepaintReasons, RepaintScheduler},
        sources::{OneShotReadableFdSource, ReadableFdSource, WaylandSocketSource},
    },
    ipc_server::IpcServer,
    state::{Astera, ClientState},
};

const MAX_NATIVE_EVENTS_PER_BATCH: usize = 256;
const MAX_NATIVE_FRAMES_PER_BATCH: usize = 4;
const MAX_QUEUED_NATIVE_EVENTS: usize = 4096;
const DEFAULT_RETRACE: Duration = Duration::from_millis(16);
const FENCE_RECHECK_INITIAL: Duration = Duration::from_millis(1);
const FENCE_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

struct NativeLoop {
    handle: LoopHandle<'static, NativeLoop>,
    display: Display<Astera>,
    state: Astera,
    ipc: IpcServer,
    events: VecDeque<NativeEvent>,
    libinput: Libinput,
    _session: LibSeatSession,
    devices: HashMap<DrmNode, NativeDevice>,
    connectors: HashMap<(DrmNode, connector::Handle), OutputId>,
    next_output_id: u32,
    gpus: GpuManager<GraphicsBackend>,
    started: Instant,
    scheduler: RepaintScheduler,
    wayland_ready_queued: bool,
    ipc_ready_queued: bool,
    config_changed_queued: bool,
    shutdown_deadline: Option<Instant>,
}

enum NativeEvent {
    Input(BackendInputEvent<LibinputInputBackend>),
    Session(SessionEvent),
    Udev(UdevEvent),
    Drm {
        node: DrmNode,
        event: DrmEvent,
    },
    FenceReady {
        node: DrmNode,
        crtc: crtc::Handle,
        frame_id: u64,
        callbacks: Vec<FrameCallback>,
    },
    WaylandClient(std::os::unix::net::UnixStream),
    WaylandReady,
    IpcReady,
    ConfigChanged,
}

type GraphicsBackend = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
type NativeRenderer<'a> = MultiRenderer<'a, 'a, GraphicsBackend, GraphicsBackend>;
type NativeOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, u64, DrmDeviceFd>;
type NativeOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    u64,
    DrmDeviceFd,
>;

fn edid_blob(device: &impl ControlDevice, connector: connector::Handle) -> Option<Vec<u8>> {
    let properties = device.get_properties(connector).ok()?;
    for (handle, raw) in properties.iter() {
        let info = device.get_property(*handle).ok()?;
        if info.name().to_bytes() != b"EDID" {
            continue;
        }
        let Value::Blob(blob) = info.value_type().convert_value(*raw) else {
            continue;
        };
        if blob == 0 {
            continue;
        }
        let edid = device.get_property_blob(blob).ok()?;
        if edid.len() < 16 {
            continue;
        }
        return Some(edid);
    }
    None
}

struct NativeDevice {
    output_manager: NativeOutputManager,
    scanner: DrmScanner,
    registration: RegistrationToken,
    outputs: HashMap<crtc::Handle, NativeSurface>,
}

struct NativeSurface {
    output: OutputId,
    drm: NativeOutput,
    pending: Option<PendingNativeFrame>,
    retrace: Duration,
    waiting_fence: Option<PendingGpuFence>,
    exported_fence: Option<PendingExportedFence>,
}

struct PendingNativeFrame {
    frame_id: u64,
    callbacks: Vec<FrameCallback>,
    queued: bool,
    deadline: Option<Instant>,
}

struct PendingGpuFence {
    frame_id: u64,
    callbacks: Vec<FrameCallback>,
    sync: SyncPoint,
    deadline: Instant,
    delay: Duration,
    expires_at: Instant,
}

struct PendingExportedFence {
    frame_id: u64,
    deadline: Instant,
    registration: RegistrationToken,
}

impl NativeLoop {
    fn enqueue(&mut self, event: NativeEvent) {
        match event {
            NativeEvent::WaylandReady if self.wayland_ready_queued => return,
            NativeEvent::IpcReady if self.ipc_ready_queued => return,
            NativeEvent::ConfigChanged if self.config_changed_queued => return,
            _ => {}
        }
        if self.events.len() >= MAX_QUEUED_NATIVE_EVENTS {
            if is_lossy_native_motion(&event) {
                tracing::warn!("native source queue is full; dropping a lossy event");
                return;
            } else {
                // calloop's upstream libinput/udev sources cannot be paused from
                // inside their callback. Preserve lifecycle ordering and the hard
                // memory bound by reducing one normal-sized batch before enqueue.
                let mut scene_changed = false;
                for _ in 0..MAX_NATIVE_EVENTS_PER_BATCH {
                    let Some(queued) = self.events.pop_front() else {
                        break;
                    };
                    match self.process_event(queued) {
                        Ok(changed) => scene_changed |= changed,
                        Err(error) => {
                            tracing::warn!(%error, "native overflow reduction failed")
                        }
                    }
                }
                if scene_changed {
                    self.request_all(RepaintReasons::DAMAGE);
                }
            }
        }
        match event {
            NativeEvent::WaylandReady => self.wayland_ready_queued = true,
            NativeEvent::IpcReady => self.ipc_ready_queued = true,
            NativeEvent::ConfigChanged => self.config_changed_queued = true,
            _ => {}
        }
        self.events.push_back(event);
    }

    fn process_events(&mut self) -> Result<()> {
        let mut scene_changed = false;
        for _ in 0..MAX_NATIVE_EVENTS_PER_BATCH {
            let Some(event) = self.events.pop_front() else {
                break;
            };
            scene_changed |= self.process_event(event)?;
        }
        if scene_changed {
            self.request_all(RepaintReasons::DAMAGE);
        }
        Ok(())
    }

    fn process_event(&mut self, event: NativeEvent) -> Result<bool> {
        let mut scene_changed = false;
        match event {
            NativeEvent::Input(event) => {
                self.state.process_input(event);
                scene_changed = true;
            }
            NativeEvent::Session(SessionEvent::PauseSession) => {
                self.libinput.suspend();
                self.scheduler.pause();
                for device in self.devices.values_mut() {
                    device.output_manager.pause();
                    for surface in device.outputs.values_mut() {
                        surface.pending = None;
                        surface.waiting_fence = None;
                        if let Some(fence) = surface.exported_fence.take() {
                            self.handle.remove(fence.registration);
                        }
                    }
                }
                tracing::info!("native session paused");
            }
            NativeEvent::Session(SessionEvent::ActivateSession) => {
                if let Err(error) = self.libinput.resume() {
                    tracing::error!(?error, "could not resume libinput");
                }
                for device in self.devices.values_mut() {
                    if let Err(error) = device.output_manager.activate(false) {
                        tracing::error!(?error, "could not reactivate DRM device");
                    }
                }
                self.scheduler.resume();
                tracing::info!("native session activated");
            }
            NativeEvent::Udev(UdevEvent::Added { device_id, path }) => {
                match DrmNode::from_dev_id(device_id) {
                    Ok(node) => {
                        if let Err(error) = self.device_added(node, &path) {
                            tracing::error!(?device_id, ?path, %error, "could not add DRM device");
                        }
                    }
                    Err(error) => tracing::error!(?device_id, %error, "invalid DRM device node"),
                }
            }
            NativeEvent::Udev(UdevEvent::Changed { device_id }) => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    self.device_changed(node);
                }
            }
            NativeEvent::Udev(UdevEvent::Removed { device_id }) => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    self.device_removed(node);
                }
            }
            NativeEvent::Drm {
                node,
                event: DrmEvent::VBlank(crtc),
            } => self.frame_submitted(node, crtc),
            NativeEvent::Drm {
                node,
                event: DrmEvent::Error(error),
            } => {
                tracing::error!(?node, ?error, "DRM event error");
                self.fail_device_frames(node);
            }
            NativeEvent::FenceReady {
                node,
                crtc,
                frame_id,
                callbacks,
            } => self.queue_prepared_frame(node, crtc, frame_id, callbacks),
            NativeEvent::WaylandClient(stream) => {
                self.display
                    .handle()
                    .insert_client(stream, Arc::new(ClientState::default()))?;
                self.display.dispatch_clients(&mut self.state)?;
                scene_changed = true;
            }
            NativeEvent::WaylandReady => {
                self.wayland_ready_queued = false;
                self.display.dispatch_clients(&mut self.state)?;
                scene_changed = true;
            }
            NativeEvent::IpcReady => {
                self.ipc_ready_queued = false;
                self.ipc.dispatch(&mut self.state);
                scene_changed = true;
            }
            NativeEvent::ConfigChanged => {
                self.config_changed_queued = false;
                self.state.notify_config_changed();
            }
        }
        Ok(scene_changed)
    }

    fn request_all(&mut self, reason: RepaintReasons) {
        let outputs = self
            .devices
            .values()
            .flat_map(|device| device.outputs.values().map(|surface| surface.output))
            .collect::<Vec<_>>();
        for output in outputs {
            self.scheduler.request(output, reason);
        }
    }

    fn begin_shutdown(&mut self) {
        if self.shutdown_deadline.is_none() {
            self.shutdown_deadline = Some(Instant::now() + SHUTDOWN_GRACE);
            self.scheduler.pause();
            tracing::info!("draining IPC replies before native compositor shutdown");
        }
    }

    fn shutdown_complete(&self) -> bool {
        self.shutdown_deadline
            .is_some_and(|deadline| !self.ipc.has_pending_output() || Instant::now() >= deadline)
    }

    fn device_added(&mut self, node: DrmNode, path: &Path) -> Result<()> {
        if self.devices.contains_key(&node) {
            return Ok(());
        }
        let fd = self._session.open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (drm, notifier) = DrmDevice::new(fd.clone(), true)?;
        let gbm = GbmDevice::new(fd.clone())?;
        self.gpus.as_mut().add_node(node, gbm.clone())?;
        let allocator = GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(node));
        let render_formats = self.gpus.single_renderer(&node)?.dmabuf_formats();
        self.state.enable_dmabuf(render_formats.clone());
        let output_manager = DrmOutputManager::new(
            drm,
            allocator,
            exporter,
            Some(gbm),
            [Fourcc::Argb8888, Fourcc::Abgr8888],
            render_formats,
        );
        let registration = self
            .handle
            .insert_source(notifier, move |event, _, runtime| {
                runtime.enqueue(NativeEvent::Drm { node, event });
            })?;
        self.devices.insert(
            node,
            NativeDevice {
                output_manager,
                scanner: DrmScanner::new(),
                registration,
                outputs: HashMap::new(),
            },
        );
        self.device_changed(node);
        Ok(())
    }

    fn device_changed(&mut self, node: DrmNode) {
        let events = {
            let Some(device) = self.devices.get_mut(&node) else {
                return;
            };
            match device
                .scanner
                .scan_connectors(device.output_manager.device())
            {
                Ok(scan) => scan.into_iter().collect::<Vec<_>>(),
                Err(error) => {
                    tracing::error!(?node, %error, "could not scan DRM connectors");
                    return;
                }
            }
        };
        for event in events {
            match event {
                DrmScanEvent::Connected { connector, crtc } => {
                    let Some(crtc) = crtc else {
                        tracing::warn!(?node, connector = ?connector.handle(), "connector has no usable CRTC or mode");
                        continue;
                    };
                    if connector.modes().is_empty() {
                        tracing::warn!(?node, connector = ?connector.handle(), "connector has no usable mode");
                        continue;
                    }
                    let candidates = connector
                        .modes()
                        .iter()
                        .map(|mode| {
                            let (width, height) = mode.size();
                            ModeCandidate {
                                width,
                                height,
                                refresh_millihz: mode.vrefresh(),
                                preferred: mode.mode_type().contains(ModeTypeFlags::PREFERRED),
                            }
                        })
                        .collect::<Vec<_>>();
                    let name = format!(
                        "{}-{}",
                        connector.interface().as_str(),
                        connector.interface_id()
                    );
                    let fallback_key = format!("{node:?}:{name}");
                    let edid = self.devices.get(&node).and_then(|device| {
                        edid_blob(device.output_manager.device(), connector.handle())
                    });
                    let plan = scan_outputs(&SnapshotSource(vec![ConnectorSnapshot {
                        connector: fallback_key,
                        edid,
                        modes: candidates.clone(),
                    }]))
                    .expect("snapshot source cannot fail")
                    .pop()
                    .expect("connected connector has at least one mode");
                    let mode_index = candidates
                        .iter()
                        .position(|candidate| *candidate == plan.mode)
                        .expect("planned mode came from connector snapshot");
                    let mode = connector.modes()[mode_index];
                    let (width, height) = mode.size();
                    let id = OutputId(self.next_output_id);
                    self.next_output_id += 1;
                    let output = Output::new(
                        id,
                        plan.stable_key,
                        Size::new(i64::from(width), i64::from(height)),
                    );
                    match self.state.connect_output(output) {
                        Ok(()) => {
                            let protocol_output = self
                                .state
                                .protocol_output(id)
                                .expect("new output has protocol state");
                            let mut renderer = match self.gpus.single_renderer(&node) {
                                Ok(renderer) => renderer,
                                Err(error) => {
                                    tracing::error!(?node, %error, "could not obtain DRM renderer");
                                    let _ = self.state.disconnect_output(id);
                                    continue;
                                }
                            };
                            let device = self.devices.get_mut(&node).unwrap();
                            let planes = device.output_manager.device().planes(&crtc).ok();
                            let initial = DrmOutputRenderElements::<
                                NativeRenderer<'_>,
                                WaylandSurfaceRenderElement<NativeRenderer<'_>>,
                            >::default();
                            match device.output_manager.initialize_output::<
                                _,
                                WaylandSurfaceRenderElement<NativeRenderer<'_>>,
                            >(
                                crtc,
                                mode,
                                &[connector.handle()],
                                &protocol_output,
                                planes,
                                &mut renderer,
                                &initial,
                            ) {
                                Ok(drm_output) => {
                                    device.outputs.insert(
                                        crtc,
                                        NativeSurface {
                                            output: id,
                                            drm: drm_output,
                                            pending: None,
                                            retrace: if mode.vrefresh() > 0 {
                                                Duration::from_secs_f64(
                                                    1000.0 / f64::from(mode.vrefresh()),
                                                )
                                            } else {
                                                DEFAULT_RETRACE
                                            },
                                            waiting_fence: None,
                                            exported_fence: None,
                                        },
                                    );
                                    self.scheduler.add_output(id);
                                    self.scheduler
                                        .request(id, RepaintReasons::FULL_REPAINT);
                                    self.connectors.insert((node, connector.handle()), id);
                                    tracing::info!(?node, ?id, %name, "DRM connector connected");
                                }
                                Err(error) => {
                                    tracing::error!(?node, ?id, ?error, "could not initialize KMS output");
                                    let _ = self.state.disconnect_output(id);
                                }
                            }
                        }
                        Err(error) => {
                            tracing::error!(?node, %error, %name, "could not register output");
                        }
                    }
                }
                DrmScanEvent::Disconnected { connector, crtc } => {
                    let Some(id) = self.connectors.remove(&(node, connector.handle())) else {
                        continue;
                    };
                    if let Err(error) = self.state.disconnect_output(id) {
                        tracing::error!(?node, ?id, %error, "could not unregister output");
                    }
                    self.scheduler.remove_output(id);
                    if let Some(crtc) = crtc
                        && let Some(device) = self.devices.get_mut(&node)
                    {
                        device.outputs.remove(&crtc);
                    }
                }
            }
        }
    }

    fn device_removed(&mut self, node: DrmNode) {
        let outputs: Vec<_> = self
            .connectors
            .iter()
            .filter_map(|((device, _), output)| (*device == node).then_some(*output))
            .collect();
        self.connectors.retain(|(device, _), _| *device != node);
        for output in outputs {
            if let Err(error) = self.state.disconnect_output(output) {
                tracing::error!(?node, ?output, %error, "could not remove DRM output");
            }
            self.scheduler.remove_output(output);
        }
        if let Some(device) = self.devices.remove(&node) {
            self.handle.remove(device.registration);
            self.gpus.as_mut().remove_node(&node);
        }
    }

    fn frame_submitted(&mut self, node: DrmNode, crtc: crtc::Handle) {
        let Some(surface) = self
            .devices
            .get_mut(&node)
            .and_then(|device| device.outputs.get_mut(&crtc))
        else {
            return;
        };
        let frame_id = match surface.drm.frame_submitted() {
            Ok(Some(frame_id)) => frame_id,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(?node, ?crtc, ?error, "could not retire submitted DRM frame");
                if let Some(frame) = surface.pending.take() {
                    surface.drm.reset_buffers();
                    self.scheduler.retry_at(
                        surface.output,
                        frame.frame_id,
                        Instant::now() + surface.retrace,
                    );
                }
                return;
            }
        };
        let Some(frame) = surface
            .pending
            .take_if(|frame| frame.queued && frame.frame_id == frame_id)
        else {
            tracing::warn!(
                ?node,
                ?crtc,
                frame_id,
                "retired DRM frame has no matching snapshot"
            );
            return;
        };
        if self.scheduler.presented(surface.output, frame.frame_id) {
            let frame_time = self.started.elapsed().as_millis() as u32;
            complete_frame_callbacks(&frame.callbacks, frame_time);
        }
    }

    fn fail_device_frames(&mut self, node: DrmNode) {
        let Some(device) = self.devices.get_mut(&node) else {
            return;
        };
        for surface in device.outputs.values_mut() {
            if let Some(frame) = surface.pending.take() {
                surface.drm.reset_buffers();
                self.scheduler.retry_at(
                    surface.output,
                    frame.frame_id,
                    Instant::now() + surface.retrace,
                );
            }
            if let Some(fence) = surface.waiting_fence.take() {
                surface.drm.reset_buffers();
                self.scheduler.retry_at(
                    surface.output,
                    fence.frame_id,
                    Instant::now() + surface.retrace,
                );
            }
            if let Some(fence) = surface.exported_fence.take() {
                self.handle.remove(fence.registration);
                surface.drm.reset_buffers();
                self.scheduler.retry_at(
                    surface.output,
                    fence.frame_id,
                    Instant::now() + surface.retrace,
                );
            }
        }
    }

    fn retire_software_presentations(&mut self, now: Instant) {
        let ready =
            self.devices
                .values_mut()
                .flat_map(|device| device.outputs.values_mut())
                .filter_map(|surface| {
                    let due = surface.pending.as_ref().is_some_and(|frame| {
                        !frame.queued && frame.deadline.is_some_and(|d| d <= now)
                    });
                    due.then(|| (surface.output, surface.pending.take().unwrap()))
                })
                .collect::<Vec<_>>();
        for (output, frame) in ready {
            if self.scheduler.presented(output, frame.frame_id) {
                complete_frame_callbacks(
                    &frame.callbacks,
                    self.started.elapsed().as_millis() as u32,
                );
            }
        }
    }

    fn next_software_presentation(&self) -> Option<Instant> {
        self.devices
            .values()
            .flat_map(|device| device.outputs.values())
            .filter_map(|surface| surface.pending.as_ref()?.deadline)
            .min()
    }

    fn next_fence_check(&self) -> Option<Instant> {
        self.devices
            .values()
            .flat_map(|device| device.outputs.values())
            .flat_map(|surface| {
                [
                    surface.waiting_fence.as_ref().map(|fence| fence.deadline),
                    surface.exported_fence.as_ref().map(|fence| fence.deadline),
                ]
                .into_iter()
                .flatten()
            })
            .min()
    }

    fn poll_non_exportable_fences(&mut self, now: Instant) {
        let mut ready = Vec::new();
        let mut failed = Vec::new();
        for (node, device) in &mut self.devices {
            for (crtc, surface) in &mut device.outputs {
                if let Some(fence) = surface.waiting_fence.as_mut()
                    && fence.deadline <= now
                {
                    if fence.sync.is_reached() {
                        let fence = surface.waiting_fence.take().unwrap();
                        ready.push((*node, *crtc, fence.frame_id, fence.callbacks));
                    } else if fence.expires_at <= now {
                        let fence = surface.waiting_fence.take().unwrap();
                        surface.drm.reset_buffers();
                        failed.push((surface.output, fence.frame_id, surface.retrace, None));
                    } else {
                        fence.delay = (fence.delay * 2).min(surface.retrace);
                        fence.deadline = now + fence.delay;
                    }
                }
                if surface
                    .exported_fence
                    .as_ref()
                    .is_some_and(|fence| fence.deadline <= now)
                {
                    let fence = surface.exported_fence.take().unwrap();
                    surface.drm.reset_buffers();
                    failed.push((
                        surface.output,
                        fence.frame_id,
                        surface.retrace,
                        Some(fence.registration),
                    ));
                }
            }
        }
        for (output, frame_id, retrace, registration) in failed {
            if let Some(registration) = registration {
                self.handle.remove(registration);
            }
            tracing::warn!(?output, frame_id, "GPU fence timed out; retrying frame");
            self.scheduler.retry_at(output, frame_id, now + retrace);
        }
        for (node, crtc, frame_id, callbacks) in ready {
            self.queue_prepared_frame(node, crtc, frame_id, callbacks);
        }
    }

    fn render_all(&mut self) {
        let requests = self
            .scheduler
            .begin_ready(Instant::now(), MAX_NATIVE_FRAMES_PER_BATCH);
        for request in requests {
            let route = self.devices.iter().find_map(|(node, device)| {
                device.outputs.iter().find_map(|(crtc, surface)| {
                    (surface.output == request.output).then_some((*node, *crtc))
                })
            });
            let Some((node, crtc)) = route else {
                self.scheduler.remove_output(request.output);
                continue;
            };
            self.render_output(node, crtc, request);
        }
    }

    fn queue_prepared_frame(
        &mut self,
        node: DrmNode,
        crtc: crtc::Handle,
        frame_id: u64,
        callbacks: Vec<FrameCallback>,
    ) {
        let Some(surface) = self
            .devices
            .get_mut(&node)
            .and_then(|device| device.outputs.get_mut(&crtc))
        else {
            return;
        };
        let output = surface.output;
        if surface
            .exported_fence
            .as_ref()
            .is_some_and(|fence| fence.frame_id == frame_id)
        {
            surface.exported_fence = None;
        }
        if !self.scheduler.submitted(output, frame_id) {
            tracing::debug!(?output, frame_id, "discarding stale prepared DRM frame");
            return;
        }
        if let Err(error) = surface.drm.queue_frame(frame_id) {
            tracing::warn!(?node, ?crtc, ?error, "could not queue prepared DRM frame");
            self.scheduler
                .retry_at(output, frame_id, Instant::now() + surface.retrace);
            return;
        }
        surface.pending = Some(PendingNativeFrame {
            frame_id,
            callbacks,
            queued: true,
            deadline: None,
        });
    }

    fn render_output(&mut self, node: DrmNode, crtc: crtc::Handle, request: RenderRequest) {
        let output = request.output;
        let roots = self.state.render_roots_for_output(output);
        let mut renderer = match self.gpus.single_renderer(&node) {
            Ok(renderer) => renderer,
            Err(error) => {
                tracing::error!(?node, %error, "could not acquire output renderer");
                self.scheduler
                    .retry_at(output, request.frame_id, Instant::now() + DEFAULT_RETRACE);
                return;
            }
        };
        self.state.validate_dmabuf_imports(&mut renderer);
        let mut callbacks = Vec::new();
        let elements: Vec<WaylandSurfaceRenderElement<NativeRenderer<'_>>> = roots
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
                    elements.extend(surface_tree_snapshot(
                        &mut renderer,
                        popup.wl_surface(),
                        *location + offset,
                        *scale,
                        1.0,
                        Kind::Unspecified,
                        &mut callbacks,
                    ));
                }
                elements.extend(surface_tree_snapshot(
                    &mut renderer,
                    surface,
                    *location,
                    *scale,
                    1.0,
                    Kind::Unspecified,
                    &mut callbacks,
                ));
                elements
            })
            .collect();
        let Some(surface) = self
            .devices
            .get_mut(&node)
            .and_then(|device| device.outputs.get_mut(&crtc))
        else {
            self.scheduler.remove_output(output);
            return;
        };
        match surface.drm.render_frame(
            &mut renderer,
            &elements,
            Color32F::new(0.025, 0.035, 0.06, 1.0),
            FrameFlags::empty(),
        ) {
            Ok(frame) if frame.is_empty => {
                if self.scheduler.submitted(output, request.frame_id) {
                    surface.pending = Some(PendingNativeFrame {
                        frame_id: request.frame_id,
                        callbacks,
                        queued: false,
                        deadline: Some(Instant::now() + surface.retrace),
                    });
                }
            }
            Ok(frame) => {
                let sync = frame
                    .needs_sync()
                    .then(|| match frame.primary_element {
                        PrimaryPlaneElement::Swapchain(primary) => Some(primary.sync),
                        PrimaryPlaneElement::Element(_) => None,
                    })
                    .flatten();
                if let Some(fence_fd) = sync.as_ref().and_then(|sync| sync.export()) {
                    let mut callbacks = Some(callbacks);
                    let registration = self.handle.insert_source(
                        OneShotReadableFdSource::new(fence_fd),
                        move |(), _, runtime| {
                            runtime.enqueue(NativeEvent::FenceReady {
                                node,
                                crtc,
                                frame_id: request.frame_id,
                                callbacks: callbacks
                                    .take()
                                    .expect("one-shot GPU fence fired more than once"),
                            });
                        },
                    );
                    match registration {
                        Ok(registration) => {
                            surface.exported_fence = Some(PendingExportedFence {
                                frame_id: request.frame_id,
                                deadline: Instant::now() + FENCE_TIMEOUT,
                                registration,
                            });
                        }
                        Err(error) => {
                            tracing::warn!(%error, "could not register GPU fence; retrying frame");
                            surface.drm.reset_buffers();
                            self.scheduler.retry_at(
                                output,
                                request.frame_id,
                                Instant::now() + surface.retrace,
                            );
                        }
                    }
                } else {
                    if let Some(sync) = sync.filter(|sync| !sync.is_reached()) {
                        // A non-exportable fence cannot wake calloop. Check it
                        // with an exponentially backed-off deadline instead of
                        // blocking the compositor thread.
                        surface.waiting_fence = Some(PendingGpuFence {
                            frame_id: request.frame_id,
                            callbacks,
                            sync,
                            deadline: Instant::now() + FENCE_RECHECK_INITIAL,
                            delay: FENCE_RECHECK_INITIAL,
                            expires_at: Instant::now() + FENCE_TIMEOUT,
                        });
                    } else if !self.scheduler.submitted(output, request.frame_id) {
                        tracing::debug!(
                            ?output,
                            frame_id = request.frame_id,
                            "discarding stale rendered DRM frame"
                        );
                    } else if let Err(error) = surface.drm.queue_frame(request.frame_id) {
                        tracing::warn!(?node, ?crtc, ?error, "could not queue DRM frame");
                        self.scheduler.retry_at(
                            output,
                            request.frame_id,
                            Instant::now() + surface.retrace,
                        );
                    } else {
                        surface.pending = Some(PendingNativeFrame {
                            frame_id: request.frame_id,
                            callbacks,
                            queued: true,
                            deadline: None,
                        });
                    }
                }
            }
            Err(error) => {
                tracing::warn!(?node, ?crtc, ?error, "could not render DRM frame");
                self.scheduler
                    .retry_at(output, request.frame_id, Instant::now() + surface.retrace);
            }
        }
    }
}

fn is_lossy_native_motion(event: &NativeEvent) -> bool {
    matches!(
        event,
        NativeEvent::Input(
            BackendInputEvent::PointerMotion { .. }
                | BackendInputEvent::PointerMotionAbsolute { .. }
        )
    )
}

pub fn run(config: Config, config_path: std::path::PathBuf) -> Result<()> {
    let mut event_loop: EventLoop<NativeLoop> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    let display = Display::<Astera>::new()?;
    let mut state = Astera::new(&display.handle(), config);
    state.set_output_configuration_supported(false);
    state.watch_config(config_path);
    state.disconnect_output(astera_core::OutputId(0))?;
    let listener = WaylandSocketSource::bind_auto("astera", 1..32)?;
    let socket_name = listener
        .socket_name()
        .context("Wayland listening socket has no name")?
        .to_string_lossy()
        .into_owned();
    let ipc = IpcServer::bind(&socket_name)?;
    event_loop
        .handle()
        .insert_source(ipc.event_source(), |(), _, runtime| {
            runtime.enqueue(NativeEvent::IpcReady);
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(listener, |stream, _, runtime| {
            runtime.enqueue(NativeEvent::WaylandClient(stream));
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(ReadableFdSource::duplicate(&display)?, |(), _, runtime| {
            runtime.enqueue(NativeEvent::WaylandReady);
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    if let Some(config_fd) = state.config_watch_fd()? {
        event_loop
            .handle()
            .insert_source(ReadableFdSource::new(config_fd), |(), _, runtime| {
                runtime.enqueue(NativeEvent::ConfigChanged);
            })
            .map_err(|error| anyhow!(error.to_string()))?;
    }

    let (session, session_notifier) = LibSeatSession::new()?;
    let seat_name = session.seat();
    let udev = UdevBackend::new(&seat_name)?;
    let initial_devices = udev
        .device_list()
        .map(|(device, path)| (device, path.to_path_buf()))
        .collect::<Vec<_>>();
    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput.udev_assign_seat(&seat_name).map_err(|()| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("libinput could not assign seat {seat_name:?}"),
        )
    })?;
    let input = LibinputInputBackend::new(libinput.clone());

    event_loop
        .handle()
        .insert_source(input, |event, _, runtime| {
            runtime.enqueue(NativeEvent::Input(event));
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(session_notifier, |event, _, runtime| {
            runtime.enqueue(NativeEvent::Session(event));
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    event_loop
        .handle()
        .insert_source(udev, |event, _, runtime| {
            runtime.enqueue(NativeEvent::Udev(event));
        })
        .map_err(|error| anyhow!(error.to_string()))?;

    let mut runtime = NativeLoop {
        handle,
        display,
        state,
        ipc,
        events: VecDeque::new(),
        libinput,
        _session: session,
        devices: HashMap::new(),
        connectors: HashMap::new(),
        next_output_id: 1,
        gpus: GpuManager::new(GbmGlesBackend::with_context_priority(ContextPriority::High))?,
        started: Instant::now(),
        scheduler: RepaintScheduler::default(),
        wayland_ready_queued: false,
        ipc_ready_queued: false,
        config_changed_queued: false,
        shutdown_deadline: None,
    };
    for (device_id, path) in initial_devices {
        match DrmNode::from_dev_id(device_id) {
            Ok(node) => {
                if let Err(error) = runtime.device_added(node, &path) {
                    tracing::error!(?device_id, ?path, %error, "could not initialize DRM device");
                }
            }
            Err(error) => tracing::error!(?device_id, %error, "invalid initial DRM device node"),
        }
    }
    tracing::info!(wayland_display = %socket_name, seat = %seat_name, "native session is ready");
    println!("WAYLAND_DISPLAY={socket_name}");

    while !runtime.shutdown_complete() {
        let now = Instant::now();
        let deadline = [
            runtime.ipc.next_timeout(),
            runtime.state.next_timer_deadline(),
            runtime.scheduler.earliest_deadline(),
            runtime.next_software_presentation(),
            runtime.next_fence_check(),
            runtime.shutdown_deadline,
        ]
        .into_iter()
        .flatten()
        .min();
        let timeout = if runtime.events.is_empty() && !runtime.scheduler.has_ready_at(now) {
            deadline.map(|deadline| deadline.saturating_duration_since(now))
        } else {
            Some(Duration::ZERO)
        };
        event_loop.dispatch(timeout, &mut runtime)?;
        runtime.process_events()?;
        let now = Instant::now();
        runtime.ipc.expire(now);
        let timer_due = runtime
            .state
            .next_timer_deadline()
            .is_some_and(|deadline| deadline <= now);
        runtime.state.poll_config();
        if runtime.state.should_quit() {
            runtime.begin_shutdown();
        }
        runtime.state.process_key_repeats();
        runtime.state.remove_dead_windows();
        runtime.poll_non_exportable_fences(now);
        runtime.retire_software_presentations(now);
        if timer_due {
            runtime.request_all(RepaintReasons::DAMAGE);
        }
        runtime.ipc.finish_tick(&mut runtime.state);
        if runtime.shutdown_deadline.is_none() {
            runtime.render_all();
        }
        runtime.display.flush_clients()?;
    }
    tracing::info!("native compositor is shutting down");
    Ok(())
}
