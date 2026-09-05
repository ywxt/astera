use std::{
    io::{Read, Write},
    os::fd::AsFd,
    os::unix::net::{UnixListener, UnixStream},
    sync::Arc,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use astera_core::{Scale120, WorkspaceTransaction};
use smithay::reexports::wayland_server::Display;
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop,
    globals::registry_queue_init,
    protocol::{
        wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor,
        wl_data_device::WlDataDevice, wl_data_device_manager::WlDataDeviceManager,
        wl_data_offer::WlDataOffer, wl_data_source::WlDataSource, wl_keyboard::WlKeyboard,
        wl_output::WlOutput, wl_pointer::WlPointer, wl_registry::WlRegistry, wl_seat::WlSeat,
        wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface, wl_touch::WlTouch,
    },
};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::ExtDataControlDeviceV1,
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::ExtDataControlOfferV1,
    ext_data_control_source_v1::ExtDataControlSourceV1,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::ExtIdleNotificationV1, ext_idle_notifier_v1::ExtIdleNotifierV1,
};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1, ext_session_lock_v1::ExtSessionLockV1,
};
use wayland_protocols::ext::transient_seat::v1::client::{
    ext_transient_seat_manager_v1::ExtTransientSeatManagerV1,
    ext_transient_seat_v1::{self, ExtTransientSeatV1},
};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1, zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
};
use wayland_protocols::wp::{
    alpha_modifier::v1::client::{
        wp_alpha_modifier_surface_v1::WpAlphaModifierSurfaceV1,
        wp_alpha_modifier_v1::WpAlphaModifierV1,
    },
    color_representation::v1::client::{
        wp_color_representation_manager_v1::{self, WpColorRepresentationManagerV1},
        wp_color_representation_surface_v1::WpColorRepresentationSurfaceV1,
    },
    commit_timing::v1::client::{
        wp_commit_timer_v1::WpCommitTimerV1, wp_commit_timing_manager_v1::WpCommitTimingManagerV1,
    },
    content_type::v1::client::{
        wp_content_type_manager_v1::WpContentTypeManagerV1, wp_content_type_v1::WpContentTypeV1,
    },
    cursor_shape::v1::client::{
        wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
        wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
    },
    drm_lease::v1::client::wp_drm_lease_device_v1::WpDrmLeaseDeviceV1,
    fifo::v1::client::{wp_fifo_manager_v1::WpFifoManagerV1, wp_fifo_v1::WpFifoV1},
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::WpFractionalScaleV1,
    },
    idle_inhibit::zv1::client::{
        zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
        zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
    },
    input_timestamps::zv1::client::{
        zwp_input_timestamps_manager_v1::ZwpInputTimestampsManagerV1,
        zwp_input_timestamps_v1::{self, ZwpInputTimestampsV1},
    },
    keyboard_shortcuts_inhibit::zv1::client::{
        zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
        zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
    },
    linux_drm_syncobj::v1::client::wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1,
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
    pointer_warp::v1::client::wp_pointer_warp_v1::WpPointerWarpV1,
    presentation_time::client::{
        wp_presentation::WpPresentation, wp_presentation_feedback::WpPresentationFeedback,
    },
    primary_selection::zv1::client::{
        zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
        zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
        zwp_primary_selection_offer_v1::ZwpPrimarySelectionOfferV1,
        zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
    },
    relative_pointer::zv1::client::{
        zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
        zwp_relative_pointer_v1::ZwpRelativePointerV1,
    },
    security_context::v1::client::{
        wp_security_context_manager_v1::WpSecurityContextManagerV1,
        wp_security_context_v1::WpSecurityContextV1,
    },
    single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1,
    tablet::zv2::client::{
        zwp_tablet_manager_v2::ZwpTabletManagerV2, zwp_tablet_seat_v2::ZwpTabletSeatV2,
    },
    tearing_control::v1::client::{
        wp_tearing_control_manager_v1::WpTearingControlManagerV1,
        wp_tearing_control_v1::{self, WpTearingControlV1},
    },
    text_input::zv3::client::{
        zwp_text_input_manager_v3::ZwpTextInputManagerV3, zwp_text_input_v3::ZwpTextInputV3,
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};
use wayland_protocols::xdg::foreign::zv2::client::{
    zxdg_exported_v2::ZxdgExportedV2, zxdg_exporter_v2::ZxdgExporterV2,
    zxdg_imported_v2::ZxdgImportedV2, zxdg_importer_v2::ZxdgImporterV2,
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::XdgPopup, xdg_positioner::XdgPositioner, xdg_surface::XdgSurface,
    xdg_toplevel::XdgToplevel, xdg_wm_base::XdgWmBase,
};
use wayland_protocols::xdg::system_bell::v1::client::xdg_system_bell_v1::XdgSystemBellV1;
use wayland_protocols::xdg::toplevel_drag::v1::client::{
    xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1, xdg_toplevel_drag_v1::XdgToplevelDragV1,
};
use wayland_protocols::xdg::toplevel_icon::v1::client::{
    xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1, xdg_toplevel_icon_v1::XdgToplevelIconV1,
};
use wayland_protocols::xdg::toplevel_tag::v1::client::xdg_toplevel_tag_manager_v1::XdgToplevelTagManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1, zxdg_output_v1::ZxdgOutputV1,
};
use wayland_protocols::xdg::{
    activation::v1::client::{
        xdg_activation_token_v1::XdgActivationTokenV1, xdg_activation_v1::XdgActivationV1,
    },
    decoration::zv1::client::{
        zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
        zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
    },
    dialog::v1::client::{xdg_dialog_v1::XdgDialogV1, xdg_wm_dialog_v1::XdgWmDialogV1},
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2, zwp_input_method_v2::ZwpInputMethodV2,
    zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
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

impl Dispatch<WpColorRepresentationManagerV1, mpsc::Sender<(bool, bool, bool)>> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &WpColorRepresentationManagerV1,
        event: wp_color_representation_manager_v1::Event,
        data: &mpsc::Sender<(bool, bool, bool)>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            wp_color_representation_manager_v1::Event::SupportedAlphaMode { alpha_mode } => {
                let electrical = matches!(
                    alpha_mode,
                    wayland_client::WEnum::Value(
                        wayland_protocols::wp::color_representation::v1::client::wp_color_representation_surface_v1::AlphaMode::PremultipliedElectrical
                    )
                );
                data.send((electrical, false, false)).unwrap();
            }
            wp_color_representation_manager_v1::Event::SupportedCoefficientsAndRanges {
                ..
            } => data.send((false, true, false)).unwrap(),
            wp_color_representation_manager_v1::Event::Done => {
                data.send((false, false, true)).unwrap();
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtTransientSeatV1, mpsc::Sender<u32>> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &ExtTransientSeatV1,
        event: ext_transient_seat_v1::Event,
        data: &mpsc::Sender<u32>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let ext_transient_seat_v1::Event::Ready { global_name } = event {
            data.send(global_name).unwrap();
        }
    }
}

#[derive(Default)]
struct WorkspaceClientState {
    groups: Vec<ExtWorkspaceGroupHandleV1>,
    workspaces: Vec<ExtWorkspaceHandleV1>,
    names: HashMap<wayland_client::backend::ObjectId, String>,
    active: HashSet<wayland_client::backend::ObjectId>,
    memberships: Vec<(
        wayland_client::backend::ObjectId,
        wayland_client::backend::ObjectId,
    )>,
    done: usize,
}

thread_local! {
    static WORKSPACE_CLIENT_STATE: std::cell::RefCell<Option<Arc<std::sync::Mutex<WorkspaceClientState>>>> = const { std::cell::RefCell::new(None) };
}

fn workspace_client_state() -> Arc<std::sync::Mutex<WorkspaceClientState>> {
    WORKSPACE_CLIENT_STATE.with(|state| state.borrow().as_ref().unwrap().clone())
}

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

impl Dispatch<ExtWorkspaceManagerV1, ()> for TestClient {
    wayland_client::event_created_child!(TestClient, ExtWorkspaceManagerV1, [
        0 => (ExtWorkspaceGroupHandleV1, workspace_client_state()),
        1 => (ExtWorkspaceHandleV1, workspace_client_state())
    ]);

    fn event(
        _state: &mut Self,
        _proxy: &ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        let shared = workspace_client_state();
        let mut state = shared.lock().unwrap();
        match event {
            ext_workspace_manager_v1::Event::WorkspaceGroup { workspace_group } => {
                state.groups.push(workspace_group);
            }
            ext_workspace_manager_v1::Event::Workspace { workspace } => {
                state.workspaces.push(workspace);
            }
            ext_workspace_manager_v1::Event::Done => state.done += 1,
            _ => {}
        }
    }
}

impl Dispatch<ExtWorkspaceGroupHandleV1, Arc<std::sync::Mutex<WorkspaceClientState>>>
    for TestClient
{
    fn event(
        _state: &mut Self,
        proxy: &ExtWorkspaceGroupHandleV1,
        event: ext_workspace_group_handle_v1::Event,
        data: &Arc<std::sync::Mutex<WorkspaceClientState>>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let ext_workspace_group_handle_v1::Event::WorkspaceEnter { workspace } = event {
            data.lock()
                .unwrap()
                .memberships
                .push((proxy.id(), workspace.id()));
        }
    }
}

impl Dispatch<ExtWorkspaceHandleV1, Arc<std::sync::Mutex<WorkspaceClientState>>> for TestClient {
    fn event(
        _state: &mut Self,
        proxy: &ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        data: &Arc<std::sync::Mutex<WorkspaceClientState>>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        let mut state = data.lock().unwrap();
        match event {
            ext_workspace_handle_v1::Event::Name { name } => {
                state.names.insert(proxy.id(), name);
            }
            ext_workspace_handle_v1::Event::State {
                state: workspace_state,
            } => {
                if matches!(
                    workspace_state,
                    wayland_client::WEnum::Value(state)
                        if state.contains(ext_workspace_handle_v1::State::Active)
                ) {
                    state.active.insert(proxy.id());
                } else {
                    state.active.remove(&proxy.id());
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(TestClient: ignore WlCompositor);
delegate_noop!(TestClient: ignore WlSurface);
delegate_noop!(TestClient: ignore WlCallback);
delegate_noop!(TestClient: ignore WlSeat);
delegate_noop!(TestClient: ignore ExtTransientSeatManagerV1);
delegate_noop!(TestClient: ignore WpColorRepresentationSurfaceV1);
delegate_noop!(TestClient: ignore WpLinuxDrmSyncobjManagerV1);
delegate_noop!(TestClient: ignore WpDrmLeaseDeviceV1);
delegate_noop!(TestClient: ignore WlOutput);
delegate_noop!(TestClient: ignore WlPointer);
delegate_noop!(TestClient: ignore WlKeyboard);
delegate_noop!(TestClient: ignore WlTouch);
delegate_noop!(TestClient: ignore ZwpInputTimestampsManagerV1);
delegate_noop!(TestClient: ignore WlShm);
delegate_noop!(TestClient: ignore WlShmPool);
delegate_noop!(TestClient: ignore WlBuffer);
delegate_noop!(TestClient: ignore WlDataDeviceManager);
delegate_noop!(TestClient: ignore WlDataOffer);
delegate_noop!(TestClient: ignore WlDataSource);
delegate_noop!(TestClient: ignore XdgToplevel);
delegate_noop!(TestClient: ignore XdgPopup);
delegate_noop!(TestClient: ignore XdgPositioner);
delegate_noop!(TestClient: ignore ZxdgOutputManagerV1);
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
delegate_noop!(TestClient: ignore ZwpInputMethodKeyboardGrabV2);
delegate_noop!(TestClient: ignore ZwpInputPopupSurfaceV2);
delegate_noop!(TestClient: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(TestClient: ignore ZwpVirtualKeyboardV1);
delegate_noop!(TestClient: ignore ZwpLinuxDmabufV1);
delegate_noop!(TestClient: ignore ZwpPrimarySelectionDeviceManagerV1);
delegate_noop!(TestClient: ignore ZwpPrimarySelectionDeviceV1);
delegate_noop!(TestClient: ignore ZwpPrimarySelectionOfferV1);
delegate_noop!(TestClient: ignore ZwpPrimarySelectionSourceV1);
delegate_noop!(TestClient: ignore WpSinglePixelBufferManagerV1);
delegate_noop!(TestClient: ignore WpAlphaModifierV1);
delegate_noop!(TestClient: ignore WpAlphaModifierSurfaceV1);
delegate_noop!(TestClient: ignore WpContentTypeManagerV1);
delegate_noop!(TestClient: ignore WpContentTypeV1);
delegate_noop!(TestClient: ignore XdgWmDialogV1);
delegate_noop!(TestClient: ignore XdgDialogV1);
delegate_noop!(TestClient: ignore WpFifoManagerV1);
delegate_noop!(TestClient: ignore WpFifoV1);
delegate_noop!(TestClient: ignore WpCommitTimingManagerV1);
delegate_noop!(TestClient: ignore WpCommitTimerV1);
delegate_noop!(TestClient: ignore WpSecurityContextManagerV1);
delegate_noop!(TestClient: ignore WpSecurityContextV1);
delegate_noop!(TestClient: ignore WpPresentation);
delegate_noop!(TestClient: ignore ExtDataControlManagerV1);
delegate_noop!(TestClient: ignore ExtDataControlDeviceV1);
delegate_noop!(TestClient: ignore ExtDataControlOfferV1);
delegate_noop!(TestClient: ignore ExtDataControlSourceV1);
delegate_noop!(TestClient: ignore ZxdgExporterV2);
delegate_noop!(TestClient: ignore ZxdgImporterV2);
delegate_noop!(TestClient: ignore XdgSystemBellV1);
delegate_noop!(TestClient: ignore XdgToplevelTagManagerV1);
delegate_noop!(TestClient: ignore XdgToplevelIconManagerV1);
delegate_noop!(TestClient: ignore XdgToplevelIconV1);
delegate_noop!(TestClient: ignore WpTearingControlManagerV1);
delegate_noop!(TestClient: ignore WpTearingControlV1);
delegate_noop!(TestClient: ignore WpPointerWarpV1);
delegate_noop!(TestClient: ignore XdgToplevelDragManagerV1);
delegate_noop!(TestClient: ignore XdgToplevelDragV1);
delegate_noop!(TestClient: ignore ExtForeignToplevelHandleV1);

impl Dispatch<ZwpInputTimestampsV1, mpsc::Sender<(u32, u32, u32)>> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpInputTimestampsV1,
        event: zwp_input_timestamps_v1::Event,
        timestamps: &mpsc::Sender<(u32, u32, u32)>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let zwp_input_timestamps_v1::Event::Timestamp {
            tv_sec_hi,
            tv_sec_lo,
            tv_nsec,
        } = event
        {
            timestamps.send((tv_sec_hi, tv_sec_lo, tv_nsec)).unwrap();
        }
    }
}

impl Dispatch<WlPointer, mpsc::Sender<u32>> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &WlPointer,
        event: wayland_client::protocol::wl_pointer::Event,
        serials: &mpsc::Sender<u32>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_pointer::Event::Enter { serial, .. } = event {
            serials.send(serial).unwrap();
        }
    }
}

struct PointerButtonSerial(mpsc::Sender<u32>);

impl Dispatch<WlPointer, PointerButtonSerial> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &WlPointer,
        event: wayland_client::protocol::wl_pointer::Event,
        serial: &PointerButtonSerial,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_pointer::Event::Button {
            serial: value,
            state:
                wayland_client::WEnum::Value(wayland_client::protocol::wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            serial.0.send(value).unwrap();
        }
    }
}

impl Dispatch<ZxdgExportedV2, mpsc::Sender<String>> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &ZxdgExportedV2,
        event: wayland_protocols::xdg::foreign::zv2::client::zxdg_exported_v2::Event,
        handle: &mpsc::Sender<String>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::foreign::zv2::client::zxdg_exported_v2::Event::Handle {
            handle: value,
        } = event
        {
            let _ = handle.send(value);
        }
    }
}

