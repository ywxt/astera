use std::{
    io::{Read, Write},
    os::fd::AsFd,
    os::unix::net::UnixStream,
    sync::Arc,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use astera_core::{Scale120, WorkspaceTransaction};
use smithay::reexports::wayland_server::Display;
use wayland_client::{
    Connection, Dispatch, QueueHandle, delegate_noop,
    globals::registry_queue_init,
    protocol::{
        wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
        wl_data_device::WlDataDevice, wl_data_device_manager::WlDataDeviceManager,
        wl_data_offer::WlDataOffer, wl_data_source::WlDataSource, wl_output::WlOutput,
        wl_pointer::WlPointer, wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm::WlShm,
        wl_shm_pool::WlShmPool, wl_surface::WlSurface,
    },
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::ExtIdleNotificationV1, ext_idle_notifier_v1::ExtIdleNotifierV1,
};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1, ext_session_lock_v1::ExtSessionLockV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1, zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
};
use wayland_protocols::wp::{
    cursor_shape::v1::client::{
        wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
        wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
    },
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::WpFractionalScaleV1,
    },
    idle_inhibit::zv1::client::{
        zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
        zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
    },
    keyboard_shortcuts_inhibit::zv1::client::{
        zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
        zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
    },
    pointer_constraints::zv1::client::{
        zwp_locked_pointer_v1::ZwpLockedPointerV1,
        zwp_pointer_constraints_v1::{self, ZwpPointerConstraintsV1},
    },
    pointer_gestures::zv1::client::{
        zwp_pointer_gesture_hold_v1::ZwpPointerGestureHoldV1,
        zwp_pointer_gesture_pinch_v1::ZwpPointerGesturePinchV1,
        zwp_pointer_gesture_swipe_v1::ZwpPointerGestureSwipeV1,
        zwp_pointer_gestures_v1::ZwpPointerGesturesV1,
    },
    relative_pointer::zv1::client::{
        zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
        zwp_relative_pointer_v1::ZwpRelativePointerV1,
    },
    tablet::zv2::client::{
        zwp_tablet_manager_v2::ZwpTabletManagerV2, zwp_tablet_seat_v2::ZwpTabletSeatV2,
    },
    text_input::zv3::client::{
        zwp_text_input_manager_v3::ZwpTextInputManagerV3, zwp_text_input_v3::ZwpTextInputV3,
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::XdgPopup, xdg_positioner::XdgPositioner, xdg_surface::XdgSurface,
    xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};
use wayland_protocols::xdg::{
    activation::v1::client::{
        xdg_activation_token_v1::XdgActivationTokenV1, xdg_activation_v1::XdgActivationV1,
    },
    decoration::zv1::client::{
        zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
        zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
    },
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2, zwp_input_method_v2::ZwpInputMethodV2,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::ZwlrLayerShellV1, zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
};
use wayland_protocols_wlr::output_power_management::v1::client::{
    zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1,
    zwlr_output_power_v1::{self, ZwlrOutputPowerV1},
};

use super::clock::testing::ManualClock;
use super::*;

struct TestClient;

impl Dispatch<WlRegistry, wayland_client::globals::GlobalListContents> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: wayland_client::protocol::wl_registry::Event,
        _data: &wayland_client::globals::GlobalListContents,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(TestClient: ignore WlCompositor);
delegate_noop!(TestClient: ignore WlSurface);
delegate_noop!(TestClient: ignore WlCallback);
delegate_noop!(TestClient: ignore WlSeat);
delegate_noop!(TestClient: ignore WlOutput);
delegate_noop!(TestClient: ignore WlPointer);
delegate_noop!(TestClient: ignore WlShm);
delegate_noop!(TestClient: ignore WlShmPool);
delegate_noop!(TestClient: ignore WlBuffer);
delegate_noop!(TestClient: ignore WlDataDeviceManager);
delegate_noop!(TestClient: ignore WlDataOffer);
delegate_noop!(TestClient: ignore WlDataSource);
delegate_noop!(TestClient: ignore XdgToplevel);
delegate_noop!(TestClient: ignore XdgPopup);
delegate_noop!(TestClient: ignore XdgPositioner);
delegate_noop!(TestClient: ignore WpViewporter);
delegate_noop!(TestClient: ignore WpViewport);
delegate_noop!(TestClient: ignore WpFractionalScaleManagerV1);
delegate_noop!(TestClient: ignore WpFractionalScaleV1);
delegate_noop!(TestClient: ignore ZwlrLayerShellV1);
delegate_noop!(TestClient: ignore XdgActivationV1);
delegate_noop!(TestClient: ignore XdgActivationTokenV1);
delegate_noop!(TestClient: ignore ZxdgDecorationManagerV1);
delegate_noop!(TestClient: ignore ZxdgToplevelDecorationV1);
delegate_noop!(TestClient: ignore ExtIdleNotifierV1);
delegate_noop!(TestClient: ignore ExtIdleNotificationV1);
delegate_noop!(TestClient: ignore ZwpIdleInhibitManagerV1);
delegate_noop!(TestClient: ignore ZwpIdleInhibitorV1);
delegate_noop!(TestClient: ignore ExtSessionLockManagerV1);
delegate_noop!(TestClient: ignore ExtSessionLockV1);
delegate_noop!(TestClient: ignore ZwlrOutputPowerManagerV1);
delegate_noop!(TestClient: ignore ZwlrOutputPowerV1);
delegate_noop!(TestClient: ignore ZwpRelativePointerManagerV1);
delegate_noop!(TestClient: ignore ZwpRelativePointerV1);
delegate_noop!(TestClient: ignore ZwpPointerConstraintsV1);
delegate_noop!(TestClient: ignore ZwpLockedPointerV1);
delegate_noop!(TestClient: ignore ZwpKeyboardShortcutsInhibitManagerV1);
delegate_noop!(TestClient: ignore ZwpKeyboardShortcutsInhibitorV1);
delegate_noop!(TestClient: ignore ZwpPointerGesturesV1);
delegate_noop!(TestClient: ignore ZwpPointerGestureSwipeV1);
delegate_noop!(TestClient: ignore ZwpPointerGesturePinchV1);
delegate_noop!(TestClient: ignore ZwpPointerGestureHoldV1);
delegate_noop!(TestClient: ignore ZwpTabletManagerV2);
delegate_noop!(TestClient: ignore ZwpTabletSeatV2);
delegate_noop!(TestClient: ignore WpCursorShapeManagerV1);
delegate_noop!(TestClient: ignore WpCursorShapeDeviceV1);
delegate_noop!(TestClient: ignore ZwpTextInputManagerV3);
delegate_noop!(TestClient: ignore ZwpTextInputV3);
delegate_noop!(TestClient: ignore ZwpInputMethodManagerV2);
delegate_noop!(TestClient: ignore ZwpInputMethodV2);
delegate_noop!(TestClient: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(TestClient: ignore ZwpVirtualKeyboardV1);
delegate_noop!(TestClient: ignore ZwpLinuxDmabufV1);

impl Dispatch<ZwpLinuxDmabufFeedbackV1, (mpsc::Sender<()>, mpsc::Sender<Vec<u8>>)> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpLinuxDmabufFeedbackV1,
        event: wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::Event,
        feedback: &(mpsc::Sender<()>, mpsc::Sender<Vec<u8>>),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::Event;
        match event {
            Event::Done => {
                let _ = feedback.0.send(());
            }
            Event::TrancheTargetDevice { device } => {
                let _ = feedback.1.send(device);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, (Vec<u8>, mpsc::Sender<()>)> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &WlDataSource,
        event: wayland_client::protocol::wl_data_source::Event,
        payload: &(Vec<u8>, mpsc::Sender<()>),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_data_source::Event::Send { fd, .. } = event {
            let mut file = std::fs::File::from(fd);
            file.write_all(&payload.0).unwrap();
            let _ = payload.1.send(());
        }
    }
}

impl Dispatch<ZwpInputMethodV2, mpsc::Sender<()>> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpInputMethodV2,
        event: wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_v2::Event,
        unavailable: &mpsc::Sender<()>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(
            event,
            wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_v2::Event::Unavailable
        ) {
            let _ = unavailable.send(());
        }
    }
}

