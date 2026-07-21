use std::{
    collections::HashMap,
    error::Error,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

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
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            Color32F, ImportDma,
            element::{
                Kind,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
            gles::GlesRenderer,
            multigpu::{GpuManager, MultiRenderer, gbm::GbmGlesBackend},
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
        wayland_server::{Display, ListeningSocket},
    },
    utils::{DeviceFd, Physical, Point},
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use crate::{
    ipc_server::IpcServer,
    state::{Astera, ClientState, send_frames_surface_tree},
};

struct NativeLoop {
    handle: LoopHandle<'static, NativeLoop>,
    display: Display<Astera>,
    state: Astera,
    ipc: IpcServer,
    listener: ListeningSocket,
    libinput: Libinput,
    _session: LibSeatSession,
    devices: HashMap<DrmNode, NativeDevice>,
    connectors: HashMap<(DrmNode, connector::Handle), OutputId>,
    next_output_id: u32,
    gpus: GpuManager<GraphicsBackend>,
    started: Instant,
}

type GraphicsBackend = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
type NativeRenderer<'a> = MultiRenderer<'a, 'a, GraphicsBackend, GraphicsBackend>;
type NativeOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;
type NativeOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    (),
    DrmDeviceFd,
>;

fn edid_output_key(device: &impl ControlDevice, connector: connector::Handle) -> Option<String> {
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
        let manufacturer = u16::from_be_bytes([edid[8], edid[9]]);
        let product = u16::from_le_bytes([edid[10], edid[11]]);
        let serial = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);
        let fingerprint = edid.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
        return Some(format!(
            "edid:{manufacturer:04x}:{product:04x}:{serial:08x}:{fingerprint:016x}"
        ));
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
    pending: bool,
}

impl NativeLoop {
    fn device_added(&mut self, node: DrmNode, path: &Path) -> Result<(), Box<dyn Error>> {
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
        let output_manager = DrmOutputManager::new(
            drm,
            allocator,
            exporter,
            Some(gbm),
            [Fourcc::Argb8888, Fourcc::Abgr8888],
            render_formats,
        );
        let registration =
            self.handle
                .insert_source(notifier, move |event, _, runtime| match event {
                    DrmEvent::VBlank(crtc) => runtime.frame_submitted(node, crtc),
                    DrmEvent::Error(error) => tracing::error!(?node, ?error, "DRM event error"),
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
                    let mode = connector
                        .modes()
                        .iter()
                        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
                        .copied()
                        .unwrap_or(connector.modes()[0]);
                    let (width, height) = mode.size();
                    let name = format!(
                        "{}-{}",
                        connector.interface().as_str(),
                        connector.interface_id()
                    );
                    let id = OutputId(self.next_output_id);
                    self.next_output_id += 1;
                    let stable_key = self
                        .devices
                        .get(&node)
                        .and_then(|device| {
                            edid_output_key(device.output_manager.device(), connector.handle())
                        })
                        .unwrap_or_else(|| format!("{node:?}:{name}"));
                    let output = Output::new(
                        id,
                        stable_key,
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
                                            pending: false,
                                        },
                                    );
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
                    if let Some(crtc) = crtc {
                        if let Some(device) = self.devices.get_mut(&node) {
                            device.outputs.remove(&crtc);
                        }
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
        if let Err(error) = surface.drm.frame_submitted() {
            tracing::warn!(?node, ?crtc, ?error, "could not retire submitted DRM frame");
        }
        surface.pending = false;
    }

    fn render_all(&mut self) {
        let outputs = self
            .devices
            .iter()
            .flat_map(|(node, device)| {
                device
                    .outputs
                    .iter()
                    .filter(|(_, surface)| !surface.pending)
                    .map(|(crtc, surface)| (*node, *crtc, surface.output))
            })
            .collect::<Vec<_>>();
        for (node, crtc, output) in outputs {
            self.render_output(node, crtc, output);
        }
    }

    fn render_output(&mut self, node: DrmNode, crtc: crtc::Handle, output: OutputId) {
        let roots = self.state.render_roots_for_output(output);
        let mut renderer = match self.gpus.single_renderer(&node) {
            Ok(renderer) => renderer,
            Err(error) => {
                tracing::error!(?node, %error, "could not acquire output renderer");
                return;
            }
        };
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
                    elements.extend(render_elements_from_surface_tree(
                        &mut renderer,
                        popup.wl_surface(),
                        *location + offset,
                        *scale,
                        1.0,
                        Kind::Unspecified,
                    ));
                }
                elements.extend(render_elements_from_surface_tree(
                    &mut renderer,
                    surface,
                    *location,
                    *scale,
                    1.0,
                    Kind::Unspecified,
                ));
                elements
            })
            .collect();
        let Some(surface) = self
            .devices
            .get_mut(&node)
            .and_then(|device| device.outputs.get_mut(&crtc))
        else {
            return;
        };
        match surface.drm.render_frame(
            &mut renderer,
            &elements,
            Color32F::new(0.025, 0.035, 0.06, 1.0),
            FrameFlags::empty(),
        ) {
            Ok(frame) if frame.is_empty => {}
            Ok(frame) => {
                if frame.needs_sync() {
                    if let PrimaryPlaneElement::Swapchain(primary) = frame.primary_element {
                        let _ = primary.sync.wait();
                    }
                }
                if let Err(error) = surface.drm.queue_frame(()) {
                    tracing::warn!(?node, ?crtc, ?error, "could not queue DRM frame");
                    return;
                }
                surface.pending = true;
                let frame_time = self.started.elapsed().as_millis() as u32;
                for (root, _, _) in roots {
                    send_frames_surface_tree(&root, frame_time);
                    for (popup, _) in PopupManager::popups_for_surface(&root) {
                        send_frames_surface_tree(popup.wl_surface(), frame_time);
                    }
                }
            }
            Err(error) => tracing::warn!(?node, ?crtc, ?error, "could not render DRM frame"),
        }
    }
}