impl Dispatch<ZxdgImportedV2, mpsc::Sender<()>> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &ZxdgImportedV2,
        event: wayland_protocols::xdg::foreign::zv2::client::zxdg_imported_v2::Event,
        destroyed: &mpsc::Sender<()>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(
            event,
            wayland_protocols::xdg::foreign::zv2::client::zxdg_imported_v2::Event::Destroyed
        ) {
            let _ = destroyed.send(());
        }
    }
}

impl Dispatch<WpPresentationFeedback, mpsc::Sender<bool>> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &WpPresentationFeedback,
        event: wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::Event,
        presented: &mpsc::Sender<bool>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::Event;
        match event {
            Event::Presented { .. } => {
                let _ = presented.send(true);
            }
            Event::Discarded => {
                let _ = presented.send(false);
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for TestClient {
    wayland_client::event_created_child!(TestClient, ExtForeignToplevelListV1, [
        0 => (ExtForeignToplevelHandleV1, ())
    ]);

    fn event(
        _state: &mut Self,
        _proxy: &ExtForeignToplevelListV1,
        _event: wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_list_v1::Event,
        _data: &(),
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZxdgOutputV1, mpsc::Sender<(i32, i32)>> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &ZxdgOutputV1,
        event: wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::Event,
        sizes: &mpsc::Sender<(i32, i32)>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::Event::LogicalSize {
            width,
            height,
        } = event
        {
            let _ = sizes.send((width, height));
        }
    }
}

impl
    Dispatch<
        ZwpLinuxDmabufFeedbackV1,
        (
            mpsc::Sender<()>,
            mpsc::Sender<Vec<u8>>,
            mpsc::Sender<Vec<u8>>,
        ),
    > for TestClient
{
    fn event(
        _state: &mut Self,
        _proxy: &ZwpLinuxDmabufFeedbackV1,
        event: wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::Event,
        feedback: &(
            mpsc::Sender<()>,
            mpsc::Sender<Vec<u8>>,
            mpsc::Sender<Vec<u8>>,
        ),
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
            Event::MainDevice { device } => {
                let _ = feedback.2.send(device);
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

impl Dispatch<ZwlrLayerSurfaceV1, Arc<std::sync::atomic::AtomicBool>> for TestClient {
    fn event(
        _state: &mut Self,
        proxy: &ZwlrLayerSurfaceV1,
        event: wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event,
        closed: &Arc<std::sync::atomic::AtomicBool>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        match event {
            wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event::Configure {
                serial,
                ..
            } => proxy.ack_configure(serial),
            wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Event::Closed => {
                closed.store(true, std::sync::atomic::Ordering::Release);
            }
            _ => {}
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

impl Dispatch<XdgPopup, mpsc::Sender<()>> for TestClient {
    fn event(
        _state: &mut Self,
        _proxy: &XdgPopup,
        event: wayland_protocols::xdg::shell::client::xdg_popup::Event,
        dismissed: &mpsc::Sender<()>,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
    ) {
        if matches!(
            event,
            wayland_protocols::xdg::shell::client::xdg_popup::Event::PopupDone
        ) {
            let _ = dismissed.send(());
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

fn attach_one_pixel_buffer(
    shm: &WlShm,
    surface: &WlSurface,
    queue: &QueueHandle<TestClient>,
    name: &str,
) {
    let fd = rustix::fs::memfd_create(name, rustix::fs::MemfdFlags::CLOEXEC).unwrap();
    rustix::fs::ftruncate(&fd, 4).unwrap();
    let pool = shm.create_pool(fd.as_fd(), 4, queue, ());
    let buffer = pool.create_buffer(
        0,
        1,
        1,
        4,
        wayland_client::protocol::wl_shm::Format::Argb8888,
        queue,
        (),
    );
    surface.attach(Some(&buffer), 0, 0);
}

#[test]
fn input_timestamps_report_nanoseconds_and_drop_destroyed_subscriptions() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    let server_client = display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (timestamp_tx, timestamp_rx) = mpsc::channel();
    let (receive_tx, receive_rx) = mpsc::sync_channel(0);
    let (destroy_tx, destroy_rx) = mpsc::sync_channel(0);
    let (destroyed_tx, destroyed_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<ZwpInputTimestampsManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let pointer = seat.get_pointer(&queue, ());
        let timestamps = manager.get_pointer_timestamps(&pointer, &queue, timestamp_tx);
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        receive_rx.recv().unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        destroy_rx.recv().unwrap();
        timestamps.destroy();
        connection.flush().unwrap();
        destroyed_tx.send(()).unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.input_timestamp_subscriptions.len() == 1
    });
    state.send_input_timestamp(
        input_timestamps::InputTimestampKind::Pointer,
        Some(&server_client.id()),
        1_234_567,
    );
    display.flush_clients().unwrap();
    receive_tx.send(()).unwrap();
    let mut timestamp = None;
    dispatch_until(&mut display, &mut state, |_| {
        timestamp = timestamp_rx.try_recv().ok();
        timestamp.is_some()
    });
    assert_eq!(timestamp.unwrap(), (0, 1, 234_567_000));

    destroy_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        destroyed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        state.input_timestamp_subscriptions.is_empty()
    });
    client.join().unwrap();
}

#[test]
fn input_timestamps_do_not_leak_to_transient_seat_resources() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    let server_client = display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::trusted_input()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (receive_tx, receive_rx) = mpsc::sync_channel(0);
    let (result_tx, result_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let main_seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let transient_manager = globals
            .bind::<ExtTransientSeatManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let timestamp_manager = globals
            .bind::<ZwpInputTimestampsManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let (name_tx, name_rx) = mpsc::channel();
        let _transient = transient_manager.create(&queue, name_tx);
        connection.flush().unwrap();
        events.blocking_dispatch(&mut TestClient).unwrap();
        let global_name = name_rx.recv().unwrap();
        let transient_seat: WlSeat = globals.registry().bind(global_name, 9, &queue, ());
        let main_pointer = main_seat.get_pointer(&queue, ());
        let transient_pointer = transient_seat.get_pointer(&queue, ());
        let (main_tx, main_rx) = mpsc::channel();
        let (transient_tx, transient_rx) = mpsc::channel();
        let _main_timestamps =
            timestamp_manager.get_pointer_timestamps(&main_pointer, &queue, main_tx);
        let _transient_timestamps =
            timestamp_manager.get_pointer_timestamps(&transient_pointer, &queue, transient_tx);
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();

        receive_rx.recv().unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        result_tx
            .send((main_rx.try_recv().is_ok(), transient_rx.try_recv().is_ok()))
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.input_timestamp_subscriptions.len() == 2
    });
    state.send_input_timestamp(
        input_timestamps::InputTimestampKind::Pointer,
        Some(&server_client.id()),
        2_500_125,
    );
    display.flush_clients().unwrap();
    receive_tx.send(()).unwrap();
    let mut result = None;
    dispatch_until(&mut display, &mut state, |_| {
        result = result_rx.try_recv().ok();
        result.is_some()
    });
    assert_eq!(result, Some((true, false)));
    client.join().unwrap();
}

#[test]
fn keyboard_timestamps_follow_input_method_grab_delivery() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (app_server, app_client) = UnixStream::pair().unwrap();
    let (ime_server, ime_client) = UnixStream::pair().unwrap();
    let app = display
        .handle()
        .insert_client(app_server, Arc::new(ClientState::default()))
        .unwrap();
    display
        .handle()
        .insert_client(ime_server, Arc::new(ClientState::trusted_input()))
        .unwrap();

    let (app_ready_tx, app_ready_rx) = mpsc::sync_channel(0);
    let (check_grabbed_tx, check_grabbed_rx) = mpsc::sync_channel(0);
    let (grabbed_result_tx, grabbed_result_rx) = mpsc::sync_channel(0);
    let (check_released_tx, check_released_rx) = mpsc::sync_channel(0);
    let (released_result_tx, released_result_rx) = mpsc::sync_channel(0);
    let app_thread = thread::spawn(move || {
        let connection = Connection::from_socket(app_client).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<ZwpInputTimestampsManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let keyboard = seat.get_keyboard(&queue, ());
        let (timestamp_tx, timestamp_rx) = mpsc::channel();
        let _timestamps = manager.get_keyboard_timestamps(&keyboard, &queue, timestamp_tx);
        connection.flush().unwrap();
        app_ready_tx.send(()).unwrap();

        check_grabbed_rx.recv().unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        grabbed_result_tx
            .send(timestamp_rx.try_recv().is_ok())
            .unwrap();
        check_released_rx.recv().unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        released_result_tx
            .send(timestamp_rx.try_recv().is_ok())
            .unwrap();
    });

    let (ime_ready_tx, ime_ready_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let (released_tx, released_rx) = mpsc::sync_channel(0);
    let ime_thread = thread::spawn(move || {
        let connection = Connection::from_socket(ime_client).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<ZwpInputMethodManagerV2, _, _>(&queue, 1..=1, ())
            .unwrap();
        let input_method = manager.get_input_method(&seat, &queue, ());
        let grab = input_method.grab_keyboard(&queue, ());
        connection.flush().unwrap();
        ime_ready_tx.send(()).unwrap();

        release_rx.recv().unwrap();
        grab.release();
        connection.flush().unwrap();
        released_tx.send(()).unwrap();
        events.dispatch_pending(&mut TestClient).unwrap();
    });

    let mut app_ready = false;
    let mut ime_ready = false;
    dispatch_until(&mut display, &mut state, |_| {
        app_ready |= app_ready_rx.try_recv().is_ok();
        ime_ready |= ime_ready_rx.try_recv().is_ok();
        app_ready && ime_ready
    });
    dispatch_until(&mut display, &mut state, |state| {
        state.input_timestamp_subscriptions.len() == 1
            && state.seat.input_method().keyboard_grabbed()
    });
    state.forward_keyboard_event(Some(&app.id()), 4_000_125);
    display.flush_clients().unwrap();
    check_grabbed_tx.send(()).unwrap();
    let mut grabbed_received = None;
    dispatch_until(&mut display, &mut state, |_| {
        grabbed_received = grabbed_result_rx.try_recv().ok();
        grabbed_received.is_some()
    });
    assert_eq!(grabbed_received, Some(false));

    release_tx.send(()).unwrap();
    let mut client_released = false;
    dispatch_until(&mut display, &mut state, |state| {
        client_released |= released_rx.try_recv().is_ok();
        client_released && !state.seat.input_method().keyboard_grabbed()
    });
    state.forward_keyboard_event(Some(&app.id()), 4_000_250);
    display.flush_clients().unwrap();
    check_released_tx.send(()).unwrap();
    let mut released_received = None;
    dispatch_until(&mut display, &mut state, |_| {
        released_received = released_result_rx.try_recv().ok();
        released_received.is_some()
    });
    assert_eq!(released_received, Some(true));
    app_thread.join().unwrap();
    ime_thread.join().unwrap();
}

#[test]
fn touch_timestamps_follow_data_device_grab_delivery() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    let server_client = display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (receive_tx, receive_rx) = mpsc::sync_channel(0);
    let (result_tx, result_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<ZwpInputTimestampsManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let touch = seat.get_touch(&queue, ());
        let (timestamp_tx, timestamp_rx) = mpsc::channel();
        let _timestamps = manager.get_touch_timestamps(&touch, &queue, timestamp_tx);
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();

        receive_rx.recv().unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        result_tx
            .send((timestamp_rx.try_recv().ok(), timestamp_rx.try_recv().ok()))
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.input_timestamp_subscriptions.len() == 1
    });
    state.pending_client_dnd_input = Some(ClientDndInput::Touch);
    let seat = state.seat.clone();
    ClientDndGrabHandler::started(&mut state, None, None, seat.clone());
    state.send_input_timestamp(
        input_timestamps::InputTimestampKind::Touch,
        Some(&server_client.id()),
        5_000_125,
    );
    ClientDndGrabHandler::dropped(&mut state, None, false, seat);
    state.send_input_timestamp(
        input_timestamps::InputTimestampKind::Touch,
        Some(&server_client.id()),
        5_000_250,
    );
    display.flush_clients().unwrap();
    receive_tx.send(()).unwrap();

    let mut result = None;
    dispatch_until(&mut display, &mut state, |_| {
        result = result_rx.try_recv().ok();
        result.is_some()
    });
    assert_eq!(result, Some((Some((0, 5, 250_000)), None)));
    assert_eq!(state.active_client_dnd_input, None);
    client.join().unwrap();
}

#[test]
fn security_context_accepts_clients_and_hides_privileged_managers() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    let creator = display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let creator_id = creator.id();
    let socket_path = std::env::temp_dir().join(format!(
        "astera-security-context-{}-{:?}.sock",
        std::process::id(),
        thread::current().id()
    ));
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).unwrap();
    let (close_read, close_write) = rustix::pipe::pipe().unwrap();
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let creator_thread = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, event_queue) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = event_queue.handle();
        let manager = globals
            .bind::<WpSecurityContextManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let data_control = globals
            .bind::<ExtDataControlManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let source = data_control.create_data_source(&queue, ());
        source.offer("text/plain;charset=utf-8".into());
        let primary_source = data_control.create_data_source(&queue, ());
        primary_source.offer("text/plain".into());
        let device = data_control.get_data_device(&seat, &queue, ());
        device.set_selection(Some(&source));
        device.set_primary_selection(Some(&primary_source));
        let context = manager.create_listener(listener.as_fd(), close_read.as_fd(), &queue, ());
        context.set_sandbox_engine("bubblewrap".into());
        context.set_app_id("org.example.App".into());
        context.set_instance_id("instance-7".into());
        context.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();
        done_rx.recv().unwrap();
        drop(close_write);
    });

    dispatch_until(&mut display, &mut state, |_| {
        committed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        !state.pending_security_contexts.is_empty()
    });
    let (source, context) = state.take_pending_security_contexts().pop().unwrap();
    assert_eq!(context.sandbox_engine.as_deref(), Some("bubblewrap"));
    assert_eq!(context.app_id.as_deref(), Some("org.example.App"));
    assert_eq!(context.instance_id.as_deref(), Some("instance-7"));
    assert_eq!(context.creator_client_id, creator_id);

    let mut accept_loop =
        smithay::reexports::calloop::EventLoop::<Vec<UnixStream>>::try_new().unwrap();
    accept_loop
        .handle()
        .insert_source(source, |stream, _, accepted| accepted.push(stream))
        .unwrap();
    let sandbox_socket = UnixStream::connect(&socket_path).unwrap();
    let mut accepted = Vec::new();
    accept_loop
        .dispatch(Some(Duration::from_millis(100)), &mut accepted)
        .unwrap();
    let sandbox_server_socket = accepted.pop().unwrap();
    let sandbox_state = Arc::new(ClientState::sandboxed(context));
    assert_eq!(
        sandbox_state.security_context().unwrap().app_id.as_deref(),
        Some("org.example.App")
    );
    display
        .handle()
        .insert_client(sandbox_server_socket, sandbox_state)
        .unwrap();

    let (visibility_tx, visibility_rx) = mpsc::sync_channel(0);
    let sandbox = thread::spawn(move || {
        let connection = Connection::from_socket(sandbox_socket).unwrap();
        let (globals, event_queue) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = event_queue.handle();
        visibility_tx
            .send((
                globals
                    .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
                    .is_ok(),
                globals
                    .bind::<WpSecurityContextManagerV1, _, _>(&queue, 1..=1, ())
                    .is_err(),
                globals
                    .bind::<ExtDataControlManagerV1, _, _>(&queue, 1..=1, ())
                    .is_err(),
            ))
            .unwrap();
    });
    dispatch_until(&mut display, &mut state, |_| {
        visibility_rx
            .try_recv()
            .is_ok_and(|visible| visible == (true, true, true))
    });

    sandbox.join().unwrap();
    done_tx.send(()).unwrap();
    creator_thread.join().unwrap();
    std::fs::remove_file(socket_path).unwrap();
}