impl Dispatch<
    wayland_protocols::ext::session_lock::v1::client::ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
    mpsc::Sender<(u32, u32, u32)>,
> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &wayland_protocols::ext::session_lock::v1::client::ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
        event: wayland_protocols::ext::session_lock::v1::client::ext_session_lock_surface_v1::Event,
        serials: &mpsc::Sender<(u32, u32, u32)>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::ext::session_lock::v1::client::ext_session_lock_surface_v1::Event::Configure { serial, width, height } = event {
            let _ = serials.send((serial, width, height));
        }
    }
}

impl Dispatch<WlDataDevice, mpsc::Sender<()>> for TestClient {
    wayland_client::event_created_child!(TestClient, WlDataDevice, [
        0 => (WlDataOffer, ())
    ]);

    fn event(
        _state: &mut Self,
        _proxy: &WlDataDevice,
        event: wayland_client::protocol::wl_data_device::Event,
        selection: &mpsc::Sender<()>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(
            event,
            wayland_client::protocol::wl_data_device::Event::Selection { id: Some(_) }
        ) {
            let _ = selection.send(());
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for TestClient {
    fn event(
        _state: &mut Self,
        proxy: &ZwlrLayerSurfaceV1,
        event: wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event::Configure {
            serial,
            ..
        } = event
        {
            proxy.ack_configure(serial);
        }
    }
}

impl Dispatch<XdgWmBase, ()> for TestClient {
    fn event(
        _state: &mut Self,
        proxy: &XdgWmBase,
        event: wayland_protocols::xdg::shell::client::xdg_wm_base::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_wm_base::Event::Ping { serial } = event {
            proxy.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for TestClient {
    fn event(
        _state: &mut Self,
        proxy: &XdgSurface,
        event: wayland_protocols::xdg::shell::client::xdg_surface::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_surface::Event::Configure { serial } =
            event
        {
            proxy.ack_configure(serial);
        }
    }
}

fn dispatch_until(
    display: &mut Display<Astera>,
    state: &mut Astera,
    mut condition: impl FnMut(&mut Astera) -> bool,
) {
    for _ in 0..10_000 {
        display.dispatch_clients(state).unwrap();
        display.flush_clients().unwrap();
        if condition(state) {
            return;
        }
        thread::yield_now();
    }
    panic!("Wayland harness condition did not become true");
}

#[test]
fn output_power_requests_are_exclusive_coalesced_and_backend_confirmed() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state.enable_output_power_management();
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (requested_tx, requested_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, event_queue) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = event_queue.handle();
        let output = globals.bind::<WlOutput, _, _>(&queue, 1..=4, ()).unwrap();
        let manager = globals
            .bind::<ZwlrOutputPowerManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let control = manager.get_output_power(&output, &queue, ());
        let rejected = manager.get_output_power(&output, &queue, ());
        rejected.destroy();
        control.set_mode(zwlr_output_power_v1::Mode::Off);
        control.set_mode(zwlr_output_power_v1::Mode::On);
        control.set_mode(zwlr_output_power_v1::Mode::Off);
        connection.flush().unwrap();
        requested_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        requested_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        !state.pending_output_power.is_empty()
    });
    assert_eq!(state.output_power_controls.len(), 1);
    assert_eq!(
        state.take_output_power_requests(),
        vec![(OutputId(0), false)],
        "only the latest request for an output should reach KMS"
    );
    state.confirm_output_power(OutputId(0), false);
    assert!(!state.output_power_modes[&OutputId(0)]);
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn privileged_input_globals_are_hidden_from_ordinary_clients() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (result_tx, result_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, queue) = registry_queue_init::<TestClient>(&connection).unwrap();
        let handle = queue.handle();
        result_tx
            .send((
                globals
                    .bind::<ZwpInputMethodManagerV2, _, _>(&handle, 1..=1, ())
                    .is_err(),
                globals
                    .bind::<ZwpVirtualKeyboardManagerV1, _, _>(&handle, 1..=1, ())
                    .is_err(),
                globals
                    .bind::<ZwpTextInputManagerV3, _, _>(&handle, 1..=1, ())
                    .is_ok(),
            ))
            .unwrap();
    });
    let mut result = None;
    dispatch_until(&mut display, &mut state, |_| match result_rx.try_recv() {
        Ok(value) => {
            result = Some(value);
            true
        }
        Err(_) => false,
    });
    assert_eq!(result, Some((true, true, true)));
    client.join().unwrap();
}

#[test]
fn dmabuf_with_a_render_node_advertises_version_four_feedback() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state.enable_dmabuf(
        Some(1),
        [smithay::backend::allocator::Format {
            code: smithay::backend::allocator::Fourcc::Argb8888,
            modifier: smithay::backend::allocator::Modifier::Linear,
        }],
    );
    state.register_output_dmabuf_feedback(
        OutputId(0),
        2,
        [smithay::backend::allocator::Format {
            code: smithay::backend::allocator::Fourcc::Abgr8888,
            modifier: smithay::backend::allocator::Modifier::Linear,
        }],
    );
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (result_tx, result_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let Ok(dmabuf) = globals.bind::<ZwpLinuxDmabufV1, _, _>(&queue, 4..=4, ()) else {
            result_tx.send(false).unwrap();
            return;
        };
        let (done_tx, done_rx) = mpsc::channel();
        let (target_tx, target_rx) = mpsc::channel();
        let surface = compositor.create_surface(&queue, ());
        let _feedback = dmabuf.get_surface_feedback(&surface, &queue, (done_tx, target_tx));
        connection.flush().unwrap();
        let connected = events.roundtrip(&mut TestClient).is_ok();
        let target_device = 2u64.to_ne_bytes();
        result_tx
            .send(
                connected
                    && done_rx.try_recv().is_ok()
                    && target_rx.try_iter().any(|device| device == target_device),
            )
            .unwrap();
    });
    let mut advertised = None;
    dispatch_until(&mut display, &mut state, |_| match result_rx.try_recv() {
        Ok(value) => {
            advertised = Some(value);
            true
        }
        Err(_) => false,
    });
    assert_eq!(advertised, Some(true));
    client.join().unwrap();
}

