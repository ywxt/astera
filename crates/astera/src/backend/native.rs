use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
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
        input::{Device as _, Event as _, InputEvent as BackendInputEvent},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            Color32F, ImportAll, ImportDma, ImportMem,
            element::{
                Kind, RenderElementStates, memory::MemoryRenderBufferRenderElement,
                render_elements, surface::WaylandSurfaceRenderElement,
            },
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
        render::{
            FrameCallback, PresentationCapture, PresentedFifoBarrier, complete_frame_callbacks,
            surface_tree_snapshot,
        },
        runtime::{RenderRequest, RepaintReasons, RepaintScheduler},
        sources::{OneShotReadableFdSource, ReadableFdSource, WaylandSocketSource},
    },
    ipc_server::IpcServer,
    state::{Astera, ClientState, InputServiceSupervisor, cursor::CursorRenderSource},
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
    session_active: bool,
    deferred_devices: HashMap<DrmNode, PathBuf>,
    shutdown_deadline: Option<Instant>,
    input_service: Option<InputServiceSupervisor>,
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
        fifo_barriers: Vec<PresentedFifoBarrier>,
        scanout: PreparedScanout,
    },
    WaylandClient(std::os::unix::net::UnixStream),
    WaylandReady,
    IpcReady,
    ConfigChanged,
    InputServiceExited(crate::state::InputServiceExit),
}

type GraphicsBackend = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
type NativeRenderer<'a> = MultiRenderer<'a, 'a, GraphicsBackend, GraphicsBackend>;

render_elements! {
    NativeRenderElement<R> where R: ImportMem + ImportAll;
    Surface=WaylandSurfaceRenderElement<R>,
    Memory=MemoryRenderBufferRenderElement<R>,
}
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
    active: bool,
}

struct NativeSurface {
    output: OutputId,
    drm: NativeOutput,
    pending: Option<PendingNativeFrame>,
    retrace: Duration,
    waiting_fence: Option<PendingGpuFence>,
    exported_fence: Option<PendingExportedFence>,
    requested_power_mode: Option<bool>,
    committing_power_mode: Option<bool>,
}

struct PendingNativeFrame {
    frame_id: u64,
    callbacks: Vec<FrameCallback>,
    fifo_barriers: Vec<PresentedFifoBarrier>,
    queued: bool,
    deadline: Option<Instant>,
    lock_generation: Option<u64>,
    power_mode: Option<bool>,
}

struct PendingGpuFence {
    frame_id: u64,
    callbacks: Vec<FrameCallback>,
    fifo_barriers: Vec<PresentedFifoBarrier>,
    sync: SyncPoint,
    deadline: Instant,
    delay: Duration,
    expires_at: Instant,
    scanout: PreparedScanout,
}