#[test]
fn presentation_feedback_is_committed_and_completed_once() {
    use smithay::{
        desktop::utils::SurfacePresentationFeedback,
        wayland::{compositor::with_states, presentation::Refresh},
    };
    use wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;

    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let (presented_tx, presented_rx) = mpsc::channel();
    let (complete_tx, complete_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let presentation = globals
            .bind::<WpPresentation, _, _>(&queue, 1..=2, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        let _feedback = presentation.feedback(&surface, &queue, presented_tx);
        surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();
        complete_rx.recv().unwrap();
        events.blocking_dispatch(&mut TestClient).unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        committed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    let surface = state.windows[0].surface.wl_surface().clone();
    let mut feedback = with_states(&surface, |states| {
        SurfacePresentationFeedback::from_states(states, wp_presentation_feedback::Kind::ZeroCopy)
            .unwrap()
    });
    crate::backend::render::complete_presentation_feedback(
        std::slice::from_mut(&mut feedback),
        &state.protocol_output(OutputId(0)).unwrap(),
        crate::backend::render::monotonic_time(),
        Refresh::fixed(Duration::from_millis(16)),
        42,
        wp_presentation_feedback::Kind::Vsync,
    );
    display.flush_clients().unwrap();
    complete_tx.send(()).unwrap();
    assert!(presented_rx.recv_timeout(Duration::from_secs(1)).unwrap());
    client.join().unwrap();
}

#[test]
fn transient_seat_advertises_bindable_global_and_removes_it_on_destroy() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::trusted_input()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (destroy_tx, destroy_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let manager = globals
            .bind::<ExtTransientSeatManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let (name_tx, name_rx) = mpsc::channel();
        let transient = manager.create(&queue, name_tx);
        connection.flush().unwrap();
        events.blocking_dispatch(&mut TestClient).unwrap();
        let global_name = name_rx.recv().unwrap();
        let seat: WlSeat = globals.registry().bind(global_name, 9, &queue, ());
        connection.flush().unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        ready_tx.send(()).unwrap();
        destroy_rx.recv().unwrap();
        transient.destroy();
        seat.release();
        connection.flush().unwrap();
    });

    dispatch_until(&mut display, &mut state, |state| {
        ready_rx.try_recv().is_ok() && state.transient_seats.len() == 1
    });
    assert_eq!(state.transient_seats.len(), 1);
    destroy_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |state| {
        state.transient_seats.is_empty()
    });
    client.join().unwrap();
}

#[test]
fn color_representation_advertises_only_supported_alpha_and_commits_atomically() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (created_tx, created_rx) = mpsc::sync_channel(0);
    let (commit_tx, commit_rx) = mpsc::sync_channel(0);
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let (destroy_tx, destroy_rx) = mpsc::sync_channel(0);
    let (destroyed_tx, destroyed_rx) = mpsc::sync_channel(0);
    let (final_commit_tx, final_commit_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let (capability_tx, capability_rx) = mpsc::channel();
        let manager = globals
            .bind::<WpColorRepresentationManagerV1, _, _>(&queue, 1..=1, capability_tx)
            .unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        let capabilities = capability_rx.try_iter().collect::<Vec<_>>();
        assert_eq!(
            capabilities,
            vec![(true, false, false), (false, false, true)]
        );

        let surface = compositor.create_surface(&queue, ());
        let representation = manager.get_surface(&surface, &queue, ());
        representation.set_alpha_mode(
            wayland_protocols::wp::color_representation::v1::client::wp_color_representation_surface_v1::AlphaMode::PremultipliedElectrical,
        );
        connection.flush().unwrap();
        created_tx.send(()).unwrap();

        commit_rx.recv().unwrap();
        surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();

        destroy_rx.recv().unwrap();
        representation.destroy();
        connection.flush().unwrap();
        destroyed_tx.send(()).unwrap();

        final_commit_rx.recv().unwrap();
        surface.commit();
        connection.flush().unwrap();
    });

    let mut created = false;
    dispatch_until(&mut display, &mut state, |state| {
        created |= created_rx.try_recv().is_ok();
        created && !state.pending_color_alpha.is_empty()
    });
    let surface = state.color_representations.keys().next().unwrap().clone();
    assert!(!state.electrical_alpha_surfaces.contains(&surface));

    commit_tx.send(()).unwrap();
    let mut committed = false;
    dispatch_until(&mut display, &mut state, |state| {
        committed |= committed_rx.try_recv().is_ok();
        committed && state.electrical_alpha_surfaces.contains(&surface)
    });

    destroy_tx.send(()).unwrap();
    let mut destroyed = false;
    dispatch_until(&mut display, &mut state, |state| {
        destroyed |= destroyed_rx.try_recv().is_ok();
        destroyed && state.pending_color_alpha.contains_key(&surface)
    });
    assert!(state.electrical_alpha_surfaces.contains(&surface));

    final_commit_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |state| {
        !state.electrical_alpha_surfaces.contains(&surface)
    });
    client.join().unwrap();
}

#[test]
fn output_power_allows_multiple_controls_and_coalesces_backend_requests() {
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
        let second_control = manager.get_output_power(&output, &queue, ());
        control.set_mode(zwlr_output_power_v1::Mode::Off);
        second_control.set_mode(zwlr_output_power_v1::Mode::Off);
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
    assert_eq!(state.output_power_controls[&OutputId(0)].len(), 2);
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
fn late_output_power_confirmation_does_not_resurrect_removed_output_state() {
    let display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state
        .connect_output(Output::new(
            OutputId(1),
            "power-fallback-output",
            Size::new(1024, 768),
        ))
        .unwrap();
    state.disconnect_output(OutputId(0)).unwrap();
    assert!(!state.output_power_modes.contains_key(&OutputId(0)));

    // Model a KMS completion queued before hot-unplug but delivered afterwards.
    state.confirm_output_power(OutputId(0), false);

    assert!(!state.output_power_modes.contains_key(&OutputId(0)));
    assert!(state.output_power_modes[&OutputId(1)]);
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
                globals
                    .bind::<ExtTransientSeatManagerV1, _, _>(&handle, 1..=1, ())
                    .is_err(),
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
    assert_eq!(result, Some((true, true, true, true)));
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
    let (initial_tx, initial_rx) = mpsc::sync_channel(0);
    let (rebase_tx, rebase_rx) = mpsc::sync_channel(0);
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
        let (main_tx, main_rx) = mpsc::channel();
        let surface = compositor.create_surface(&queue, ());
        let _feedback =
            dmabuf.get_surface_feedback(&surface, &queue, (done_tx, target_tx, main_tx));
        connection.flush().unwrap();
        let connected = events.roundtrip(&mut TestClient).is_ok();
        let target_device = 2u64.to_ne_bytes();
        let initial_ok = connected
            && done_rx.try_recv().is_ok()
            && target_rx.try_iter().any(|device| device == target_device)
            && main_rx
                .try_iter()
                .any(|device| device == 1u64.to_ne_bytes());
        initial_tx.send(initial_ok).unwrap();
        rebase_rx.recv().unwrap();
        let connected = events.roundtrip(&mut TestClient).is_ok();
        result_tx
            .send(
                connected
                    && done_rx.try_recv().is_ok()
                    && main_rx.try_iter().any(|device| device == target_device),
            )
            .unwrap();
    });
    let mut initial = None;
    dispatch_until(&mut display, &mut state, |_| match initial_rx.try_recv() {
        Ok(value) => {
            initial = Some(value);
            true
        }
        Err(_) => false,
    });
    assert_eq!(initial, Some(true));
    state.unregister_dmabuf_device(1);
    rebase_tx.send(()).unwrap();
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
fn native_drm_protocols_are_not_advertised_without_a_capable_device() {
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
                    .bind::<WpLinuxDrmSyncobjManagerV1, _, _>(&handle, 1..=1, ())
                    .is_err(),
                globals
                    .bind::<WpDrmLeaseDeviceV1, _, _>(&handle, 1..=1, ())
                    .is_err(),
            ))
            .unwrap();
    });
    let mut hidden = (false, false);
    dispatch_until(&mut display, &mut state, |_| match result_rx.try_recv() {
        Ok(value) => {
            hidden = value;
            true
        }
        Err(_) => false,
    });
    assert_eq!(hidden, (true, true));
    assert!(state.pending_drm_syncobj_sources.is_empty());
    client.join().unwrap();
}

#[test]
fn removing_the_last_dmabuf_device_disables_stale_global_and_allows_recreation() {
    let display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let format = smithay::backend::allocator::Format {
        code: smithay::backend::allocator::Fourcc::Argb8888,
        modifier: smithay::backend::allocator::Modifier::Linear,
    };
    state.enable_dmabuf(Some(1), [format]);
    assert!(state.dmabuf_enabled);
    assert_eq!(state.dmabuf_default_device, Some(1));
    assert!(state.dmabuf_global.is_some());

    state.unregister_dmabuf_device(1);
    assert!(!state.dmabuf_enabled);
    assert_eq!(state.dmabuf_default_device, None);
    assert!(state.dmabuf_global.is_none());

    state.enable_dmabuf(Some(2), [format]);
    assert!(state.dmabuf_enabled);
    assert_eq!(state.dmabuf_default_device, Some(2));
    assert!(state.dmabuf_global.is_some());
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
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<ZwpInputMethodManagerV2, _, _>(&queue, 1..=1, ())
            .unwrap();
        let _first = manager.get_input_method(&seat, &queue, ());
        let (unavailable_tx, unavailable_rx) = mpsc::channel();
        let second = manager.get_input_method(&seat, &queue, unavailable_tx);
        connection.flush().unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        let surface = compositor.create_surface(&queue, ());
        let _popup = second.get_input_popup_surface(&surface, &queue, ());
        let _grab = second.grab_keyboard(&queue, ());
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
fn session_lock_revokes_an_input_method_before_its_first_request() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::trusted_input()))
        .unwrap();
    let (created_tx, created_rx) = mpsc::sync_channel(0);
    let (revoked_tx, revoked_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<ZwpInputMethodManagerV2, _, _>(&queue, 1..=1, ())
            .unwrap();
        // Do not issue any request on this object: the lock transition must still know which
        // privileged client owns it and require a fresh connection after unlock.
        let _input_method = manager.get_input_method(&seat, &queue, ());
        connection.flush().unwrap();
        created_tx.send(()).unwrap();
        let disconnected = events.roundtrip(&mut TestClient).is_err();
        revoked_tx.send(disconnected).unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| created_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.input_method_client.is_some()
    });
    state.secure_input_for_lock();
    display.flush_clients().unwrap();
    let mut revoked = false;
    dispatch_until(&mut display, &mut state, |_| match revoked_rx.try_recv() {
        Ok(value) => {
            revoked = value;
            true
        }
        Err(_) => false,
    });
    assert!(revoked);
    assert!(state.input_method_client.is_none());
    client.join().unwrap();
}