#[test]
fn second_input_method_is_unavailable_without_disconnect() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::trusted_input()))
        .unwrap();
    let (result_tx, result_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<ZwpInputMethodManagerV2, _, _>(&queue, 1..=1, ())
            .unwrap();
        let _first = manager.get_input_method(&seat, &queue, ());
        let (unavailable_tx, unavailable_rx) = mpsc::channel();
        let _second = manager.get_input_method(&seat, &queue, unavailable_tx);
        connection.flush().unwrap();
        let connected = events.roundtrip(&mut TestClient).is_ok();
        result_tx
            .send((connected, unavailable_rx.try_recv().is_ok()))
            .unwrap();
    });

    let mut result = None;
    dispatch_until(&mut display, &mut state, |_| match result_rx.try_recv() {
        Ok(value) => {
            result = Some(value);
            true
        }
        Err(_) => false,
    });
    assert_eq!(result, Some((true, true)));
    client.join().unwrap();
}

#[test]
fn focused_client_receives_clipboard_selection_offer() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (offer_tx, offer_rx) = mpsc::channel();
    let (continue_tx, continue_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let data_manager = globals
            .bind::<WlDataDeviceManager, _, _>(&queue, 1..=3, ())
            .unwrap();
        let (selection_tx, selection_rx) = mpsc::channel();
        let _device = data_manager.get_data_device(&seat, &queue, selection_tx);
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        surface.commit();
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        continue_rx.recv().unwrap();
        while selection_rx.try_recv().is_err() {
            events.blocking_dispatch(&mut TestClient).unwrap();
        }
        offer_tx.send(()).unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    state.map_toplevel(0);
    smithay::wayland::selection::data_device::set_data_device_selection::<Astera>(
        &display.handle(),
        &state.seat,
        vec!["text/plain;charset=utf-8".into()],
        (),
    );
    display.flush_clients().unwrap();
    continue_tx.send(()).unwrap();
    offer_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("focused data device did not receive the clipboard selection");
    client.join().unwrap();
}

#[test]
fn clipboard_selection_transfers_requested_mime_bytes() {
    const PAYLOAD: &[u8] = b"astera clipboard payload";

    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (serial_tx, serial_rx) = mpsc::sync_channel(0);
    let (selection_tx, selection_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<WlDataDeviceManager, _, _>(&queue, 1..=3, ())
            .unwrap();
        let device = manager.get_data_device(&seat, &queue, mpsc::channel().0);
        let (sent_tx, sent_rx) = mpsc::channel();
        let source = manager.create_data_source(&queue, (PAYLOAD.to_vec(), sent_tx));
        source.offer("text/plain;charset=utf-8".into());
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        surface.commit();
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        device.set_selection(Some(&source), serial_rx.recv().unwrap());
        connection.flush().unwrap();
        while sent_rx.try_recv().is_err() {
            events.blocking_dispatch(&mut TestClient).unwrap();
        }
        selection_tx.send(()).unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    state.map_toplevel(0);
    let serial = state.next_serial();
    let keyboard = state.keyboard.clone();
    let focused = state.windows[0].surface.wl_surface().clone();
    keyboard.set_focus(&mut state, Some(focused), serial);
    serial_tx.send(serial.into()).unwrap();

    let (read_fd, write_fd) = rustix::pipe::pipe().unwrap();
    let mut requested = false;
    dispatch_until(&mut display, &mut state, |state| {
        if requested {
            return selection_rx.try_recv().is_ok();
        }
        requested = smithay::wayland::selection::data_device::request_data_device_client_selection::<Astera>(
            &state.seat,
            "text/plain;charset=utf-8".into(),
            write_fd.try_clone().unwrap(),
        )
        .is_ok();
        false
    });
    drop(write_fd);
    let mut bytes = Vec::new();
    std::fs::File::from(read_fd)
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, PAYLOAD);
    client.join().unwrap();
}