pub fn run(config: Config) -> Result<(), Box<dyn Error>> {
    let mut event_loop: EventLoop<NativeLoop> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    let display = Display::<Astera>::new()?;
    let mut state = Astera::new(&display.handle(), config);
    state.disconnect_output(astera_core::OutputId(0))?;
    let listener = ListeningSocket::bind_auto("astera", 1..32)?;
    let socket_name = listener
        .socket_name()
        .ok_or("Wayland listening socket has no name")?
        .to_string_lossy()
        .into_owned();
    let ipc = IpcServer::bind(&socket_name)?;

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
            runtime.state.process_input(event);
        })?;
    event_loop
        .handle()
        .insert_source(session_notifier, |event, _, runtime| match event {
            SessionEvent::PauseSession => {
                runtime.libinput.suspend();
                for device in runtime.devices.values_mut() {
                    device.output_manager.pause();
                }
                tracing::info!("native session paused");
            }
            SessionEvent::ActivateSession => {
                if let Err(error) = runtime.libinput.resume() {
                    tracing::error!(?error, "could not resume libinput");
                }
                for device in runtime.devices.values_mut() {
                    if let Err(error) = device.output_manager.activate(false) {
                        tracing::error!(?error, "could not reactivate DRM device");
                    }
                }
                tracing::info!("native session activated");
            }
        })?;
    event_loop
        .handle()
        .insert_source(udev, |event, _, runtime| match event {
            UdevEvent::Added { device_id, path } => match DrmNode::from_dev_id(device_id) {
                Ok(node) => {
                    if let Err(error) = runtime.device_added(node, &path) {
                        tracing::error!(?device_id, ?path, %error, "could not add DRM device");
                    }
                }
                Err(error) => tracing::error!(?device_id, %error, "invalid DRM device node"),
            },
            UdevEvent::Changed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    runtime.device_changed(node);
                }
            }
            UdevEvent::Removed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    runtime.device_removed(node);
                }
            }
        })?;

    let mut runtime = NativeLoop {
        handle,
        display,
        state,
        ipc,
        listener,
        libinput,
        _session: session,
        devices: HashMap::new(),
        connectors: HashMap::new(),
        next_output_id: 1,
        gpus: GpuManager::new(GbmGlesBackend::with_context_priority(ContextPriority::High))?,
        started: Instant::now(),
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

    loop {
        event_loop.dispatch(Some(Duration::from_millis(16)), &mut runtime)?;
        while let Some(stream) = runtime.listener.accept()? {
            runtime
                .display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))?;
        }
        runtime.display.dispatch_clients(&mut runtime.state)?;
        runtime.ipc.dispatch(&mut runtime.state);
        runtime.state.remove_dead_windows();
        runtime.render_all();
        runtime.display.flush_clients()?;
    }
}