#[test]
fn session_lock_revokes_a_silent_virtual_keyboard() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::trusted_input()))
        .unwrap();
    let (created_tx, created_rx) = mpsc::sync_channel(0);
    let (check_tx, check_rx) = mpsc::sync_channel(0);
    let (revoked_tx, revoked_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<ZwpVirtualKeyboardManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        // No keymap or key request follows. The lock transition must revoke the object at creation
        // time instead of waiting for activity that might arrive only after unlock.
        let _keyboard = manager.create_virtual_keyboard(&seat, &queue, ());
        connection.flush().unwrap();
        created_tx.send(()).unwrap();
        check_rx.recv().unwrap();
        revoked_tx
            .send(events.roundtrip(&mut TestClient).is_err())
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| created_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        !state.virtual_keyboard_clients.is_empty()
    });
    state.secure_input_for_lock();
    display.flush_clients().unwrap();
    check_tx.send(()).unwrap();
    let mut revoked = false;
    dispatch_until(&mut display, &mut state, |_| match revoked_rx.try_recv() {
        Ok(value) => {
            revoked = value;
            true
        }
        Err(_) => false,
    });
    assert!(revoked);
    assert!(state.virtual_keyboard_clients.is_empty());
    client.join().unwrap();
}

#[test]
fn virtual_keyboard_tracking_ends_after_last_instance_is_destroyed() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::trusted_input()))
        .unwrap();
    let (created_tx, created_rx) = mpsc::sync_channel(0);
    let (destroy_first_tx, destroy_first_rx) = mpsc::sync_channel(0);
    let (first_destroyed_tx, first_destroyed_rx) = mpsc::sync_channel(0);
    let (destroy_second_tx, destroy_second_rx) = mpsc::sync_channel(0);
    let (second_destroyed_tx, second_destroyed_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, _events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = _events.handle();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<ZwpVirtualKeyboardManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let first = manager.create_virtual_keyboard(&seat, &queue, ());
        let second = manager.create_virtual_keyboard(&seat, &queue, ());
        connection.flush().unwrap();
        created_tx.send(()).unwrap();
        destroy_first_rx.recv().unwrap();
        first.destroy();
        connection.flush().unwrap();
        first_destroyed_tx.send(()).unwrap();
        destroy_second_rx.recv().unwrap();
        second.destroy();
        connection.flush().unwrap();
        second_destroyed_tx.send(()).unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| created_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state
            .virtual_keyboard_clients
            .first()
            .is_some_and(|(_, _, count)| *count == 2)
    });
    destroy_first_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        first_destroyed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        state
            .virtual_keyboard_clients
            .first()
            .is_some_and(|(_, _, count)| *count == 1)
    });
    destroy_second_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        second_destroyed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        state.virtual_keyboard_clients.is_empty()
    });
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
fn clipboard_selection_rejects_an_unissued_serial() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (request_tx, request_rx) = mpsc::sync_channel(0);
    let (sent_tx, sent_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, _events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = _events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<WlDataDeviceManager, _, _>(&queue, 1..=3, ())
            .unwrap();
        let device = manager.get_data_device(&seat, &queue, mpsc::channel().0);
        let source = manager.create_data_source(&queue, ());
        source.offer("text/plain".into());
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        request_rx.recv().unwrap();
        device.set_selection(Some(&source), 0xfeed_beef);
        connection.flush().unwrap();
        sent_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    state.map_toplevel(0);
    request_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| sent_rx.try_recv().is_ok());
    for _ in 0..8 {
        display.dispatch_clients(&mut state).unwrap();
    }
    assert_eq!(state.last_selection_serial, None);
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn primary_selection_rejects_an_unissued_serial() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (request_tx, request_rx) = mpsc::sync_channel(0);
    let (sent_tx, sent_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let manager = globals
            .bind::<ZwpPrimarySelectionDeviceManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let device = manager.get_device(&seat, &queue, ());
        let source = manager.create_source(&queue, ());
        source.offer("text/plain".into());
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        request_rx.recv().unwrap();
        device.set_selection(Some(&source), 0xfeed_beef);
        connection.flush().unwrap();
        sent_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    state.map_toplevel(0);
    request_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| sent_rx.try_recv().is_ok());
    for _ in 0..8 {
        display.dispatch_clients(&mut state).unwrap();
    }
    assert_eq!(state.last_primary_selection_serial, None);
    done_tx.send(()).unwrap();
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
    let recipient = focused.client().unwrap().id();
    keyboard.set_focus(&mut state, Some(focused), serial);
    state
        .activation_tracker
        .remember(serial, recipient, state.clock.now());
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
    state
        .connect_output(Output::new(
            OutputId(1),
            "lock-hotplug-output",
            Size::new(1024, 768),
        ))
        .unwrap();
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
fn duplicate_session_lock_output_rejects_client_without_server_panic() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (requested_tx, requested_rx) = mpsc::sync_channel(0);
    let (result_tx, result_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let output = globals.bind::<WlOutput, _, _>(&queue, 1..=4, ()).unwrap();
        let manager = globals
            .bind::<ExtSessionLockManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let lock = manager.lock(&queue, ());
        let first = compositor.create_surface(&queue, ());
        let second = compositor.create_surface(&queue, ());
        let _first_lock = lock.get_lock_surface(&first, &output, &queue, mpsc::channel().0);
        let _duplicate = lock.get_lock_surface(&second, &output, &queue, mpsc::channel().0);
        connection.flush().unwrap();
        requested_tx.send(()).unwrap();
        result_tx
            .send(events.roundtrip(&mut TestClient).is_err())
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        requested_rx.try_recv().is_ok()
    });
    let mut rejected = false;
    dispatch_until(&mut display, &mut state, |_| match result_rx.try_recv() {
        Ok(value) => {
            rejected = value;
            true
        }
        Err(_) => false,
    });
    assert!(rejected);
    assert!(state.session_is_locked());
    client.join().unwrap();
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
fn single_pixel_buffer_maps_an_xdg_toplevel() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (role_tx, role_rx) = mpsc::sync_channel(0);
    let (mapped_tx, mapped_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let manager = globals
            .bind::<WpSinglePixelBufferManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        surface.commit();
        connection.flush().unwrap();
        role_tx.send(()).unwrap();
        events.roundtrip(&mut TestClient).unwrap();

        let buffer =
            manager.create_u32_rgba_buffer(u32::MAX, 0, u32::MAX / 2, u32::MAX, &queue, ());
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, 1, 1);
        surface.commit();
        connection.flush().unwrap();
        mapped_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| role_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    dispatch_until(&mut display, &mut state, |_| mapped_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.windows.first().is_some_and(|window| window.mapped)
    });
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn alpha_modifier_is_double_buffered_with_surface_commit() {
    const FACTOR: u32 = u32::MAX / 3;

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
        let manager = globals
            .bind::<WpAlphaModifierV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        let modifier = manager.get_surface(&surface, &queue, ());
        modifier.set_multiplier(FACTOR);
        surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        committed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    let multiplier = with_states(state.windows[0].surface.wl_surface(), |states| {
        states
            .cached_state
            .get::<smithay::wayland::alpha_modifier::AlphaModifierSurfaceCachedState>()
            .current()
            .multiplier()
    });
    assert_eq!(multiplier, Some(FACTOR));
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn content_type_is_double_buffered_and_can_be_recreated() {
    use smithay::reexports::wayland_protocols::wp::content_type::v1::server::wp_content_type_v1::Type;
    use smithay::wayland::content_type::ContentTypeSurfaceCachedState;

    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (requested_tx, requested_rx) = mpsc::sync_channel(0);
    let (commit_tx, commit_rx) = mpsc::sync_channel(0);
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let (destroy_tx, destroy_rx) = mpsc::sync_channel(0);
    let (destroyed_tx, destroyed_rx) = mpsc::sync_channel(0);
    let (reset_tx, reset_rx) = mpsc::sync_channel(0);
    let (reset_committed_tx, reset_committed_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let manager = globals
            .bind::<WpContentTypeManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        let content = manager.get_surface_content_type(&surface, &queue, ());
        content.set_content_type(
            wayland_protocols::wp::content_type::v1::client::wp_content_type_v1::Type::Video,
        );
        connection.flush().unwrap();
        requested_tx.send(()).unwrap();

        commit_rx.recv().unwrap();
        surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();

        destroy_rx.recv().unwrap();
        content.destroy();
        let replacement = manager.get_surface_content_type(&surface, &queue, ());
        replacement.destroy();
        connection.flush().unwrap();
        destroyed_tx.send(()).unwrap();

        reset_rx.recv().unwrap();
        surface.commit();
        connection.flush().unwrap();
        reset_committed_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        requested_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    let content_type = |state: &Astera| {
        with_states(state.windows[0].surface.wl_surface(), |states| {
            *states
                .cached_state
                .get::<ContentTypeSurfaceCachedState>()
                .current()
                .content_type()
        })
    };
    assert_eq!(content_type(&state), Type::None);

    commit_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        committed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        content_type(state) == Type::Video
    });

    destroy_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        destroyed_rx.try_recv().is_ok()
    });
    for _ in 0..8 {
        display.dispatch_clients(&mut state).unwrap();
    }
    assert_eq!(content_type(&state), Type::Video);

    reset_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        reset_committed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        content_type(state) == Type::None
    });
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn fifo_wait_blocks_surface_state_until_presented_barrier_signals() {
    use smithay::{
        reexports::wayland_protocols::wp::content_type::v1::server::wp_content_type_v1::Type,
        wayland::{content_type::ContentTypeSurfaceCachedState, fifo::FifoBarrierCachedState},
    };

    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (barrier_tx, barrier_rx) = mpsc::sync_channel(0);
    let (wait_tx, wait_rx) = mpsc::sync_channel(0);
    let (waited_tx, waited_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let fifo_manager = globals
            .bind::<WpFifoManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let content_manager = globals
            .bind::<WpContentTypeManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        let fifo = fifo_manager.get_fifo(&surface, &queue, ());
        let content = content_manager.get_surface_content_type(&surface, &queue, ());
        fifo.set_barrier();
        surface.commit();
        connection.flush().unwrap();
        barrier_tx.send(()).unwrap();

        wait_rx.recv().unwrap();
        content.set_content_type(
            wayland_protocols::wp::content_type::v1::client::wp_content_type_v1::Type::Video,
        );
        fifo.wait_barrier();
        surface.commit();
        connection.flush().unwrap();
        waited_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| barrier_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    let surface = state.windows[0].surface.wl_surface().clone();
    let barrier = with_states(&surface, |states| {
        states
            .cached_state
            .get::<FifoBarrierCachedState>()
            .current()
            .barrier
            .clone()
            .expect("set_barrier commit must publish a barrier")
    });
    assert!(!barrier.is_signaled());

    wait_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| waited_rx.try_recv().is_ok());
    for _ in 0..8 {
        display.dispatch_clients(&mut state).unwrap();
    }
    let content_type =
        |surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface| {
            with_states(surface, |states| {
                *states
                    .cached_state
                    .get::<ContentTypeSurfaceCachedState>()
                    .current()
                    .content_type()
            })
        };
    assert_eq!(content_type(&surface), Type::None);

    let presented = state.fifo_barriers_for_output(astera_core::OutputId(0));
    assert_eq!(presented.len(), 1);
    assert_eq!(presented[0].barrier, barrier);
    state.signal_fifo_barriers(&presented);
    dispatch_until(&mut display, &mut state, |_| {
        content_type(&surface) == Type::Video
    });
    assert!(barrier.is_signaled());
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn commit_timing_blocks_state_until_monotonic_deadline() {
    use smithay::{
        reexports::wayland_protocols::wp::content_type::v1::server::wp_content_type_v1::Type,
        utils::{Monotonic, Time},
        wayland::{commit_timing::Timestamp, content_type::ContentTypeSurfaceCachedState},
    };

    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let target_seconds = u64::try_from(now.tv_sec).unwrap() + 1;
    let target_nanos = u32::try_from(now.tv_nsec).unwrap();
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
        let timing = globals
            .bind::<WpCommitTimingManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let content_manager = globals
            .bind::<WpContentTypeManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        let timer = timing.get_timer(&surface, &queue, ());
        let content = content_manager.get_surface_content_type(&surface, &queue, ());
        content.set_content_type(
            wayland_protocols::wp::content_type::v1::client::wp_content_type_v1::Type::Video,
        );
        timer.set_timestamp(
            (target_seconds >> 32) as u32,
            target_seconds as u32,
            target_nanos,
        );
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
    let content_type =
        |surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface| {
            with_states(surface, |states| {
                *states
                    .cached_state
                    .get::<ContentTypeSurfaceCachedState>()
                    .current()
                    .content_type()
            })
        };
    assert_eq!(content_type(&surface), Type::None);
    let deadline = state
        .next_timer_deadline()
        .expect("blocked commit must arm the event-loop timer");
    assert!(deadline > std::time::Instant::now());

    let target = Time::<Monotonic>::from(Duration::new(target_seconds, target_nanos));
    state.signal_commit_timers_until(Timestamp::from(target));
    assert_eq!(content_type(&surface), Type::Video);
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn commit_timing_rejects_invalid_nanoseconds() {
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
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let timing = globals
            .bind::<WpCommitTimingManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let timer = timing.get_timer(&surface, &queue, ());
        timer.set_timestamp(0, 1, 1_000_000_000);
        connection.flush().unwrap();
        result_tx
            .send(events.roundtrip(&mut TestClient).is_err())
            .unwrap();
    });

    let mut rejected = None;
    dispatch_until(&mut display, &mut state, |_| match result_rx.try_recv() {
        Ok(result) => {
            rejected = Some(result);
            true
        }
        Err(_) => false,
    });
    assert_eq!(rejected, Some(true));
    client.join().unwrap();
}

