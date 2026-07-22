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
    protocol::{wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_surface::WlSurface},
};
use wayland_protocols::wp::{
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::WpFractionalScaleV1,
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::XdgPopup, xdg_positioner::XdgPositioner, xdg_surface::XdgSurface,
    xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
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
delegate_noop!(TestClient: ignore XdgToplevel);
delegate_noop!(TestClient: ignore XdgPopup);
delegate_noop!(TestClient: ignore XdgPositioner);
delegate_noop!(TestClient: ignore WpViewporter);
delegate_noop!(TestClient: ignore WpViewport);
delegate_noop!(TestClient: ignore WpFractionalScaleManagerV1);
delegate_noop!(TestClient: ignore WpFractionalScaleV1);
delegate_noop!(TestClient: ignore ZwlrLayerShellV1);

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
    let mut second = Output::new(OutputId(1), "test-output-2", Size::new(2560, 1440));
    second.physical_size = Size::new(3840, 2160);
    second.native_scale = Scale120(180);
    second.transform = OutputTransform::Rotate90;

    state.connect_output(second).unwrap();
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