#[test]
fn session_lock_is_fail_closed_before_confirmation_and_after_disconnect() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (requested_tx, requested_rx) = mpsc::sync_channel(0);
    let (invalid_tx, invalid_rx) = mpsc::sync_channel(0);
    let (invalid_sent_tx, invalid_sent_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, event_queue) = registry_queue_init::<TestClient>(&connection).unwrap();
        let manager = globals
            .bind::<ExtSessionLockManagerV1, _, _>(&event_queue.handle(), 1..=1, ())
            .unwrap();
        let lock = manager.lock(&event_queue.handle(), ());
        connection.flush().unwrap();
        requested_tx.send(()).unwrap();
        invalid_rx.recv().unwrap();
        lock.unlock_and_destroy();
        connection.flush().unwrap();
        invalid_sent_tx.send(()).unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        requested_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| state.session_is_locked());
    assert!(matches!(state.session_state, SessionState::Locking { .. }));
    assert!(state.render_roots().is_empty());

    invalid_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        invalid_sent_rx.try_recv().is_ok()
    });
    for _ in 0..8 {
        display.dispatch_clients(&mut state).unwrap();
    }
    assert!(matches!(state.session_state, SessionState::Locking { .. }));
    state.lock_frame_presented(OutputId(0), None);
    assert!(matches!(state.session_state, SessionState::Locking { .. }));
    state.session_output_connected(OutputId(1));
    state.confirm_output_power(OutputId(1), false);
    state.confirm_output_power(OutputId(1), true);
    let generation = state.locking_generation();
    state.lock_frame_presented(OutputId(0), generation);
    assert!(matches!(state.session_state, SessionState::Locking { .. }));
    state.lock_frame_presented(OutputId(1), generation);
    assert!(matches!(state.session_state, SessionState::Locked { .. }));
    client.join().unwrap();
    for _ in 0..8 {
        display.dispatch_clients(&mut state).unwrap();
    }
    assert!(state.session_is_locked());
    assert!(state.render_roots().is_empty());
}

#[test]
fn session_lock_surface_allows_damage_only_commits() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let (result_tx, result_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let output = globals.bind::<WlOutput, _, _>(&queue, 1..=4, ()).unwrap();
        let shm = globals.bind::<WlShm, _, _>(&queue, 1..=1, ()).unwrap();
        let manager = globals
            .bind::<ExtSessionLockManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let lock = manager.lock(&queue, ());
        let surface = compositor.create_surface(&queue, ());
        let (configure_tx, configure_rx) = mpsc::channel();
        let lock_surface = lock.get_lock_surface(&surface, &output, &queue, configure_tx);
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        let (serial, width, height) = loop {
            if let Ok(configure) = configure_rx.try_recv() {
                break configure;
            }
            events.blocking_dispatch(&mut TestClient).unwrap();
        };
        lock_surface.ack_configure(serial);
        let stride = i32::try_from(width.saturating_mul(4)).unwrap();
        let length = u64::try_from(stride).unwrap() * u64::from(height);
        let fd =
            rustix::fs::memfd_create("astera-lock-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        rustix::fs::ftruncate(&fd, length).unwrap();
        let pool = shm.create_pool(fd.as_fd(), i32::try_from(length).unwrap(), &queue, ());
        let buffer = pool.create_buffer(
            0,
            i32::try_from(width).unwrap(),
            i32::try_from(height).unwrap(),
            stride,
            wayland_client::protocol::wl_shm::Format::Argb8888,
            &queue,
            (),
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.commit();
        // No attach here: this valid commit retains the buffer from the preceding commit.
        surface.damage_buffer(0, 0, 1, 1);
        surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();
        result_tx
            .send(events.roundtrip(&mut TestClient).is_ok())
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        !state.lock_surfaces.is_empty()
    });
    dispatch_until(&mut display, &mut state, |_| {
        committed_rx.try_recv().is_ok()
    });
    let mut result = None;
    dispatch_until(&mut display, &mut state, |_| match result_rx.try_recv() {
        Ok(value) => {
            result = Some(value);
            true
        }
        Err(_) => false,
    });
    assert_eq!(result, Some(true));
    client.join().unwrap();
}