#[test]
fn xdg_dialog_tracks_modal_parent_and_can_be_recreated() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (modal_tx, modal_rx) = mpsc::sync_channel(0);
    let (destroy_tx, destroy_rx) = mpsc::sync_channel(0);
    let (destroyed_tx, destroyed_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let dialogs = globals
            .bind::<XdgWmDialogV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let parent_surface = compositor.create_surface(&queue, ());
        let parent_xdg = shell.get_xdg_surface(&parent_surface, &queue, ());
        let parent = parent_xdg.get_toplevel(&queue, ());
        let child_surface = compositor.create_surface(&queue, ());
        let child_xdg = shell.get_xdg_surface(&child_surface, &queue, ());
        let child = child_xdg.get_toplevel(&queue, ());
        child.set_parent(Some(&parent));
        let dialog = dialogs.get_xdg_dialog(&child, &queue, ());
        dialog.set_modal();
        connection.flush().unwrap();
        modal_tx.send(()).unwrap();

        destroy_rx.recv().unwrap();
        dialog.destroy();
        let replacement = dialogs.get_xdg_dialog(&child, &queue, ());
        replacement.destroy();
        connection.flush().unwrap();
        destroyed_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| modal_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 2);
    let dialog_state = |state: &Astera| {
        with_states(state.windows[1].surface.wl_surface(), |states| {
            let attributes = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap();
            (attributes.modal, attributes.parent.clone())
        })
    };
    let (modal, parent) = dialog_state(&state);
    assert!(modal);
    assert_eq!(parent, Some(state.windows[0].surface.wl_surface().clone()));

    destroy_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        destroyed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| !dialog_state(state).0);
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn unmapping_or_destroying_toplevel_reparents_its_children() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (mapped_tx, mapped_rx) = mpsc::sync_channel(0);
    let (unmap_tx, unmap_rx) = mpsc::sync_channel(0);
    let (unmapped_tx, unmapped_rx) = mpsc::sync_channel(0);
    let (setup_destroy_tx, setup_destroy_rx) = mpsc::sync_channel(0);
    let (destroy_ready_tx, destroy_ready_rx) = mpsc::sync_channel(0);
    let (destroy_tx, destroy_rx) = mpsc::sync_channel(0);
    let (destroyed_tx, destroyed_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let shm = globals.bind::<WlShm, _, _>(&queue, 1..=1, ()).unwrap();

        let root_surface = compositor.create_surface(&queue, ());
        let root_xdg = shell.get_xdg_surface(&root_surface, &queue, ());
        let root = root_xdg.get_toplevel(&queue, ());
        let middle_surface = compositor.create_surface(&queue, ());
        let middle_xdg = shell.get_xdg_surface(&middle_surface, &queue, ());
        let middle = middle_xdg.get_toplevel(&queue, ());
        let leaf_surface = compositor.create_surface(&queue, ());
        let leaf_xdg = shell.get_xdg_surface(&leaf_surface, &queue, ());
        let leaf = leaf_xdg.get_toplevel(&queue, ());
        middle.set_parent(Some(&root));
        leaf.set_parent(Some(&middle));
        root_surface.commit();
        middle_surface.commit();
        leaf_surface.commit();
        events.roundtrip(&mut TestClient).unwrap();

        let fd = rustix::fs::memfd_create("astera-parent-chain", rustix::fs::MemfdFlags::CLOEXEC)
            .unwrap();
        rustix::fs::ftruncate(&fd, 5 * 4 * 4 * 4).unwrap();
        let pool = shm.create_pool(fd.as_fd(), 5 * 4 * 4 * 4, &queue, ());
        for (index, surface) in [&root_surface, &middle_surface, &leaf_surface]
            .into_iter()
            .enumerate()
        {
            let buffer = pool.create_buffer(
                (index * 4 * 4 * 4) as i32,
                4,
                4,
                4 * 4,
                wayland_client::protocol::wl_shm::Format::Argb8888,
                &queue,
                (),
            );
            surface.attach(Some(&buffer), 0, 0);
            surface.damage_buffer(0, 0, 4, 4);
            surface.commit();
        }
        connection.flush().unwrap();
        mapped_tx.send(()).unwrap();

        unmap_rx.recv().unwrap();
        middle_surface.attach(None, 0, 0);
        middle_surface.commit();
        connection.flush().unwrap();
        unmapped_tx.send(()).unwrap();

        setup_destroy_rx.recv().unwrap();
        let destroyed_middle_surface = compositor.create_surface(&queue, ());
        let destroyed_middle_xdg = shell.get_xdg_surface(&destroyed_middle_surface, &queue, ());
        let destroyed_middle = destroyed_middle_xdg.get_toplevel(&queue, ());
        let destroyed_leaf_surface = compositor.create_surface(&queue, ());
        let destroyed_leaf_xdg = shell.get_xdg_surface(&destroyed_leaf_surface, &queue, ());
        let destroyed_leaf = destroyed_leaf_xdg.get_toplevel(&queue, ());
        destroyed_middle.set_parent(Some(&root));
        destroyed_leaf.set_parent(Some(&destroyed_middle));
        destroyed_middle_surface.commit();
        destroyed_leaf_surface.commit();
        events.roundtrip(&mut TestClient).unwrap();
        for (index, surface) in [&destroyed_middle_surface, &destroyed_leaf_surface]
            .into_iter()
            .enumerate()
        {
            let buffer = pool.create_buffer(
                ((index + 3) * 4 * 4 * 4) as i32,
                4,
                4,
                4 * 4,
                wayland_client::protocol::wl_shm::Format::Argb8888,
                &queue,
                (),
            );
            surface.attach(Some(&buffer), 0, 0);
            surface.damage_buffer(0, 0, 4, 4);
            surface.commit();
        }
        connection.flush().unwrap();
        destroy_ready_tx.send(()).unwrap();

        destroy_rx.recv().unwrap();
        destroyed_middle.destroy();
        connection.flush().unwrap();
        destroyed_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| mapped_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.windows.len() == 3 && state.windows.iter().all(|window| window.mapped)
    });
    let root = state.windows[0].surface.wl_surface().clone();
    let middle = state.windows[1].surface.wl_surface().clone();
    let leaf = state.windows[2].surface.wl_surface().clone();

    unmap_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| unmapped_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| !state.windows[1].mapped);
    let parent_of =
        |surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface| {
            with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .parent
                    .clone()
            })
        };
    assert_eq!(parent_of(&middle), None);
    assert_eq!(parent_of(&leaf), Some(root.clone()));

    setup_destroy_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        destroy_ready_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        state.windows.len() == 5 && state.windows[3..].iter().all(|window| window.mapped)
    });
    let destroyed_middle = state.windows[3].surface.wl_surface().clone();
    let destroyed_leaf = state.windows[4].surface.wl_surface().clone();
    assert_eq!(parent_of(&destroyed_leaf), Some(destroyed_middle));

    destroy_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        destroyed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 4);
    assert_eq!(parent_of(&destroyed_leaf), Some(root));

    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn xdg_foreign_invalid_export_disconnects_client_without_server_panic() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (requested_tx, requested_rx) = mpsc::sync_channel(0);
    let (result_tx, result_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let exporter = globals
            .bind::<ZxdgExporterV2, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let (handle_tx, _handle_rx) = mpsc::channel();
        let _exported = exporter.export_toplevel(&surface, &queue, handle_tx);
        connection.flush().unwrap();
        requested_tx.send(()).unwrap();

        result_tx
            .send(events.roundtrip(&mut TestClient).is_err())
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        requested_rx.try_recv().is_ok()
    });
    let mut result = None;
    dispatch_until(&mut display, &mut state, |_| {
        result = result_rx.try_recv().ok();
        result.is_some()
    });
    assert_eq!(result, Some(true));
    client.join().unwrap();
}

#[test]
fn xdg_foreign_links_cross_client_parent_and_revokes_it_with_export() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (parent_server, parent_client) = UnixStream::pair().unwrap();
    let (child_server, child_client) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(parent_server, Arc::new(ClientState::default()))
        .unwrap();
    display
        .handle()
        .insert_client(child_server, Arc::new(ClientState::default()))
        .unwrap();

    let (handle_tx, handle_rx) = mpsc::channel();
    let (revoke_tx, revoke_rx) = mpsc::sync_channel(0);
    let (parent_done_tx, parent_done_rx) = mpsc::sync_channel(0);
    let parent = thread::spawn(move || {
        let connection = Connection::from_socket(parent_client).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let exporter = globals
            .bind::<ZxdgExporterV2, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let toplevel = xdg_surface.get_toplevel(&queue, ());
        toplevel.set_app_id("org.astera.Parent".into());
        let exported = exporter.export_toplevel(&surface, &queue, handle_tx);
        surface.commit();
        connection.flush().unwrap();
        events.blocking_dispatch(&mut TestClient).unwrap();
        revoke_rx.recv().unwrap();
        exported.destroy();
        connection.flush().unwrap();
        parent_done_rx.recv().unwrap();
    });

    let handle = loop {
        display.dispatch_clients(&mut state).unwrap();
        display.flush_clients().unwrap();
        if let Ok(handle) = handle_rx.try_recv() {
            break handle;
        }
        thread::yield_now();
    };
    let (linked_tx, linked_rx) = mpsc::sync_channel(0);
    let (check_tx, check_rx) = mpsc::sync_channel(0);
    let (destroyed_tx, destroyed_rx) = mpsc::channel();
    let (observed_tx, observed_rx) = mpsc::sync_channel(0);
    let child = thread::spawn(move || {
        let connection = Connection::from_socket(child_client).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let importer = globals
            .bind::<ZxdgImporterV2, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let toplevel = xdg_surface.get_toplevel(&queue, ());
        toplevel.set_app_id("org.astera.Child".into());
        let imported = importer.import_toplevel(handle, &queue, destroyed_tx);
        imported.set_parent_of(&surface);
        surface.commit();
        connection.flush().unwrap();
        linked_tx.send(()).unwrap();
        check_rx.recv().unwrap();
        loop {
            if destroyed_rx.try_recv().is_ok() {
                observed_tx.send(true).unwrap();
                break;
            }
            events.blocking_dispatch(&mut TestClient).unwrap();
        }
    });

    dispatch_until(&mut display, &mut state, |_| linked_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 2);
    let parent_surface = state.windows[0].surface.wl_surface().clone();
    let child_surface = state.windows[1].surface.wl_surface().clone();
    let foreign_parent = || {
        with_states(&child_surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .parent
                .clone()
        })
    };
    dispatch_until(&mut display, &mut state, |_| {
        foreign_parent().as_ref() == Some(&parent_surface)
    });

    revoke_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| foreign_parent().is_none());
    display.flush_clients().unwrap();
    check_tx.send(()).unwrap();
    assert!(observed_rx.recv_timeout(Duration::from_secs(1)).unwrap());
    parent_done_tx.send(()).unwrap();
    parent.join().unwrap();
    child.join().unwrap();
}

#[test]
fn system_bell_marks_target_urgent_and_flash_expires_on_timer() {
    let mut display = Display::<Astera>::new().unwrap();
    let start = Instant::now();
    let clock = Arc::new(ManualClock::new(start));
    let mut state = Astera::new_with_clock(&display.handle(), Config::default(), clock.clone());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (ring_tx, ring_rx) = mpsc::sync_channel(0);
    let (sent_tx, sent_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let bell = globals
            .bind::<XdgSystemBellV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        surface.commit();
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        ring_rx.recv().unwrap();
        bell.ring(Some(&surface));
        connection.flush().unwrap();
        sent_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    state.map_toplevel(0);
    let generation = state.render_generation();
    ring_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| sent_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.bell_flash_active());
    assert!(state.windows[0].urgent);
    assert!(state.render_generation() > generation);
    assert_eq!(
        state.next_visual_timer_deadline(),
        Some(start + Duration::from_millis(150))
    );

    let generation = state.render_generation();
    clock.advance(Duration::from_millis(151));
    state.process_idle_timers();
    assert!(!state.bell_flash_active());
    assert!(state.render_generation() > generation);
    assert_ne!(
        state.next_visual_timer_deadline(),
        Some(start + Duration::from_millis(150))
    );

    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn toplevel_tag_keeps_tag_and_accessible_description_distinct() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (updated_tx, updated_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let tags = globals
            .bind::<XdgToplevelTagManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let toplevel = xdg_surface.get_toplevel(&queue, ());
        tags.set_toplevel_tag(&toplevel, "settings".into());
        tags.set_toplevel_description(&toplevel, "Application preferences".into());
        surface.commit();
        connection.flush().unwrap();
        updated_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| updated_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.windows.len() == 1
            && state.windows[0].tag.as_deref() == Some("settings")
            && state.windows[0].description.as_deref() == Some("Application preferences")
    });
    state.map_toplevel(0);
    let snapshot = state.public_snapshot();
    let metadata = &snapshot.windows[0].metadata;
    assert_eq!(metadata.tag.as_deref(), Some("settings"));
    assert_eq!(
        metadata.description.as_deref(),
        Some("Application preferences")
    );

    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn toplevel_icon_is_double_buffered_and_immutable_without_server_panic() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (set_tx, set_rx) = mpsc::sync_channel(0);
    let (pending_tx, pending_rx) = mpsc::sync_channel(0);
    let (commit_tx, commit_rx) = mpsc::sync_channel(0);
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let (inspect_tx, inspect_rx) = mpsc::sync_channel(0);
    let (destroyed_tx, destroyed_rx) = mpsc::sync_channel(0);
    let (destroy_ok_tx, destroy_ok_rx) = mpsc::sync_channel(0);
    let (mutated_tx, mutated_rx) = mpsc::sync_channel(0);
    let (error_tx, error_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let shm = globals.bind::<WlShm, _, _>(&queue, 1..=1, ()).unwrap();
        let icons = globals
            .bind::<XdgToplevelIconManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let toplevel = xdg_surface.get_toplevel(&queue, ());
        surface.commit();
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();

        set_rx.recv().unwrap();
        let fd = rustix::fs::memfd_create("astera-icon", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        rustix::fs::ftruncate(&fd, 16 * 16 * 4).unwrap();
        let pool = shm.create_pool(fd.as_fd(), 16 * 16 * 4, &queue, ());
        let buffer = pool.create_buffer(
            0,
            16,
            16,
            16 * 4,
            wayland_client::protocol::wl_shm::Format::Argb8888,
            &queue,
            (),
        );
        let icon = icons.create_icon(&queue, ());
        icon.set_name("org.astera.Editor".into());
        icon.add_buffer(&buffer, 1);
        icons.set_icon(&toplevel, Some(&icon));
        connection.flush().unwrap();
        pending_tx.send(()).unwrap();

        commit_rx.recv().unwrap();
        surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();
        inspect_rx.recv().unwrap();

        icon.destroy();
        connection.flush().unwrap();
        destroyed_tx.send(()).unwrap();
        destroy_ok_tx
            .send(events.roundtrip(&mut TestClient).is_ok())
            .unwrap();

        let immutable_icon = icons.create_icon(&queue, ());
        immutable_icon.set_name("immutable".into());
        icons.set_icon(&toplevel, Some(&immutable_icon));
        immutable_icon.set_name("must-fail".into());
        connection.flush().unwrap();
        mutated_tx.send(()).unwrap();
        error_tx
            .send(events.roundtrip(&mut TestClient).is_err())
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    set_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| pending_rx.try_recv().is_ok());
    for _ in 0..4 {
        display.dispatch_clients(&mut state).unwrap();
    }
    assert!(state.windows[0].icon_name.is_none());
    assert!(state.windows[0].icon_buffers.is_empty());

    commit_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        committed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        state.windows[0].icon_name.as_deref() == Some("org.astera.Editor")
            && state.windows[0].icon_buffers == [(16, 1)]
    });
    state.map_toplevel(0);
    assert_eq!(
        state.public_snapshot().windows[0]
            .metadata
            .icon_name
            .as_deref(),
        Some("org.astera.Editor")
    );

    inspect_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        destroyed_rx.try_recv().is_ok()
    });
    display.flush_clients().unwrap();
    let mut destroy_succeeded = false;
    dispatch_until(&mut display, &mut state, |_| {
        match destroy_ok_rx.try_recv() {
            Ok(value) => {
                destroy_succeeded = value;
                true
            }
            Err(_) => false,
        }
    });
    assert!(destroy_succeeded);

    dispatch_until(&mut display, &mut state, |_| mutated_rx.try_recv().is_ok());
    display.flush_clients().unwrap();
    let mut rejected = false;
    dispatch_until(&mut display, &mut state, |_| match error_rx.try_recv() {
        Ok(value) => {
            rejected = value;
            true
        }
        Err(_) => false,
    });
    assert!(rejected);
    client.join().unwrap();
}

