use std::{collections::HashMap, error::Error, path::Path, sync::Arc, time::Duration};

use astera_config::Config;
use astera_core::{Output, OutputId, Size};
use smithay::{
    backend::{
        drm::{DrmDevice, DrmDeviceFd, DrmEvent, DrmNode},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent},
    },
    reexports::{
        calloop::{EventLoop, LoopHandle, RegistrationToken},
        drm::control::{ModeTypeFlags, connector},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::{Display, ListeningSocket},
    },
    utils::DeviceFd,
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use crate::{
    ipc_server::IpcServer,
    state::{Astera, ClientState},
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
}

struct NativeDevice {
    drm: DrmDevice,
    scanner: DrmScanner,
    registration: RegistrationToken,
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
        let (drm, notifier) = DrmDevice::new(fd, true)?;
        let registration = self
            .handle
            .insert_source(notifier, move |event, _, _runtime| match event {
                DrmEvent::VBlank(crtc) => tracing::trace!(?node, ?crtc, "DRM vblank"),
                DrmEvent::Error(error) => tracing::error!(?node, ?error, "DRM event error"),
            })?;
        self.devices.insert(
            node,
            NativeDevice {
                drm,
                scanner: DrmScanner::new(),
                registration,
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
            match device.scanner.scan_connectors(&device.drm) {
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
                    if crtc.is_none() || connector.modes().is_empty() {
                        tracing::warn!(?node, connector = ?connector.handle(), "connector has no usable CRTC or mode");
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
                    let output = Output::new(
                        id,
                        format!("{node:?}:{name}"),
                        Size::new(i64::from(width), i64::from(height)),
                    );
                    match self.state.connect_output(output) {
                        Ok(()) => {
                            self.connectors.insert((node, connector.handle()), id);
                            tracing::info!(?node, ?id, %name, "DRM connector connected");
                        }
                        Err(error) => {
                            tracing::error!(?node, %error, %name, "could not register output");
                        }
                    }
                }
                DrmScanEvent::Disconnected { connector, .. } => {
                    let Some(id) = self.connectors.remove(&(node, connector.handle())) else {
                        continue;
                    };
                    if let Err(error) = self.state.disconnect_output(id) {
                        tracing::error!(?node, ?id, %error, "could not unregister output");
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
                    device.drm.pause();
                }
                tracing::info!("native session paused");
            }
            SessionEvent::ActivateSession => {
                if let Err(error) = runtime.libinput.resume() {
                    tracing::error!(?error, "could not resume libinput");
                }
                for device in runtime.devices.values_mut() {
                    if let Err(error) = device.drm.activate(false) {
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
        runtime.display.flush_clients()?;
    }
}
