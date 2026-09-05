use smithay::{
    desktop::{WindowSurfaceType, utils::under_from_surface_tree},
    input::pointer::PointerHandle,
    reexports::{
        wayland_protocols::wp::pointer_warp::v1::server::wp_pointer_warp_v1::{
            self, WpPointerWarpV1,
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, backend::GlobalId,
        },
    },
};

use super::Astera;

#[derive(Debug)]
pub(super) struct PointerWarpState {
    _global: GlobalId,
}

impl PointerWarpState {
    pub(super) fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<Astera, WpPointerWarpV1, _>(1, ()),
        }
    }
}

impl GlobalDispatch<WpPointerWarpV1, ()> for Astera {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<WpPointerWarpV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WpPointerWarpV1, ()> for Astera {
    fn request(
        state: &mut Self,
        client: &Client,
        _resource: &WpPointerWarpV1,
        request: wp_pointer_warp_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wp_pointer_warp_v1::Request::WarpPointer {
                surface,
                pointer,
                x,
                y,
                serial,
            } => {
                let Some(request_pointer) = PointerHandle::<Astera>::from_resource(&pointer) else {
                    return;
                };
                if request_pointer != state.pointer
                    || state.pointer.current_focus().as_ref() != Some(&surface)
                    || state.pointer_enter_serial != Some((client.id(), serial.into()))
                {
                    return;
                }
                let Some((focused, origin, scale)) = state.pointer_focus_origin.clone() else {
                    return;
                };
                if focused != surface || !x.is_finite() || !y.is_finite() {
                    return;
                }
                let local = (x, y).into();
                if under_from_surface_tree(&surface, local, (0, 0), WindowSurfaceType::TOPLEVEL)
                    .is_none()
                {
                    return;
                }
                let target = (origin.x + x * scale, origin.y + y * scale).into();
                state.handle_absolute_pointer_motion(target, 0);
            }
            wp_pointer_warp_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}