#[test]
fn tearing_hint_is_double_buffered_and_destroy_restores_vsync() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (set_tx, set_rx) = mpsc::sync_channel(0);
    let (commit_tx, commit_rx) = mpsc::sync_channel(0);
    let (destroy_tx, destroy_rx) = mpsc::sync_channel(0);
    let (reset_tx, reset_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let manager = globals
            .bind::<WpTearingControlManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let control = manager.get_tearing_control(&surface, &queue, ());
        control.set_presentation_hint(wp_tearing_control_v1::PresentationHint::Async);
        connection.flush().unwrap();
        set_tx.send(()).unwrap();

        commit_rx.recv().unwrap();
        surface.commit();
        connection.flush().unwrap();

        destroy_rx.recv().unwrap();
        control.destroy();
        connection.flush().unwrap();

        reset_rx.recv().unwrap();
        surface.commit();
        connection.flush().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| set_rx.try_recv().is_ok());
    display.dispatch_clients(&mut state).unwrap();
    let surface = state
        .tearing_controls
        .keys()
        .next()
        .expect("client created tearing control")
        .clone();
    assert_eq!(state.pending_tearing_hints.get(&surface), Some(&true));
    assert!(!state.asynchronous_surfaces.contains(&surface));

    commit_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |state| {
        state.asynchronous_surfaces.contains(&surface)
    });

    destroy_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |state| {
        !state.tearing_controls.contains_key(&surface)
    });
    assert!(state.asynchronous_surfaces.contains(&surface));
    assert_eq!(state.pending_tearing_hints.get(&surface), Some(&false));

    reset_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |state| {
        !state.asynchronous_surfaces.contains(&surface)
    });
    client.join().unwrap();
}

#[test]
fn duplicate_tearing_control_is_rejected_without_server_panic() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (sent_tx, sent_rx) = mpsc::sync_channel(0);
    let (error_tx, error_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let manager = globals
            .bind::<WpTearingControlManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let _first = manager.get_tearing_control(&surface, &queue, ());
        let _duplicate = manager.get_tearing_control(&surface, &queue, ());
        connection.flush().unwrap();
        sent_tx.send(()).unwrap();
        error_tx
            .send(events.roundtrip(&mut TestClient).is_err())
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| sent_rx.try_recv().is_ok());
    display.dispatch_clients(&mut state).unwrap();
    display.flush_clients().unwrap();
    let mut rejected = false;
    dispatch_until(&mut display, &mut state, |_| match error_rx.try_recv() {
        Ok(value) => {
            rejected = value;
            true
        }
        Err(_) => false,
    });
    assert!(rejected);
    client.join().unwrap();
}

#[test]
fn pointer_warp_requires_current_enter_serial_and_surface_bounds() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (read_enter_tx, read_enter_rx) = mpsc::sync_channel(0);
    let (serial_tx, serial_rx) = mpsc::sync_channel(0);
    let (invalid_go_tx, invalid_go_rx) = mpsc::sync_channel(0);
    let (invalid_done_tx, invalid_done_rx) = mpsc::sync_channel(0);
    let (valid_go_tx, valid_go_rx) = mpsc::sync_channel(0);
    let (valid_done_tx, valid_done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let warp = globals
            .bind::<WpPointerWarpV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let shm = globals.bind::<WlShm, _, _>(&queue, 1..=1, ()).unwrap();
        let (enter_tx, enter_rx) = mpsc::channel();
        let pointer = seat.get_pointer(&queue, enter_tx);
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        surface.commit();
        events.roundtrip(&mut TestClient).unwrap();
        let fd = rustix::fs::memfd_create("astera-warp", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        rustix::fs::ftruncate(&fd, 100 * 100 * 4).unwrap();
        let pool = shm.create_pool(fd.as_fd(), 100 * 100 * 4, &queue, ());
        let buffer = pool.create_buffer(
            0,
            100,
            100,
            100 * 4,
            wayland_client::protocol::wl_shm::Format::Argb8888,
            &queue,
            (),
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, 100, 100);
        surface.commit();
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();

        read_enter_rx.recv().unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        let serial = enter_rx.recv().unwrap();
        serial_tx.send(serial).unwrap();

        invalid_go_rx.recv().unwrap();
        warp.warp_pointer(&surface, &pointer, 30.0, 40.0, serial.wrapping_add(1));
        warp.warp_pointer(&surface, &pointer, 10_000.0, 10_000.0, serial);
        connection.flush().unwrap();
        invalid_done_tx.send(()).unwrap();

        valid_go_rx.recv().unwrap();
        warp.warp_pointer(&surface, &pointer, 30.0, 40.0, serial);
        connection.flush().unwrap();
        valid_done_tx.send(()).unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.windows.len() == 1 && state.windows[0].mapped
    });
    let window = state.windows[0].id;
    let (origin, _, _, _) = state.visual_geometry(window).unwrap();
    let initial = (origin.x as f64 + 5.0, origin.y as f64 + 5.0).into();
    state.handle_pointer_motion(initial, 1);
    display.flush_clients().unwrap();

    read_enter_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| serial_rx.try_recv().is_ok());
    invalid_go_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        invalid_done_rx.try_recv().is_ok()
    });
    display.dispatch_clients(&mut state).unwrap();
    assert_eq!(state.pointer_location, initial);

    valid_go_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        valid_done_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        state.pointer_location
            == SmithayPoint::from((origin.x as f64 + 30.0, origin.y as f64 + 40.0))
    });
    client.join().unwrap();
}

#[test]
fn xdg_toplevel_drag_moves_window_with_data_device_grab() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (start_tx, start_rx) = mpsc::sync_channel(0);
    let (started_tx, started_rx) = mpsc::sync_channel(0);
    let (check_timestamp_tx, check_timestamp_rx) = mpsc::sync_channel(0);
    let (timestamp_result_tx, timestamp_result_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let (cleanup_tx, cleanup_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let shm = globals.bind::<WlShm, _, _>(&queue, 1..=1, ()).unwrap();
        let data_manager = globals
            .bind::<WlDataDeviceManager, _, _>(&queue, 1..=3, ())
            .unwrap();
        let drag_manager = globals
            .bind::<XdgToplevelDragManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let timestamp_manager = globals
            .bind::<ZwpInputTimestampsManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let (button_tx, button_rx) = mpsc::channel();
        let pointer = seat.get_pointer(&queue, PointerButtonSerial(button_tx));
        let (timestamp_tx, timestamp_rx) = mpsc::channel();
        let _timestamps = timestamp_manager.get_pointer_timestamps(&pointer, &queue, timestamp_tx);
        let device = data_manager.get_data_device(&seat, &queue, mpsc::channel().0);
        let source = data_manager.create_data_source(&queue, ());
        source.offer("application/x-astera-tab".into());
        let drag = drag_manager.get_xdg_toplevel_drag(&source, &queue, ());
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let toplevel = xdg_surface.get_toplevel(&queue, ());
        drag.attach(&toplevel, 20, 25);
        surface.commit();
        events.roundtrip(&mut TestClient).unwrap();
        let fd = rustix::fs::memfd_create("astera-drag", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        rustix::fs::ftruncate(&fd, 100 * 100 * 4).unwrap();
        let pool = shm.create_pool(fd.as_fd(), 100 * 100 * 4, &queue, ());
        let buffer = pool.create_buffer(
            0,
            100,
            100,
            100 * 4,
            wayland_client::protocol::wl_shm::Format::Argb8888,
            &queue,
            (),
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, 100, 100);
        surface.commit();
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();

        start_rx.recv().unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        let serial = button_rx.recv().unwrap();
        while timestamp_rx.try_recv().is_ok() {}
        device.start_drag(Some(&source), &surface, None, serial);
        connection.flush().unwrap();
        started_tx.send(()).unwrap();
        check_timestamp_rx.recv().unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        timestamp_result_tx
            .send(timestamp_rx.try_recv().is_ok())
            .unwrap();
        done_rx.recv().unwrap();
        drag.destroy();
        connection.flush().unwrap();
        cleanup_tx.send(()).unwrap();
        drop(pointer);
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.windows.len() == 1 && state.windows[0].mapped && state.toplevel_drags.len() == 1
    });
    let window = state.windows[0].id;
    let dragged_surface = state.windows[0].surface.wl_surface().clone();
    let workspace = state.desktop.find_window(window).unwrap();
    state
        .desktop
        .apply_window(
            workspace,
            WindowTransaction::SetMode {
                id: window,
                mode: WindowMode::Floating,
                viewport_size: state.desktop.outputs[&state.active_output]
                    .output
                    .logical_size,
            },
        )
        .unwrap();
    let (origin, _, _, _) = state.visual_geometry(window).unwrap();
    state.handle_pointer_motion((origin.x as f64 + 5.0, origin.y as f64 + 5.0).into(), 1);
    let pointer = state.pointer.clone();
    let press_serial = state.next_serial();
    pointer.button(
        &mut state,
        &ButtonEvent {
            serial: press_serial,
            time: 2,
            button: 0x110,
            state: smithay::backend::input::ButtonState::Pressed,
        },
    );
    pointer.frame(&mut state);
    display.flush_clients().unwrap();
    start_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| started_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state
            .drag
            .is_some_and(|drag| drag.window == window && drag.source == DragSource::Dnd)
    });

    state.handle_pointer_motion((origin.x as f64 + 80.0, origin.y as f64 + 70.0).into(), 3);
    let preview = state.drag.unwrap().target;
    assert_eq!(preview.origin, Point::new(origin.x + 60, origin.y + 45));
    assert_ne!(
        state.pointer.current_focus().as_ref(),
        Some(&dragged_surface)
    );
    display.flush_clients().unwrap();
    check_timestamp_tx.send(()).unwrap();
    let mut timestamp_received = None;
    dispatch_until(&mut display, &mut state, |_| {
        timestamp_received = timestamp_result_rx.try_recv().ok();
        timestamp_received.is_some()
    });
    assert_eq!(timestamp_received, Some(false));
    let release_serial = state.next_serial();
    pointer.button(
        &mut state,
        &ButtonEvent {
            serial: release_serial,
            time: 4,
            button: 0x110,
            state: smithay::backend::input::ButtonState::Released,
        },
    );
    pointer.frame(&mut state);
    assert!(state.drag.is_none());
    assert_eq!(
        state.desktop.workspace(workspace).unwrap().floating[&window]
            .viewport
            .rect,
        preview
    );

    done_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| cleanup_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.toplevel_drags.is_empty()
    });
    client.join().unwrap();
}

#[test]
fn toplevel_drag_cannot_be_destroyed_before_dnd_ends() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (sent_tx, sent_rx) = mpsc::sync_channel(0);
    let (error_tx, error_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let data_manager = globals
            .bind::<WlDataDeviceManager, _, _>(&queue, 1..=3, ())
            .unwrap();
        let drag_manager = globals
            .bind::<XdgToplevelDragManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let source = data_manager.create_data_source(&queue, ());
        let drag = drag_manager.get_xdg_toplevel_drag(&source, &queue, ());
        drag.destroy();
        connection.flush().unwrap();
        sent_tx.send(()).unwrap();
        error_tx
            .send(events.roundtrip(&mut TestClient).is_err())
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| sent_rx.try_recv().is_ok());
    display.flush_clients().unwrap();
    let mut rejected = false;
    dispatch_until(&mut display, &mut state, |_| match error_rx.try_recv() {
        Ok(value) => {
            rejected = value;
            true
        }
        Err(_) => false,
    });
    assert!(rejected);
    client.join().unwrap();
}