#[test]
fn decoration_and_activation_globals_are_advertised() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    assert!(state.seat.get_touch().is_some());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::trusted_input()))
        .unwrap();

    let (requested_tx, requested_rx) = mpsc::sync_channel(0);
    let (destroy_surface_tx, destroy_surface_rx) = mpsc::sync_channel(0);
    let (surface_destroyed_tx, surface_destroyed_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, event_queue) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = event_queue.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let decorations = globals
            .bind::<ZxdgDecorationManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let _activation = globals
            .bind::<XdgActivationV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let idle = globals
            .bind::<ExtIdleNotifierV1, _, _>(&queue, 1..=2, ())
            .unwrap();
        let idle_inhibit = globals
            .bind::<ZwpIdleInhibitManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let pointer = seat.get_pointer(&queue, ());
        let relative_manager = globals
            .bind::<ZwpRelativePointerManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let constraints = globals
            .bind::<ZwpPointerConstraintsV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let shortcut_inhibit = globals
            .bind::<ZwpKeyboardShortcutsInhibitManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let gestures = globals
            .bind::<ZwpPointerGesturesV1, _, _>(&queue, 1..=3, ())
            .unwrap();
        let tablet_manager = globals
            .bind::<ZwpTabletManagerV2, _, _>(&queue, 1..=1, ())
            .unwrap();
        let cursor_shapes = globals
            .bind::<WpCursorShapeManagerV1, _, _>(&queue, 1..=2, ())
            .unwrap();
        let text_inputs = globals
            .bind::<ZwpTextInputManagerV3, _, _>(&queue, 1..=1, ())
            .unwrap();
        let input_methods = globals
            .bind::<ZwpInputMethodManagerV2, _, _>(&queue, 1..=1, ())
            .unwrap();
        let virtual_keyboards = globals
            .bind::<ZwpVirtualKeyboardManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let _idle_notification = idle.get_idle_notification(0, &seat, &queue, ());
        let surface = compositor.create_surface(&queue, ());
        let _relative_pointer = relative_manager.get_relative_pointer(&pointer, &queue, ());
        let _locked_pointer = constraints.lock_pointer(
            &surface,
            &pointer,
            None,
            zwp_pointer_constraints_v1::Lifetime::Persistent,
            &queue,
            (),
        );
        let _shortcut_inhibitor = shortcut_inhibit.inhibit_shortcuts(&surface, &seat, &queue, ());
        let _swipe = gestures.get_swipe_gesture(&pointer, &queue, ());
        let _pinch = gestures.get_pinch_gesture(&pointer, &queue, ());
        let _hold = gestures.get_hold_gesture(&pointer, &queue, ());
        let _tablet_seat = tablet_manager.get_tablet_seat(&seat, &queue, ());
        let _cursor_shape = cursor_shapes.get_pointer(&pointer, &queue, ());
        let _text_input = text_inputs.get_text_input(&seat, &queue, ());
        let _input_method = input_methods.get_input_method(&seat, &queue, ());
        let _virtual_keyboard = virtual_keyboards.create_virtual_keyboard(&seat, &queue, ());
        let _first_inhibitor = idle_inhibit.create_inhibitor(&surface, &queue, ());
        let _second_inhibitor = idle_inhibit.create_inhibitor(&surface, &queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let toplevel = xdg_surface.get_toplevel(&queue, ());
        let decoration = decorations.get_toplevel_decoration(&toplevel, &queue, ());
        decoration.set_mode(
            wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::Mode::ServerSide,
        );
        surface.commit();
        connection.flush().unwrap();
        requested_tx.send(()).unwrap();
        destroy_surface_rx.recv().unwrap();
        decoration.destroy();
        toplevel.destroy();
        xdg_surface.destroy();
        surface.destroy();
        connection.flush().unwrap();
        surface_destroyed_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        requested_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        state.windows.first().is_some_and(|window| {
            window.surface.with_pending_state(|pending| {
                pending.decoration_mode
                    == Some(
                        smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode::ClientSide,
                    )
            })
        })
    });
    let inhibited_surface = state.windows[0].surface.wl_surface().clone();
    let inhibitor = state
        .seat
        .keyboard_shortcuts_inhibitor_for_surface(&inhibited_surface)
        .expect("client created shortcut inhibitor");
    let keyboard = state.keyboard.clone();
    let serial = state.next_serial();
    keyboard.set_focus(&mut state, None, serial);
    let seat_handle = state.seat.clone();
    state.update_shortcut_inhibitor(&seat_handle, None);
    assert!(!inhibitor.is_active());
    let repeat_key = smithay::backend::input::Keycode::new(30);
    state.key_repeat.intercept(repeat_key);
    state.key_repeat.register(
        repeat_key,
        BindingModifiers::default(),
        astera_config::Action::Quit,
        100,
        state.clock.now(),
    );
    let serial = state.next_serial();
    keyboard.set_focus(&mut state, Some(inhibited_surface), serial);
    assert!(inhibitor.is_active());
    assert!(state.key_repeat.deadline().is_none());
    let serial = state.next_serial();
    keyboard.set_focus(&mut state, None, serial);
    state.update_shortcut_inhibitor(&seat_handle, None);
    assert!(!inhibitor.is_active());
    let constrained = state.windows[0].surface.wl_surface().clone();
    assert!(
        smithay::wayland::pointer_constraints::with_pointer_constraint(
            &constrained,
            &state.pointer,
            |constraint| constraint.is_some()
        )
    );
    dispatch_until(&mut display, &mut state, |state| {
        !state.idle_notifications.is_empty()
    });
    dispatch_until(&mut display, &mut state, |state| {
        state.idle_inhibitors.values().copied().sum::<usize>() == 2
    });
    assert_eq!(state.idle_inhibitors.values().copied().sum::<usize>(), 2);
    assert!(
        state
            .next_timer_deadline()
            .is_some_and(|deadline| deadline <= state.clock.now())
    );
    state.process_idle_timers();
    // Exercise the denial path and one-shot lifetime without manufacturing a fake input serial.
    // An invalid token must still notify the user via urgency, then disappear permanently.
    state.windows[0].mapped = true;
    let target = state.windows[0].surface.wl_surface().clone();
    let (token, data) = {
        let (token, data) = state.xdg_activation_state.create_external_token(None);
        (token.clone(), data.clone())
    };
    state.request_activation(token.clone(), data, target);
    assert!(state.windows[0].urgent);
    assert!(state.xdg_activation_state.data_for_token(&token).is_none());
    destroy_surface_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        surface_destroyed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        state.idle_inhibitors.is_empty()
    });
    assert!(
        state.idle_inhibitors.is_empty(),
        "idle inhibitors must stop affecting compositor state when their surface is destroyed"
    );
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn layer_exclusive_zone_reduces_usable_viewport() {
    use wayland_protocols_wlr::layer_shell::v1::client::{
        zwlr_layer_shell_v1::Layer,
        zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity},
    };

    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state
        .configure_output(
            OutputId(0),
            Size::new(1280, 720),
            Size::new(1280, 720),
            Scale120::ONE,
            OutputTransform::Normal,
        )
        .unwrap();
    state.publish_public_state();
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let (destroy_popup_tx, destroy_popup_rx) = mpsc::sync_channel(0);
    let (popup_destroyed_tx, popup_destroyed_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals
            .bind::<ZwlrLayerShellV1, _, _>(&queue, 1..=4, ())
            .unwrap();
        let xdg = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let viewporter = globals
            .bind::<WpViewporter, _, _>(&queue, 1..=1, ())
            .unwrap();
        let fractional = globals
            .bind::<WpFractionalScaleManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let viewport = viewporter.get_viewport(&surface, &queue, ());
        viewport.set_destination(1280, 720);
        let _fractional_scale = fractional.get_fractional_scale(&surface, &queue, ());
        let layer =
            shell.get_layer_surface(&surface, None, Layer::Top, "test-panel".into(), &queue, ());
        layer.set_size(0, 32);
        layer.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
        layer.set_exclusive_zone(32);
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        surface.commit();

        let popup_surface = compositor.create_surface(&queue, ());
        let popup_xdg_surface = xdg.get_xdg_surface(&popup_surface, &queue, ());
        let positioner = xdg.create_positioner(&queue, ());
        positioner.set_size(200, 120);
        positioner.set_anchor_rect(1270, 20, 1, 1);
        positioner.set_constraint_adjustment(
            wayland_protocols::xdg::shell::client::xdg_positioner::ConstraintAdjustment::SlideX
                | wayland_protocols::xdg::shell::client::xdg_positioner::ConstraintAdjustment::SlideY,
        );
        positioner.set_reactive();
        let popup = popup_xdg_surface.get_popup(None, &positioner, &queue, ());
        layer.get_popup(&popup);
        popup_surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();
        destroy_popup_rx.recv().unwrap();
        popup.destroy();
        connection.flush().unwrap();
        popup_destroyed_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });
    dispatch_until(&mut display, &mut state, |_| {
        committed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        state
            .usable_rect(OutputId(0))
            .is_some_and(|rect| rect.origin.y == 32)
    });
    let usable = state.usable_rect(OutputId(0)).unwrap();
    assert_eq!(usable, astera_core::Rect::new(0, 32, 1280, 688));
    dispatch_until(&mut display, &mut state, |state| {
        state.layers.first().is_some_and(|layer| {
            PopupManager::popups_for_surface(layer.surface.wl_surface())
                .next()
                .is_some()
        })
    });
    let layer_root = state.layers[0].surface.wl_surface();
    let (popup, _) = PopupManager::popups_for_surface(layer_root)
        .next()
        .expect("layer popup should be tracked");
    let PopupKind::Xdg(popup) = popup else {
        panic!("layer-shell only accepts XDG popups");
    };
    let geometry = popup.with_pending_state(|pending| pending.geometry);
    assert_eq!(geometry.loc, (1080, 0).into());
    assert_eq!(geometry.size, (200, 120).into());
    state
        .configure_output(
            OutputId(0),
            Size::new(1000, 720),
            Size::new(1000, 720),
            Scale120::ONE,
            OutputTransform::Normal,
        )
        .unwrap();
    let geometry = popup.with_pending_state(|pending| pending.geometry);
    assert_eq!(geometry.loc, (800, 0).into());
    state.layers[0].mapped = true;
    state.on_demand_layer_focus = Some(state.layers[0].id);
    state.sync_keyboard_focus();
    let layer_surface = state.layers[0].surface.wl_surface().clone();
    assert_eq!(state.keyboard.current_focus(), Some(layer_surface.clone()));
    state.sync_keyboard_focus();
    assert_eq!(state.keyboard.current_focus(), Some(layer_surface));
    state.layers[0].mapped = false;
    state.sync_keyboard_focus();
    assert_eq!(state.on_demand_layer_focus, None);
    let generation = state.render_generation();
    destroy_popup_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        popup_destroyed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        PopupManager::popups_for_surface(state.layers[0].surface.wl_surface())
            .next()
            .is_none()
    });
    assert!(
        state.render_generation() > generation,
        "destroying a popup must schedule repaint of its former pixels"
    );
    assert!(
        state.publish_public_state().iter().any(|event| matches!(
            event.event,
            astera_ipc::wire::v1::Event::OutputChanged { .. }
        )),
        "layer commits must invalidate public state"
    );
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn uncommitted_toplevel_does_not_map_and_role_destroy_cleans_up() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (mapped_tx, mapped_rx) = mpsc::sync_channel(0);
    let (destroy_tx, destroy_rx) = mpsc::sync_channel(0);
    let (role_destroyed_tx, role_destroyed_rx) = mpsc::sync_channel(0);
    let (surface_destroy_tx, surface_destroy_rx) = mpsc::sync_channel(0);

    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut event_queue) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = event_queue.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let toplevel = xdg_surface.get_toplevel(&queue, ());
        surface.commit();
        connection.flush().unwrap();
        mapped_tx.send(()).unwrap();
        destroy_rx.recv().unwrap();
        toplevel.destroy();
        xdg_surface.destroy();
        connection.flush().unwrap();
        role_destroyed_tx.send(()).unwrap();
        surface_destroy_rx.recv().unwrap();
        surface.destroy();
        connection.flush().unwrap();
        // Consume any final configure/ping without waiting for another roundtrip.
        event_queue.dispatch_pending(&mut TestClient).unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| mapped_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    let capabilities = state.windows[0].surface.current_state().capabilities;
    assert_eq!(capabilities.capabilities().count(), 3);
    assert!(
        capabilities
            .capabilities()
            .any(|capability| *capability == xdg_toplevel::WmCapabilities::Maximize)
    );
    assert!(
        capabilities
            .capabilities()
            .any(|capability| *capability == xdg_toplevel::WmCapabilities::Fullscreen)
    );
    assert!(
        capabilities
            .capabilities()
            .any(|capability| *capability == xdg_toplevel::WmCapabilities::Minimize)
    );
    let role_window = state.windows[0].id;
    assert!(!state.windows[0].mapped);
    assert_eq!(
        state.desktop.find_window(role_window),
        Err(astera_core::DesktopError::UnknownWindow(role_window))
    );
    destroy_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        role_destroyed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| state.windows.is_empty());
    surface_destroy_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn initial_toplevel_mode_requests_are_retained_until_mapping() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (step_tx, step_rx) = mpsc::sync_channel(0);
    let (continue_tx, continue_rx) = mpsc::sync_channel(0);

    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, _events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = _events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let toplevel = xdg_surface.get_toplevel(&queue, ());

        toplevel.set_maximized();
        connection.flush().unwrap();
        step_tx.send(1).unwrap();
        continue_rx.recv().unwrap();

        toplevel.unset_maximized();
        toplevel.set_fullscreen(None);
        connection.flush().unwrap();
        step_tx.send(2).unwrap();
        continue_rx.recv().unwrap();

        toplevel.set_minimized();
        connection.flush().unwrap();
        step_tx.send(3).unwrap();
        continue_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| step_rx.try_recv() == Ok(1));
    dispatch_until(&mut display, &mut state, |state| {
        state
            .windows
            .first()
            .is_some_and(|window| window.initial_mode == Some(WindowMode::Maximized))
    });
    assert!(!state.windows[0].mapped);
    continue_tx.send(()).unwrap();

    dispatch_until(&mut display, &mut state, |_| step_rx.try_recv() == Ok(2));
    dispatch_until(&mut display, &mut state, |state| {
        state
            .windows
            .first()
            .is_some_and(|window| window.initial_mode == Some(WindowMode::Fullscreen))
    });
    assert!(!state.windows[0].mapped);
    let window = state.windows[0].id;
    state.map_toplevel(0);
    let workspace = state.desktop.find_window(window).unwrap();
    assert_eq!(
        state
            .desktop
            .workspace(workspace)
            .unwrap()
            .window_mode(window),
        Some(WindowMode::Fullscreen)
    );
    assert_eq!(state.windows[0].initial_mode, None);
    continue_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| step_rx.try_recv() == Ok(3));
    dispatch_until(&mut display, &mut state, |state| {
        state
            .desktop
            .workspace(workspace)
            .unwrap()
            .window_mode(window)
            == Some(WindowMode::Minimized)
    });
    assert_eq!(
        state
            .desktop
            .workspace(workspace)
            .unwrap()
            .window_mode(window),
        Some(WindowMode::Minimized)
    );
    assert_eq!(state.visual_geometry(window), None);
    state.desktop.focus_window(window).unwrap();
    assert_eq!(
        state
            .desktop
            .workspace(workspace)
            .unwrap()
            .window_mode(window),
        Some(WindowMode::Fullscreen)
    );
    continue_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn frame_callback_snapshot_does_not_relock_surface_tree_state() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);

    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        let _frame = surface.frame(&queue, ());
        surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        committed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    let surface = state.windows[0].surface.wl_surface().clone();
    state.pointer_location = (12.5, 30.0).into();
    let seat = state.seat.clone();
    <Astera as ClientDndGrabHandler>::started(
        &mut state,
        None,
        Some(surface.clone()),
        seat.clone(),
    );
    let (icon, location, scale) = state.dnd_icon_render_source(OutputId(0)).unwrap();
    assert_eq!(icon, surface);
    assert_eq!(location, (13, 30).into());
    assert_eq!(scale, 1.0);
    assert!(state.dnd_icon_render_source(OutputId(99)).is_none());
    <Astera as ClientDndGrabHandler>::dropped(&mut state, None, false, seat);
    assert!(state.dnd_icon_render_source(OutputId(0)).is_none());

    let surface = state.windows[0].surface.wl_surface().clone();
    let (snapshot_tx, snapshot_rx) = mpsc::sync_channel(0);
    thread::spawn(move || {
        let count = crate::backend::render::frame_callbacks_for_tree(&surface).len();
        snapshot_tx.send(count).unwrap();
    });
    assert_eq!(
        snapshot_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        1,
        "surface traversal must capture the pending callback without recursively locking state"
    );

    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn disconnected_xdg_client_is_removed_without_explicit_destroy() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (created_tx, created_rx) = mpsc::sync_channel(0);
    let (disconnect_tx, disconnect_rx) = mpsc::sync_channel(0);

    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, event_queue) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = event_queue.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        surface.commit();
        connection.flush().unwrap();
        created_tx.send(()).unwrap();
        disconnect_rx.recv().unwrap();
        // Dropping the connection simulates a crashed client.
    });

    dispatch_until(&mut display, &mut state, |_| created_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    disconnect_tx.send(()).unwrap();
    client.join().unwrap();
    dispatch_until(&mut display, &mut state, |state| {
        state.remove_dead_windows();
        state.windows.is_empty()
    });
}

