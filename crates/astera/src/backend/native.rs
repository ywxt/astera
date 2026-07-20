use std::{error::Error, sync::Arc, time::Duration};

use astera_config::Config;
use smithay::{
    backend::{
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent},
    },
    reexports::{
        calloop::EventLoop,
        input::Libinput,
        wayland_server::{Display, ListeningSocket},
    },
};

use crate::{
    ipc_server::IpcServer,
    state::{Astera, ClientState},
};

struct NativeLoop {
    display: Display<Astera>,
    state: Astera,
    ipc: IpcServer,
    listener: ListeningSocket,
    libinput: Libinput,
    _session: LibSeatSession,
}

pub fn run(config: Config) -> Result<(), Box<dyn Error>> {
    let mut event_loop: EventLoop<NativeLoop> = EventLoop::try_new()?;
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
                tracing::info!("native session paused");
            }
            SessionEvent::ActivateSession => {
                if let Err(error) = runtime.libinput.resume() {
                    tracing::error!(?error, "could not resume libinput");
                }
                tracing::info!("native session activated");
            }
        })?;
    event_loop
        .handle()
        .insert_source(udev, |event, _, _runtime| match event {
            UdevEvent::Added { device_id, path } => {
                tracing::info!(?device_id, ?path, "DRM device added");
            }
            UdevEvent::Changed { device_id } => {
                tracing::debug!(?device_id, "DRM device changed");
            }
            UdevEvent::Removed { device_id } => {
                tracing::info!(?device_id, "DRM device removed");
            }
        })?;

    let mut runtime = NativeLoop {
        display,
        state,
        ipc,
        listener,
        libinput,
        _session: session,
    };
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