#[test]
fn toplevel_drag_source_cannot_be_used_as_clipboard_selection() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (sent_tx, sent_rx) = mpsc::sync_channel(0);
    let (error_tx, error_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let data_manager = globals
            .bind::<WlDataDeviceManager, _, _>(&queue, 1..=3, ())
            .unwrap();
        let drag_manager = globals
            .bind::<XdgToplevelDragManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let device = data_manager.get_data_device(&seat, &queue, mpsc::channel().0);
        let source = data_manager.create_data_source(&queue, ());
        let _drag = drag_manager.get_xdg_toplevel_drag(&source, &queue, ());
        device.set_selection(Some(&source), 0);
        connection.flush().unwrap();
        sent_tx.send(()).unwrap();
        error_tx
            .send(events.roundtrip(&mut TestClient).is_err())
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| sent_rx.try_recv().is_ok());
    display.dispatch_clients(&mut state).unwrap();
    display.flush_clients().unwrap();
    let mut rejected = false;
    dispatch_until(&mut display, &mut state, |_| match error_rx.try_recv() {
        Ok(value) => {
            rejected = value;
            true
        }
        Err(_) => false,
    });
    assert!(rejected);
    client.join().unwrap();
}

#[test]
fn ext_workspace_publishes_groups_and_commits_requests_atomically() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state
        .connect_output(Output::new(
            OutputId(1),
            "workspace-second-output",
            Size::new(1024, 768),
        ))
        .unwrap();
    let first = state.desktop.active_workspace_id(OutputId(0)).unwrap();
    let second = state.desktop.active_workspace_id(OutputId(1)).unwrap();
    state
        .desktop
        .apply(WorkspaceTransaction::SetName {
            workspace: first,
            name: Some("one".into()),
        })
        .unwrap();
    state
        .desktop
        .apply(WorkspaceTransaction::SetName {
            workspace: second,
            name: Some("two".into()),
        })
        .unwrap();

    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let shared = Arc::new(std::sync::Mutex::new(WorkspaceClientState::default()));
    let client_shared = shared.clone();
    let (initial_tx, initial_rx) = mpsc::sync_channel(0);
    let (request_tx, request_rx) = mpsc::sync_channel(0);
    let (requested_tx, requested_rx) = mpsc::sync_channel(0);
    let (observe_tx, observe_rx) = mpsc::sync_channel(0);
    let (observed_tx, observed_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        WORKSPACE_CLIENT_STATE.with(|slot| *slot.borrow_mut() = Some(client_shared.clone()));
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let manager = globals
            .bind::<ExtWorkspaceManagerV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        initial_tx.send(()).unwrap();

        request_rx.recv().unwrap();
        let (workspace, target_group) = {
            let state = client_shared.lock().unwrap();
            let workspace = state
                .workspaces
                .iter()
                .find(|workspace| {
                    state
                        .names
                        .get(&workspace.id())
                        .is_some_and(|name| name == "one")
                })
                .unwrap()
                .clone();
            (workspace, state.groups[1].clone())
        };
        workspace.assign(&target_group);
        workspace.activate();
        manager.commit();
        connection.flush().unwrap();
        requested_tx.send(()).unwrap();

        observe_rx.recv().unwrap();
        events.roundtrip(&mut TestClient).unwrap();
        observed_tx.send(()).unwrap();
        WORKSPACE_CLIENT_STATE.with(|slot| slot.borrow_mut().take());
    });

    dispatch_until(&mut display, &mut state, |_| initial_rx.try_recv().is_ok());
    {
        let observed = shared.lock().unwrap();
        assert_eq!(observed.groups.len(), 2);
        // The core keeps one empty trailing workspace per output for the next insertion.
        assert_eq!(observed.workspaces.len(), 4);
        assert_eq!(observed.memberships.len(), 4);
        assert_eq!(observed.done, 1);
        assert!(observed.names.values().any(|name| name == "one"));
        assert!(observed.names.values().any(|name| name == "two"));
        assert_eq!(observed.active.len(), 2);
    }

    request_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        requested_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| {
        state
            .desktop
            .workspace_location(first)
            .is_ok_and(|location| location.output == Some(OutputId(1)))
            && state.active_output == OutputId(1)
    });
    assert_eq!(state.desktop.active_workspace_id(OutputId(1)), Some(first));

    observe_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| observed_rx.try_recv().is_ok());
    {
        let observed = shared.lock().unwrap();
        assert!(observed.memberships.len() >= 3);
        assert!(observed.done >= 2);
    }
    client.join().unwrap();
}

#[test]
fn foreign_toplevel_list_tracks_metadata_and_role_lifetime() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (created_tx, created_rx) = mpsc::sync_channel(0);
    let (update_tx, update_rx) = mpsc::sync_channel(0);
    let (updated_tx, updated_rx) = mpsc::sync_channel(0);
    let (destroy_tx, destroy_rx) = mpsc::sync_channel(0);
    let (destroyed_tx, destroyed_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let _list = globals
            .bind::<ExtForeignToplevelListV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let toplevel = xdg_surface.get_toplevel(&queue, ());
        connection.flush().unwrap();
        created_tx.send(()).unwrap();

        update_rx.recv().unwrap();
        toplevel.set_title("Astera editor".into());
        toplevel.set_app_id("org.astera.Editor".into());
        connection.flush().unwrap();
        updated_tx.send(()).unwrap();

        destroy_rx.recv().unwrap();
        toplevel.destroy();
        xdg_surface.destroy();
        connection.flush().unwrap();
        destroyed_tx.send(()).unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| created_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.windows.len() == 1 && state.windows[0].foreign_toplevel.resources().len() == 1
    });
    let handle = state.windows[0].foreign_toplevel.clone();
    assert_eq!(handle.title(), "");
    assert_eq!(handle.app_id(), "");

    update_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| updated_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state.windows[0].foreign_toplevel.title() == "Astera editor"
            && state.windows[0].foreign_toplevel.app_id() == "org.astera.Editor"
    });

    destroy_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        destroyed_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| state.windows.is_empty());
    assert!(handle.is_closed());
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
        let primary_selection = globals
            .bind::<ZwpPrimarySelectionDeviceManagerV1, _, _>(&queue, 1..=1, ())
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
        let _primary_device = primary_selection.get_device(&seat, &queue, ());
        let _primary_source = primary_selection.create_source(&queue, ());
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
    state.cursor_image_status =
        smithay::input::pointer::CursorImageStatus::Surface(constrained.clone());
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
    assert_eq!(
        state.cursor_image_status,
        smithay::input::pointer::CursorImageStatus::Hidden,
        "destroying the current cursor surface must release the stale resource"
    );
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn layer_surface_is_closed_when_its_output_is_removed() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state
        .connect_output(Output::new(
            OutputId(1),
            "layer-fallback-output",
            Size::new(1024, 768),
        ))
        .unwrap();
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client_closed = closed.clone();
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        // The initial output global is registered before the hotplugged fallback output.
        let output = globals.bind::<WlOutput, _, _>(&queue, 1..=4, ()).unwrap();
        let shell = globals
            .bind::<ZwlrLayerShellV1, _, _>(&queue, 1..=4, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let layer = shell.get_layer_surface(
            &surface,
            Some(&output),
            wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer::Top,
            "hotplug-test".into(),
            &queue,
            client_closed.clone(),
        );
        layer.set_size(100, 100);
        surface.commit();
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        while !client_closed.load(std::sync::atomic::Ordering::Acquire) {
            events.blocking_dispatch(&mut TestClient).unwrap();
        }
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| {
        state
            .layers
            .first()
            .is_some_and(|layer| layer.output == OutputId(0))
    });
    state.disconnect_output(OutputId(0)).unwrap();
    display.flush_clients().unwrap();
    client.join().unwrap();
    assert!(closed.load(std::sync::atomic::Ordering::Acquire));
    assert!(state.layers.is_empty());
}

#[test]
fn layer_surface_targeting_an_already_removed_output_is_closed() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state
        .connect_output(Output::new(
            OutputId(1),
            "layer-fallback-output",
            Size::new(1024, 768),
        ))
        .unwrap();
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (bound_tx, bound_rx) = mpsc::sync_channel(0);
    let (removed_tx, removed_rx) = mpsc::sync_channel(0);
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client_closed = closed.clone();
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        // Keep the proxy alive after its global has been removed, reproducing the race between
        // output hot-unplug and a client's get_layer_surface request.
        let removed_output = globals.bind::<WlOutput, _, _>(&queue, 1..=4, ()).unwrap();
        let shell = globals
            .bind::<ZwlrLayerShellV1, _, _>(&queue, 1..=4, ())
            .unwrap();
        connection.flush().unwrap();
        bound_tx.send(()).unwrap();
        removed_rx.recv().unwrap();

        let surface = compositor.create_surface(&queue, ());
        let layer = shell.get_layer_surface(
            &surface,
            Some(&removed_output),
            wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer::Top,
            "stale-output-test".into(),
            &queue,
            client_closed.clone(),
        );
        layer.set_size(100, 100);
        surface.commit();
        connection.flush().unwrap();
        while !client_closed.load(std::sync::atomic::Ordering::Acquire) {
            events.blocking_dispatch(&mut TestClient).unwrap();
        }
    });

    dispatch_until(&mut display, &mut state, |_| bound_rx.try_recv().is_ok());
    state.disconnect_output(OutputId(0)).unwrap();
    display.flush_clients().unwrap();
    removed_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        closed.load(std::sync::atomic::Ordering::Acquire)
    });
    display.flush_clients().unwrap();
    client.join().unwrap();
    assert!(closed.load(std::sync::atomic::Ordering::Acquire));
    assert!(state.layers.is_empty());
}

#[test]
fn initial_fullscreen_with_a_removed_output_does_not_crash_the_compositor() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (bound_tx, bound_rx) = mpsc::sync_channel(0);
    let (removed_tx, removed_rx) = mpsc::sync_channel(0);
    let (requested_tx, requested_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, event_queue) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = event_queue.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let removed_output = globals.bind::<WlOutput, _, _>(&queue, 1..=4, ()).unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        connection.flush().unwrap();
        bound_tx.send(()).unwrap();
        removed_rx.recv().unwrap();

        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let toplevel = xdg_surface.get_toplevel(&queue, ());
        toplevel.set_fullscreen(Some(&removed_output));
        surface.commit();
        connection.flush().unwrap();
        requested_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| bound_rx.try_recv().is_ok());
    state.disconnect_output(OutputId(0)).unwrap();
    display.flush_clients().unwrap();
    removed_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| {
        requested_rx.try_recv().is_ok()
    });
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    assert_eq!(state.windows[0].initial_mode, None);
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

        let exclusive_surface = compositor.create_surface(&queue, ());
        let exclusive_layer = shell.get_layer_surface(
            &exclusive_surface,
            None,
            Layer::Overlay,
            "exclusive-test".into(),
            &queue,
            (),
        );
        exclusive_layer.set_size(1, 1);
        exclusive_layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        exclusive_surface.commit();

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
    assert_eq!(
        state
            .layer_keyboard_target(popup.wl_surface())
            .map(|(layer, _, interactivity)| (layer, interactivity)),
        Some((
            state.layers[0].id,
            smithay::wayland::shell::wlr_layer::KeyboardInteractivity::OnDemand,
        ))
    );
    state.on_demand_layer_focus = Some(state.layers[0].id);
    state.sync_keyboard_focus();
    let layer_surface = state.layers[0].surface.wl_surface().clone();
    assert_eq!(state.keyboard.current_focus(), Some(layer_surface.clone()));
    state.sync_keyboard_focus();
    assert_eq!(state.keyboard.current_focus(), Some(layer_surface));
    assert_eq!(state.layers.len(), 2);
    state.layers[1].mapped = true;
    state.on_demand_layer_focus = Some(state.layers[0].id);
    state.sync_keyboard_focus();
    assert!(state.exclusive_layer_has_keyboard_focus());
    assert_eq!(
        state.keyboard.current_focus(),
        Some(state.layers[1].surface.wl_surface().clone())
    );
    state.key_repeat.register(
        smithay::backend::input::Keycode::new(30),
        BindingModifiers::default(),
        astera_config::Action::Quit,
        0,
        state.clock.now(),
    );
    state.process_key_repeats();
    assert!(state.key_repeat.deadline().is_none());
    state.layers[1].mapped = false;
    state.sync_keyboard_focus();
    assert!(!state.exclusive_layer_has_keyboard_focus());
    assert_eq!(
        state.keyboard.current_focus(),
        Some(state.layers[0].surface.wl_surface().clone())
    );
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
fn popup_grab_with_unissued_serial_is_immediately_dismissed() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (requested_tx, requested_rx) = mpsc::sync_channel(0);
    let (dismissed_tx, dismissed_rx) = mpsc::channel();

    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();

        let parent_surface = compositor.create_surface(&queue, ());
        let parent_xdg = shell.get_xdg_surface(&parent_surface, &queue, ());
        let _parent = parent_xdg.get_toplevel(&queue, ());
        parent_surface.commit();

        let positioner = shell.create_positioner(&queue, ());
        positioner.set_size(64, 32);
        positioner.set_anchor_rect(0, 0, 1, 1);
        let popup_surface = compositor.create_surface(&queue, ());
        let popup_xdg = shell.get_xdg_surface(&popup_surface, &queue, ());
        let _popup = popup_xdg.get_popup(Some(&parent_xdg), &positioner, &queue, dismissed_tx);
        _popup.grab(&seat, 0xfeed_beef);
        connection.flush().unwrap();
        requested_tx.send(()).unwrap();

        for _ in 0..10_000 {
            events.blocking_dispatch(&mut TestClient).unwrap();
            if dismissed_rx.try_recv().is_ok() {
                return;
            }
        }
        panic!("compositor did not dismiss popup with an unissued grab serial");
    });

    dispatch_until(&mut display, &mut state, |_| {
        requested_rx.try_recv().is_ok()
    });
    while !client.is_finished() {
        display.dispatch_clients(&mut state).unwrap();
        display.flush_clients().unwrap();
        thread::yield_now();
    }
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
fn toplevel_buffer_before_ack_configure_is_rejected() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let (result_tx, result_rx) = mpsc::sync_channel(0);

    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let shm = globals.bind::<WlShm, _, _>(&queue, 1..=1, ()).unwrap();
        let surface = compositor.create_surface(&queue, ());
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        let fd = rustix::fs::memfd_create(
            "astera-unconfigured-toplevel-test",
            rustix::fs::MemfdFlags::CLOEXEC,
        )
        .unwrap();
        rustix::fs::ftruncate(&fd, 4).unwrap();
        let pool = shm.create_pool(fd.as_fd(), 4, &queue, ());
        let buffer = pool.create_buffer(
            0,
            1,
            1,
            4,
            wayland_client::protocol::wl_shm::Format::Argb8888,
            &queue,
            (),
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();
        result_tx
            .send(events.roundtrip(&mut TestClient).is_err())
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        committed_rx.try_recv().is_ok()
    });
    let mut rejected = None;
    dispatch_until(&mut display, &mut state, |_| match result_rx.try_recv() {
        Ok(value) => {
            rejected = Some(value);
            true
        }
        Err(_) => false,
    });
    assert_eq!(rejected, Some(true));
    assert!(state.windows.iter().all(|window| !window.mapped));
    client.join().unwrap();
}