#[test]
fn compositor_time_can_be_advanced_without_sleeping() {
    let display = Display::<Astera>::new().unwrap();
    let start = Instant::now();
    let clock = Arc::new(ManualClock::new(start));
    let state = Astera::new_with_clock(&display.handle(), Config::default(), clock.clone());
    assert_eq!(state.clock.now(), start);
    clock.advance(Duration::from_millis(300));
    assert_eq!(state.clock.now(), start + Duration::from_millis(300));
}

#[test]
fn interactive_resize_preserves_opposite_edge_and_client_limits() {
    let start = astera_core::Rect::new(100, 100, 400, 300);
    let top_left = resized_rect(
        start,
        (100.0, 100.0),
        SmithayPoint::from((450.0, 350.0)),
        ResizeEdges {
            top: true,
            bottom: false,
            left: true,
            right: false,
        },
        Size::new(200, 160),
        Size::new(600, 500),
    );
    assert_eq!(top_left, astera_core::Rect::new(300, 240, 200, 160));

    let bottom_right = resized_rect(
        start,
        (500.0, 400.0),
        SmithayPoint::from((900.0, 900.0)),
        ResizeEdges {
            top: false,
            bottom: true,
            left: false,
            right: true,
        },
        Size::new(200, 160),
        Size::new(600, 500),
    );
    assert_eq!(bottom_right, astera_core::Rect::new(100, 100, 600, 500));
}

