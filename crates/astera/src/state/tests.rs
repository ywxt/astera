use std::{
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
        wl_callback::WlCallback, wl_compositor::WlCompositor, wl_registry::WlRegistry,
        wl_seat::WlSeat, wl_surface::WlSurface,
    },
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::ExtIdleNotificationV1, ext_idle_notifier_v1::ExtIdleNotifierV1,
};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1, ext_session_lock_v1::ExtSessionLockV1,
};
use wayland_protocols::wp::{
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::WpFractionalScaleV1,
    },
    idle_inhibit::zv1::client::{
        zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
        zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
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
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::ZwlrLayerShellV1, zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
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
    let generation = state.locking_generation();
    state.lock_frame_presented(OutputId(0), generation);
    assert!(matches!(state.session_state, SessionState::Locked { .. }));
    client.join().unwrap();
    for _ in 0..8 {
        display.dispatch_clients(&mut state).unwrap();
    }
    assert!(state.session_is_locked());
    assert!(state.render_roots().is_empty());
}

#[test]
fn decoration_and_activation_globals_are_advertised() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
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
        let _idle_notification = idle.get_idle_notification(0, &seat, &queue, ());
        let surface = compositor.create_surface(&queue, ());
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
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn layer_exclusive_zone_reduces_usable_viewport() {
    use wayland_protocols_wlr::layer_shell::v1::client::{
        zwlr_layer_shell_v1::Layer, zwlr_layer_surface_v1::Anchor,
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
        surface.commit();

        let popup_surface = compositor.create_surface(&queue, ());
        let popup_xdg_surface = xdg.get_xdg_surface(&popup_surface, &queue, ());
        let positioner = xdg.create_positioner(&queue, ());
        positioner.set_size(200, 120);
        positioner.set_anchor_rect(0, 0, 1, 1);
        let _popup = popup_xdg_surface.get_popup(None, &positioner, &queue, ());
        popup_surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();
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
        grab_offset: (0.0, 0.0),
        target: Point::ORIGIN,
        start: Point::ORIGIN,
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