#[test]
fn popup_buffer_before_ack_configure_is_rejected() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let (result_tx, result_rx) = mpsc::sync_channel(0);

    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let shm = globals.bind::<WlShm, _, _>(&queue, 1..=1, ()).unwrap();
        let parent_surface = compositor.create_surface(&queue, ());
        let parent_xdg = shell.get_xdg_surface(&parent_surface, &queue, ());
        let _parent_toplevel = parent_xdg.get_toplevel(&queue, ());
        let positioner = shell.create_positioner(&queue, ());
        positioner.set_size(1, 1);
        positioner.set_anchor_rect(0, 0, 1, 1);
        let popup_surface = compositor.create_surface(&queue, ());
        let popup_xdg = shell.get_xdg_surface(&popup_surface, &queue, ());
        let _popup = popup_xdg.get_popup(Some(&parent_xdg), &positioner, &queue, mpsc::channel().0);
        attach_one_pixel_buffer(
            &shm,
            &popup_surface,
            &queue,
            "astera-unconfigured-popup-test",
        );
        popup_surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();
        result_tx
            .send(events.roundtrip(&mut TestClient).is_err())
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        committed_rx.try_recv().is_ok()
    });
    let mut rejected = None;
    dispatch_until(&mut display, &mut state, |_| match result_rx.try_recv() {
        Ok(value) => {
            rejected = Some(value);
            true
        }
        Err(_) => false,
    });
    assert_eq!(rejected, Some(true));
    client.join().unwrap();
}

#[test]
fn layer_buffer_before_ack_configure_is_rejected() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let (result_tx, result_rx) = mpsc::sync_channel(0);

    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let layer_shell = globals
            .bind::<ZwlrLayerShellV1, _, _>(&queue, 1..=4, ())
            .unwrap();
        let shm = globals.bind::<WlShm, _, _>(&queue, 1..=1, ()).unwrap();
        let surface = compositor.create_surface(&queue, ());
        let layer = layer_shell.get_layer_surface(
            &surface,
            None,
            wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer::Top,
            "unconfigured-test".into(),
            &queue,
            (),
        );
        layer.set_size(1, 1);
        attach_one_pixel_buffer(&shm, &surface, &queue, "astera-unconfigured-layer-test");
        surface.commit();
        connection.flush().unwrap();
        committed_tx.send(()).unwrap();
        result_tx
            .send(events.roundtrip(&mut TestClient).is_err())
            .unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| {
        committed_rx.try_recv().is_ok()
    });
    let mut rejected = None;
    dispatch_until(&mut display, &mut state, |_| match result_rx.try_recv() {
        Ok(value) => {
            rejected = Some(value);
            true
        }
        Err(_) => false,
    });
    assert_eq!(rejected, Some(true));
    assert!(state.layers.iter().all(|layer| !layer.mapped));
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
fn unset_fullscreen_is_not_a_fullscreen_toggle() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (request_tx, request_rx) = mpsc::sync_channel(0);
    let (sent_tx, sent_rx) = mpsc::sync_channel(0);
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
        let toplevel = xdg_surface.get_toplevel(&queue, ());
        surface.commit();
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        request_rx.recv().unwrap();
        toplevel.unset_fullscreen();
        connection.flush().unwrap();
        sent_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    state.map_toplevel(0);
    let window = state.windows[0].id;
    let workspace = state.desktop.find_window(window).unwrap();
    assert_eq!(
        state
            .desktop
            .workspace(workspace)
            .unwrap()
            .window_mode(window),
        Some(WindowMode::Tiled)
    );

    request_tx.send(()).unwrap();
    dispatch_until(&mut display, &mut state, |_| sent_rx.try_recv().is_ok());
    for _ in 0..8 {
        display.dispatch_clients(&mut state).unwrap();
    }
    assert_eq!(
        state
            .desktop
            .workspace(workspace)
            .unwrap()
            .window_mode(window),
        Some(WindowMode::Tiled)
    );
    done_tx.send(()).unwrap();
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
    state.dnd_touch_icon = Some((OutputId(0), Some(7).into(), (48.0, 64.0).into()));
    let (_, location, _) = state.dnd_icon_render_source(OutputId(0)).unwrap();
    assert_eq!(location, (48, 64).into());
    state
        .connect_output(Output::new(
            OutputId(1),
            "touch-dnd-output",
            Size::new(800, 600),
        ))
        .unwrap();
    state.dnd_touch_icon = Some((OutputId(1), Some(7).into(), (20.0, 25.0).into()));
    assert!(state.dnd_icon_render_source(OutputId(0)).is_none());
    let (_, location, _) = state.dnd_icon_render_source(OutputId(1)).unwrap();
    assert_eq!(location, (20, 25).into());
    <Astera as ClientDndGrabHandler>::dropped(&mut state, None, false, seat);
    assert!(state.dnd_icon_render_source(OutputId(0)).is_none());
    assert!(state.dnd_touch_icon.is_none());

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
fn persistent_touch_grab_is_cancelled_without_active_slots() {
    assert!(touch_state_requires_cancel(true, true));
    assert!(touch_state_requires_cancel(false, false));
    assert!(!touch_state_requires_cancel(true, false));
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
fn pointer_constraint_deactivates_when_scene_rehit_test_removes_focus() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        let seat = globals.bind::<WlSeat, _, _>(&queue, 1..=9, ()).unwrap();
        let pointer = seat.get_pointer(&queue, ());
        let constraints = globals
            .bind::<ZwpPointerConstraintsV1, _, _>(&queue, 1..=1, ())
            .unwrap();
        let surface = compositor.create_surface(&queue, ());
        let _locked = constraints.lock_pointer(
            &surface,
            &pointer,
            None,
            zwp_pointer_constraints_v1::Lifetime::Persistent,
            &queue,
            (),
        );
        let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
        let _toplevel = xdg_surface.get_toplevel(&queue, ());
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    let surface = state.windows[0].surface.wl_surface().clone();
    let pointer = state.pointer.clone();
    pointer.motion(
        &mut state,
        Some((surface.clone(), (0.0, 0.0).into())),
        &MotionEvent {
            location: (1.0, 1.0).into(),
            serial: 1.into(),
            time: 1,
        },
    );
    state.pointer_focus_origin = Some((surface.clone(), (0.0, 0.0).into(), 1.0));
    smithay::wayland::pointer_constraints::with_pointer_constraint(
        &surface,
        &pointer,
        |constraint| constraint.unwrap().activate(),
    );
    assert!(matches!(
        state.active_pointer_constraint(),
        Some(pointer_constraints::ActivePointerConstraint::Locked)
    ));

    // The surface has no mapped scene node, which models a workspace switch/unmap before any
    // physical motion. Re-hit-testing must emit unlocked and clear the stale pointer focus.
    state.handle_pointer_motion((1.0, 1.0).into(), 2);
    assert!(state.active_pointer_constraint().is_none());
    assert!(pointer.current_focus().is_none());
    done_tx.send(()).unwrap();
    client.join().unwrap();
}

#[test]
fn pointer_gesture_keeps_its_begin_surface_until_cancelled() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let compositor = globals
            .bind::<WlCompositor, _, _>(&queue, 1..=6, ())
            .unwrap();
        let shell = globals.bind::<XdgWmBase, _, _>(&queue, 1..=6, ()).unwrap();
        for _ in 0..2 {
            let surface = compositor.create_surface(&queue, ());
            let xdg_surface = shell.get_xdg_surface(&surface, &queue, ());
            let _toplevel = xdg_surface.get_toplevel(&queue, ());
            surface.commit();
        }
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 2);
    let first = state.windows[0].surface.wl_surface().clone();
    let second = state.windows[1].surface.wl_surface().clone();
    let pointer = state.pointer.clone();
    pointer.motion(
        &mut state,
        Some((first.clone(), (0.0, 0.0).into())),
        &MotionEvent {
            location: (1.0, 1.0).into(),
            serial: 1.into(),
            time: 1,
        },
    );
    state.start_swipe_gesture(2, 3);
    pointer.motion(
        &mut state,
        Some((second, (0.0, 0.0).into())),
        &MotionEvent {
            location: (2.0, 2.0).into(),
            serial: 2.into(),
            time: 3,
        },
    );
    assert!(matches!(
        state.active_pointer_gesture.as_ref(),
        Some(ActivePointerGesture::Swipe(surface)) if surface == &first
    ));
    state.cancel_pointer_gesture(4);
    assert!(state.active_pointer_gesture.is_none());
    done_tx.send(()).unwrap();
    client.join().unwrap();
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
fn output_removal_reconfigures_migrated_fullscreen_window() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    state
        .connect_output(Output::new(
            OutputId(1),
            "fullscreen-fallback-output",
            Size::new(1024, 768),
        ))
        .unwrap();
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
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
        connection.flush().unwrap();
        ready_tx.send(()).unwrap();
        done_rx.recv().unwrap();
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    dispatch_until(&mut display, &mut state, |state| state.windows.len() == 1);
    state.map_toplevel(0);
    let window = state.windows[0].id;
    let workspace = state.desktop.find_window(window).unwrap();
    state
        .desktop
        .apply_window(
            workspace,
            WindowTransaction::SetMode {
                id: window,
                mode: WindowMode::Fullscreen,
                viewport_size: Size::new(1280, 720),
            },
        )
        .unwrap();
    state.configure_window_mode(window, WindowMode::Fullscreen);
    assert_eq!(
        state.windows[0]
            .surface
            .with_pending_state(|pending| pending.size),
        Some((1280, 720).into())
    );

    state.disconnect_output(OutputId(0)).unwrap();
    assert_eq!(
        state.desktop.workspace_location(workspace).unwrap().output,
        Some(OutputId(1))
    );
    assert_eq!(
        state.windows[0]
            .surface
            .with_pending_state(|pending| pending.size),
        Some((1024, 768).into())
    );
    done_tx.send(()).unwrap();
    client.join().unwrap();
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
fn xdg_output_reports_authoritative_logical_size() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (sizes_tx, sizes_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(0);
    let (stop_tx, stop_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let output = globals.bind::<WlOutput, _, _>(&queue, 1..=4, ()).unwrap();
        let manager = globals
            .bind::<ZxdgOutputManagerV1, _, _>(&queue, 1..=3, ())
            .unwrap();
        let _xdg_output = manager.get_xdg_output(&output, &queue, sizes_tx);
        events.roundtrip(&mut TestClient).unwrap();
        ready_tx.send(()).unwrap();
        loop {
            events.blocking_dispatch(&mut TestClient).unwrap();
            if stop_rx.try_recv().is_ok() {
                break;
            }
        }
    });

    dispatch_until(&mut display, &mut state, |_| ready_rx.try_recv().is_ok());
    assert_eq!(sizes_rx.recv().unwrap(), (1280, 720));

    // This logical viewport intentionally cannot be derived from mode / native scale. The
    // compositor model remains authoritative and xdg-output must report it verbatim.
    state
        .configure_output(
            OutputId(0),
            Size::new(3000, 2000),
            Size::new(1700, 1000),
            Scale120(180),
            OutputTransform::Rotate90,
        )
        .unwrap();
    display.flush_clients().unwrap();
    assert_eq!(
        sizes_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        (1700, 1000)
    );
    stop_tx.send(()).unwrap();
    // Wake the blocking client dispatch with one more valid output transaction.
    state.reflow_outputs();
    display.flush_clients().unwrap();
    client.join().unwrap();
}

#[test]
fn xdg_output_v3_remains_usable_with_wl_output_v1() {
    let mut display = Display::<Astera>::new().unwrap();
    let mut state = Astera::new(&display.handle(), Config::default());
    let (server_socket, client_socket) = UnixStream::pair().unwrap();
    display
        .handle()
        .insert_client(server_socket, Arc::new(ClientState::default()))
        .unwrap();
    let (sizes_tx, sizes_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::sync_channel(0);
    let client = thread::spawn(move || {
        let connection = Connection::from_socket(client_socket).unwrap();
        let (globals, mut events) = registry_queue_init::<TestClient>(&connection).unwrap();
        let queue = events.handle();
        let output = globals.bind::<WlOutput, _, _>(&queue, 1..=1, ()).unwrap();
        let manager = globals
            .bind::<ZxdgOutputManagerV1, _, _>(&queue, 3..=3, ())
            .unwrap();
        let _xdg_output = manager.get_xdg_output(&output, &queue, sizes_tx);
        let connected = events.roundtrip(&mut TestClient).is_ok();
        result_tx.send(connected).unwrap();
    });

    let mut connected = false;
    dispatch_until(&mut display, &mut state, |_| match result_rx.try_recv() {
        Ok(value) => {
            connected = value;
            true
        }
        Err(_) => false,
    });
    assert!(
        connected,
        "wl_output v1 must not receive the v2-only done event"
    );
    assert_eq!(sizes_rx.recv().unwrap(), (1280, 720));
    client.join().unwrap();
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
        output: OutputId(0),
        mode: WindowMode::Floating,
        kind: DragKind::Move,
        source: DragSource::Pointer,
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