struct PreparedScanout {
    roots: Vec<(
        smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        Point<i32, Physical>,
        f64,
    )>,
    states: RenderElementStates,
    lock_generation: Option<u64>,
    power_mode: Option<bool>,
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
        // dmabuf creation must complete while outputs are powered off as well. It only needs a
        // renderer import check, not a KMS frame or presentation opportunity.
        if self.state.has_pending_dmabuf_imports() {
            if let Some(node) = self.devices.keys().next().copied() {
                match self.gpus.single_renderer(&node) {
                    Ok(mut renderer) => self.state.validate_dmabuf_imports(&mut renderer),
                    Err(error) => {
                        tracing::warn!(?node, %error, "could not validate pending dmabuf imports");
                        self.state.fail_pending_dmabuf_imports();
                    }
                }
            } else {
                tracing::warn!("rejecting pending dmabuf imports because no GPU is available");
                self.state.fail_pending_dmabuf_imports();
            }
        }
        self.apply_output_power_requests();
        Ok(())
    }

    fn apply_output_power_requests(&mut self) {
        for (output, powered) in self.state.take_output_power_requests() {
            let Some(surface) = self
                .devices
                .values_mut()
                .flat_map(|device| device.outputs.values_mut())
                .find(|surface| surface.output == output)
            else {
                self.state.fail_output_power(output);
                continue;
            };
            surface.requested_power_mode = Some(powered);
        }
        for device in self.devices.values_mut().filter(|device| device.active) {
            for surface in device.outputs.values_mut() {
                let idle = surface.pending.is_none()
                    && surface.waiting_fence.is_none()
                    && surface.exported_fence.is_none()
                    && surface.committing_power_mode.is_none();
                let Some(powered) = idle.then(|| surface.requested_power_mode.take()).flatten()
                else {
                    continue;
                };
                if powered {
                    surface.committing_power_mode = Some(true);
                    self.scheduler.add_output(surface.output);
                    self.scheduler
                        .request(surface.output, RepaintReasons::FULL_REPAINT);
                } else {
                    match surface.drm.with_compositor(|compositor| compositor.clear()) {
                        Ok(()) => {
                            self.state.confirm_output_power(surface.output, false);
                            self.scheduler.remove_output(surface.output);
                        }
                        Err(error) => {
                            tracing::warn!(output = ?surface.output, ?error, "KMS rejected output power-off request");
                            self.state.fail_output_power(surface.output);
                        }
                    }
                }
            }
        }
    }

    fn process_event(&mut self, event: NativeEvent) -> Result<bool> {
        let mut scene_changed = false;
        match event {
            NativeEvent::Input(event) => {
                let touch_device = match &event {
                    BackendInputEvent::TouchDown { event } => Some(event.device()),
                    BackendInputEvent::TouchMotion { event } => Some(event.device()),
                    BackendInputEvent::TouchUp { event } => Some(event.device()),
                    BackendInputEvent::TouchCancel { event } => Some(event.device()),
                    BackendInputEvent::TouchFrame { event } => Some(event.device()),
                    BackendInputEvent::TabletToolAxis { event } => Some(event.device()),
                    BackendInputEvent::TabletToolProximity { event } => Some(event.device()),
                    BackendInputEvent::TabletToolTip { event } => Some(event.device()),
                    BackendInputEvent::TabletToolButton { event } => Some(event.device()),
                    _ => None,
                };
                if let Some(device) = touch_device
                    && let Some(output) = device.output_name()
                {
                    self.state.bind_touch_device_output(device.id(), output);
                }
                self.state.process_input(event);
                scene_changed = true;
            }
            NativeEvent::Session(SessionEvent::PauseSession) => {
                self.session_active = false;
                self.libinput.suspend();
                self.scheduler.pause();
                for device in self.devices.values_mut() {
                    device.active = false;
                    device.output_manager.pause();
                    for surface in device.outputs.values_mut() {
                        if let Some(powered) = surface.committing_power_mode.take() {
                            surface.requested_power_mode = Some(powered);
                        }
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
                self.session_active = true;
                if let Err(error) = self.libinput.resume() {
                    tracing::error!(?error, "could not resume libinput");
                }
                self.scheduler.resume();
                for device in self.devices.values_mut() {
                    match device.output_manager.activate(false) {
                        Ok(()) => {
                            device.active = true;
                            for surface in device.outputs.values() {
                                if self.state.output_is_powered(surface.output) {
                                    self.scheduler.add_output(surface.output);
                                    self.scheduler
                                        .request(surface.output, RepaintReasons::FULL_REPAINT);
                                }
                            }
                        }
                        Err(error) => {
                            device.active = false;
                            for surface in device.outputs.values() {
                                self.scheduler.remove_output(surface.output);
                            }
                            tracing::error!(?error, "could not reactivate DRM device");
                        }
                    }
                }
                for (node, path) in std::mem::take(&mut self.deferred_devices) {
                    if let Err(error) = self.device_added(node, &path) {
                        tracing::error!(?node, ?path, %error, "could not add deferred DRM device");
                    }
                }
                tracing::info!("native session activated");
            }
            NativeEvent::Udev(UdevEvent::Added { device_id, path }) => {
                match DrmNode::from_dev_id(device_id) {
                    Ok(node) => {
                        if !self.session_active {
                            self.deferred_devices.insert(node, path);
                        } else if let Err(error) = self.device_added(node, &path) {
                            tracing::error!(?device_id, ?path, %error, "could not add DRM device");
                        }
                    }
                    Err(error) => tracing::error!(?device_id, %error, "invalid DRM device node"),
                }
            }
            NativeEvent::Udev(UdevEvent::Changed { device_id }) => {
                if let Ok(node) = DrmNode::from_dev_id(device_id)
                    && self.session_active
                {
                    self.device_changed(node);
                }
            }
            NativeEvent::Udev(UdevEvent::Removed { device_id }) => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    self.deferred_devices.remove(&node);
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
                fifo_barriers,
                scanout,
            } => self.queue_prepared_frame(node, crtc, frame_id, callbacks, fifo_barriers, scanout),
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
            NativeEvent::InputServiceExited(exit) => {
                if let Some(service) = &mut self.input_service {
                    service.exited(exit);
                }
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
        self.state
            .enable_dmabuf(Some(node.dev_id()), render_formats.clone());
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
                active: true,
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
                            self.state.register_output_alias(name.clone(), id);
                            self.state.register_output_alias(plan.connector.clone(), id);
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
                            self.state.register_output_dmabuf_feedback(
                                id,
                                node.dev_id(),
                                renderer.dmabuf_formats(),
                            );
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
                                            requested_power_mode: None,
                                            committing_power_mode: None,
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
        // Rebase feedback after Desktop has selected replacement outputs so unmapped surfaces
        // using the active-output fallback receive the new main device as well.
        self.state.unregister_dmabuf_device(node.dev_id());
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
            if let Some(powered) = frame.power_mode {
                surface.committing_power_mode = None;
                self.state.confirm_output_power(surface.output, powered);
                if !powered {
                    self.scheduler.remove_output(surface.output);
                }
            }
            self.state
                .lock_frame_presented(surface.output, frame.lock_generation);
            let frame_time = self.started.elapsed().as_millis() as u32;
            complete_frame_callbacks(&frame.callbacks, frame_time);
            self.state.signal_fifo_barriers(&frame.fifo_barriers);
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
                let power_mode = frame.power_mode;
                if power_mode.is_some()
                    && let Some(surface) = self
                        .devices
                        .values_mut()
                        .flat_map(|device| device.outputs.values_mut())
                        .find(|surface| surface.output == output)
                {
                    surface.committing_power_mode = None;
                }
                if let Some(powered) = power_mode {
                    self.state.confirm_output_power(output, powered);
                    if !powered {
                        self.scheduler.remove_output(output);
                    }
                }
                self.state
                    .lock_frame_presented(output, frame.lock_generation);
                complete_frame_callbacks(
                    &frame.callbacks,
                    self.started.elapsed().as_millis() as u32,
                );
                self.state.signal_fifo_barriers(&frame.fifo_barriers);
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
                        ready.push((
                            *node,
                            *crtc,
                            fence.frame_id,
                            fence.callbacks,
                            fence.fifo_barriers,
                            fence.scanout,
                        ));
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
        for (node, crtc, frame_id, callbacks, fifo_barriers, scanout) in ready {
            self.queue_prepared_frame(node, crtc, frame_id, callbacks, fifo_barriers, scanout);
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
        fifo_barriers: Vec<PresentedFifoBarrier>,
        scanout: PreparedScanout,
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
        self.state
            .update_primary_scanout_output(output, &scanout.roots, &scanout.states);
        surface.pending = Some(PendingNativeFrame {
            frame_id,
            callbacks,
            fifo_barriers,
            queued: true,
            deadline: None,
            lock_generation: scanout.lock_generation,
            power_mode: scanout.power_mode,
        });
    }

    fn render_output(&mut self, node: DrmNode, crtc: crtc::Handle, request: RenderRequest) {
        let output = request.output;
        let lock_generation = self.state.locking_generation();
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
        let mut presentation = PresentationCapture {
            fifo_barriers: self.state.fifo_barriers_for_output(output),
            ..PresentationCapture::default()
        };
        let mut elements: Vec<NativeRenderElement<NativeRenderer<'_>>> = roots
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
                    elements.extend(
                        surface_tree_snapshot(
                            &mut renderer,
                            popup.wl_surface(),
                            *location + offset,
                            *scale,
                            1.0,
                            Kind::Unspecified,
                            &mut presentation,
                        )
                        .into_iter()
                        .map(Into::into),
                    );
                }
                elements.extend(
                    surface_tree_snapshot(
                        &mut renderer,
                        surface,
                        *location,
                        *scale,
                        1.0,
                        Kind::Unspecified,
                        &mut presentation,
                    )
                    .into_iter()
                    .map(Into::into),
                );
                elements
            })
            .collect();
        if let Some((icon, location, scale)) = self.state.dnd_icon_render_source(output) {
            let icon_elements = surface_tree_snapshot(
                &mut renderer,
                &icon,
                location,
                scale,
                1.0,
                Kind::Cursor,
                &mut presentation,
            );
            elements.splice(0..0, icon_elements.into_iter().map(Into::into));
        }
        if let Some(cursor) = self.state.cursor_render_source(output) {
            match cursor {
                CursorRenderSource::Surface {
                    surface,
                    location,
                    scale,
                } => {
                    let cursor_elements = surface_tree_snapshot(
                        &mut renderer,
                        &surface,
                        location,
                        scale,
                        1.0,
                        Kind::Cursor,
                        &mut presentation,
                    );
                    elements.splice(0..0, cursor_elements.into_iter().map(Into::into));
                }
                CursorRenderSource::Memory {
                    buffer,
                    location,
                    size,
                    source_size,
                } => {
                    match MemoryRenderBufferRenderElement::from_buffer(
                        &mut renderer,
                        location,
                        &buffer,
                        None,
                        Some(smithay::utils::Rectangle::from_size(source_size.to_f64())),
                        Some(size),
                        Kind::Cursor,
                    ) {
                        Ok(element) => elements.insert(0, element.into()),
                        Err(error) => tracing::warn!(?error, "could not import cursor image"),
                    }
                }
            }
        }
        let PresentationCapture {
            callbacks,
            fifo_barriers,
        } = presentation;
        let Some(surface) = self
            .devices
            .get_mut(&node)
            .and_then(|device| device.outputs.get_mut(&crtc))
        else {
            self.scheduler.remove_output(output);
            return;
        };
        let power_mode = surface.committing_power_mode;
        let clear = if self.state.session_is_locked() {
            Color32F::new(0.0, 0.0, 0.0, 1.0)
        } else {
            Color32F::new(0.025, 0.035, 0.06, 1.0)
        };
        match surface
            .drm
            .render_frame(&mut renderer, &elements, clear, FrameFlags::empty())
        {
            Ok(frame) if frame.is_empty => {
                if self.scheduler.submitted(output, request.frame_id) {
                    self.state
                        .update_primary_scanout_output(output, &roots, &frame.states);
                    surface.pending = Some(PendingNativeFrame {
                        frame_id: request.frame_id,
                        callbacks,
                        fifo_barriers,
                        queued: false,
                        deadline: Some(Instant::now() + surface.retrace),
                        lock_generation,
                        power_mode,
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
                let scanout = PreparedScanout {
                    roots,
                    states: frame.states,
                    lock_generation,
                    power_mode,
                };
                if let Some(fence_fd) = sync.as_ref().and_then(|sync| sync.export()) {
                    let mut callbacks = Some(callbacks);
                    let mut fifo_barriers = Some(fifo_barriers);
                    let mut scanout = Some(scanout);
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
                                fifo_barriers: fifo_barriers
                                    .take()
                                    .expect("one-shot GPU fence fired more than once"),
                                scanout: scanout
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
                            fifo_barriers,
                            sync,
                            deadline: Instant::now() + FENCE_RECHECK_INITIAL,
                            delay: FENCE_RECHECK_INITIAL,
                            expires_at: Instant::now() + FENCE_TIMEOUT,
                            scanout,
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
                        self.state.update_primary_scanout_output(
                            output,
                            &scanout.roots,
                            &scanout.states,
                        );
                        surface.pending = Some(PendingNativeFrame {
                            frame_id: request.frame_id,
                            callbacks,
                            fifo_barriers,
                            queued: true,
                            deadline: None,
                            lock_generation,
                            power_mode,
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
    let input_service = config.input_service.clone();
    let mut state = Astera::new(&display.handle(), config);
    state.enable_output_power_management();
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
    let (mut input_service, input_service_exits) = match input_service {
        Some(argv) => {
            let (service, exits) = InputServiceSupervisor::new(argv);
            (Some(service), Some(exits))
        }
        None => (None, None),
    };
    if let Some(service) = &mut input_service {
        service.start(display.handle())?;
    }
    event_loop
        .handle()
        .insert_source(ipc.event_source(), |(), _, runtime| {
            runtime.enqueue(NativeEvent::IpcReady);
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    if let Some(exits) = input_service_exits {
        event_loop
            .handle()
            .insert_source(exits, |event, _, runtime| {
                if let smithay::reexports::calloop::channel::Event::Msg(exit) = event {
                    runtime.enqueue(NativeEvent::InputServiceExited(exit));
                }
            })
            .map_err(|error| anyhow!(error.to_string()))?;
    }
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
        session_active: true,
        deferred_devices: HashMap::new(),
        shutdown_deadline: None,
        input_service,
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
            (!runtime.state.session_is_locked())
                .then(|| {
                    runtime
                        .input_service
                        .as_ref()
                        .and_then(InputServiceSupervisor::next_deadline)
                })
                .flatten(),
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
        let locked = runtime.state.session_is_locked();
        if let Some(service) = &mut runtime.input_service {
            service.poll(runtime.display.handle(), locked);
        }
        let now = Instant::now();
        runtime.ipc.expire(now);
        let timer_due = runtime
            .state
            .next_visual_timer_deadline()
            .is_some_and(|deadline| deadline <= now);
        runtime.state.poll_config();
        if runtime.state.should_quit() {
            runtime.begin_shutdown();
        }
        runtime.state.process_key_repeats();
        runtime.state.process_commit_timers();
        runtime.state.process_idle_timers();
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