#[test]
fn hotplug_moves_disconnected_workspaces_to_primary() {
    let display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state.publish_public_state();
    let mut second = Output::new(OutputId(1), "test-output-2", Size::new(2560, 1440));
    second.physical_size = Size::new(3840, 2160);
    second.native_scale = Scale120(180);
    second.transform = OutputTransform::Rotate90;

    state.connect_output(second).unwrap();
    assert!(state.publish_public_state().iter().any(|event| matches!(
        event.event,
        astera_ipc::wire::v1::Event::OutputOpened { .. }
    )));
    let runtime = &state.output_runtime[&OutputId(1)].wayland;
    assert_eq!(runtime.current_mode().unwrap().size, (3840, 2160).into());
    assert_eq!(runtime.current_scale().fractional_scale(), 1.5);
    assert_eq!(runtime.current_transform(), smithay::utils::Transform::_90);
    assert_eq!(
        state.output_runtime[&OutputId(1)].location,
        Point::new(1280, 0)
    );
    let disconnected_workspace = state.desktop.active_workspace_id(OutputId(0)).unwrap();
    state
        .desktop
        .apply(WorkspaceTransaction::SetName {
            workspace: disconnected_workspace,
            name: Some("main".into()),
        })
        .unwrap();
    state.disconnect_output(OutputId(0)).unwrap();
    assert!(state.publish_public_state().iter().any(|event| matches!(
        event.event,
        astera_ipc::wire::v1::Event::OutputClosed { .. }
    )));

    assert_eq!(state.active_output, OutputId(1));
    assert!(!state.output_runtime.contains_key(&OutputId(0)));
    assert_eq!(
        state
            .desktop
            .workspace_location(disconnected_workspace)
            .unwrap()
            .output,
        Some(OutputId(1))
    );
}

#[test]
fn output_reconfigure_preserves_camera_and_updates_protocol_state() {
    let display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let workspace = state.desktop.active_workspace_id(OutputId(0)).unwrap();
    state
        .desktop
        .workspace_mut(workspace)
        .unwrap()
        .camera
        .center = Point::new(740, -320);

    state
        .configure_output(
            OutputId(0),
            Size::new(3000, 2000),
            Size::new(2000, 1333),
            Scale120(180),
            OutputTransform::Rotate180,
        )
        .unwrap();

    let output = &state.desktop.outputs[&OutputId(0)];
    assert_eq!(output.output.physical_size, Size::new(3000, 2000));
    assert_eq!(output.output.logical_size, Size::new(2000, 1333));
    assert_eq!(
        state.desktop.workspace(workspace).unwrap().camera.center,
        Point::new(740, -320)
    );
    let runtime = &state.output_runtime[&OutputId(0)].wayland;
    assert_eq!(runtime.current_mode().unwrap().size, (3000, 2000).into());
    assert_eq!(runtime.current_scale().fractional_scale(), 1.5);
    assert_eq!(runtime.current_transform(), smithay::utils::Transform::_180);
}

#[test]
fn pointer_crosses_outputs_but_compositor_drag_stays_local() {
    let display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state
        .connect_output(Output::new(
            OutputId(1),
            "test-output-2",
            Size::new(1920, 1080),
        ))
        .unwrap();
    state.pointer_location = (1270.0, 200.0).into();

    let location = state.relative_pointer_location(20.0, 0.0);
    assert_eq!(state.active_output, OutputId(1));
    assert_eq!(location, (10.0, 200.0).into());

    state.active_output = OutputId(0);
    state.pointer_location = (1270.0, 200.0).into();
    state.drag = Some(DragState {
        window: WindowId(999),
        mode: WindowMode::Floating,
        kind: DragKind::Move,
        grab_offset: (0.0, 0.0),
        pointer_start: (0.0, 0.0),
        min_size: Size::new(1, 1),
        max_size: Size::new(i64::MAX, i64::MAX),
        target: astera_core::Rect::new(0, 0, 100, 100),
        start: astera_core::Rect::new(0, 0, 100, 100),
    });
    let location = state.relative_pointer_location(20.0, 0.0);
    assert_eq!(state.active_output, OutputId(0));
    assert_eq!(location, (1279.0, 200.0).into());
}

#[test]
fn fractional_output_converts_logical_origins_to_physical_pixels() {
    let physical = physical_point(Point::new(101, -25), 1.5);
    assert_eq!(physical, (152, -38).into());
}

#[test]
fn cursor_visual_honours_hotspot_visibility_and_output_ownership() {
    let display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state.pointer_location = (10.0, 20.0).into();
    state
        .named_cursors
        .get_mut(&(smithay::input::pointer::CursorIcon::Default, 120))
        .unwrap()
        .hotspot = (3, 5).into();

    let super::cursor::CursorRenderSource::Memory { location, .. } =
        state.cursor_render_source(OutputId(0)).unwrap()
    else {
        panic!("default cursor must use the compositor-owned image");
    };
    assert_eq!(location, (7.0, 15.0).into());
    assert!(state.cursor_render_source(OutputId(99)).is_none());

    state
        .desktop
        .outputs
        .get_mut(&OutputId(0))
        .unwrap()
        .output
        .native_scale = Scale120(240);
    let _ = state.cursor_render_source(OutputId(0)).unwrap();
    state
        .named_cursors
        .get_mut(&(smithay::input::pointer::CursorIcon::Default, 240))
        .unwrap()
        .hotspot = (6, 10).into();
    let super::cursor::CursorRenderSource::Memory { location, .. } =
        state.cursor_render_source(OutputId(0)).unwrap()
    else {
        panic!("scaled cursor must remain compositor-owned");
    };
    assert_eq!(location, (14.0, 30.0).into());

    state.cursor_image_status = smithay::input::pointer::CursorImageStatus::Hidden;
    assert!(state.cursor_render_source(OutputId(0)).is_none());
}

#[test]
fn touch_routing_is_stable_and_cancel_replaces_buggy_handle() {
    let display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state.register_output_alias("HDMI-A-1".into(), OutputId(0));
    state.bind_touch_device_output("touchscreen".into(), "HDMI-A-1");
    assert_eq!(
        state.touch_output_for_device("touchscreen"),
        Some(OutputId(0))
    );
    assert_ne!(state.allocate_touch_slot(), state.allocate_touch_slot());

    state
        .touch_slots
        .insert(("touchscreen".into(), 7), (OutputId(0), Some(42).into()));
    state.cancel_touch_sequences();
    assert!(state.touch_slots.is_empty());
    assert!(state.seat.get_touch().is_some());
}
