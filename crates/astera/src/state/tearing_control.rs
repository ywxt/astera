use smithay::reexports::{
    wayland_protocols::wp::tearing_control::v1::server::{
        wp_tearing_control_manager_v1::{self, WpTearingControlManagerV1},
        wp_tearing_control_v1::{self, WpTearingControlV1},
    },
    wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
        backend::{ClientId, GlobalId},
        protocol::wl_surface::WlSurface,
    },
};

use super::Astera;

#[derive(Debug)]
pub(super) struct TearingControlState {
    _global: GlobalId,
}

#[derive(Debug)]
pub(super) struct TearingControlData {
    surface: WlSurface,
}

impl TearingControlState {
    pub(super) fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<Astera, WpTearingControlManagerV1, _>(1, ()),
        }
    }
}

impl GlobalDispatch<WpTearingControlManagerV1, ()> for Astera {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<WpTearingControlManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WpTearingControlManagerV1, ()> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &WpTearingControlManagerV1,
        request: wp_tearing_control_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wp_tearing_control_manager_v1::Request::GetTearingControl { id, surface } => {
                // Initialize the new_id even on the error path. Leaving it uninitialized makes
                // wayland-server panic while it is tearing down the offending client.
                let control = data_init.init(
                    id,
                    TearingControlData {
                        surface: surface.clone(),
                    },
                );
                if state.tearing_controls.contains_key(&surface) {
                    manager.post_error(
                        wp_tearing_control_manager_v1::Error::TearingControlExists,
                        "surface already has a tearing-control object",
                    );
                    return;
                }
                state.tearing_controls.insert(surface, control);
            }
            wp_tearing_control_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<WpTearingControlV1, TearingControlData> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        control: &WpTearingControlV1,
        request: wp_tearing_control_v1::Request,
        data: &TearingControlData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wp_tearing_control_v1::Request::SetPresentationHint { hint } => {
                // Destruction of the wl_surface makes this extension inert.
                if state.tearing_controls.get(&data.surface) != Some(control) {
                    return;
                }
                let asynchronous = match hint {
                    WEnum::Value(wp_tearing_control_v1::PresentationHint::Vsync) => false,
                    WEnum::Value(wp_tearing_control_v1::PresentationHint::Async) => true,
                    WEnum::Unknown(_) | WEnum::Value(_) => return,
                };
                state
                    .pending_tearing_hints
                    .insert(data.surface.clone(), asynchronous);
            }
            wp_tearing_control_v1::Request::Destroy => {
                if state.tearing_controls.get(&data.surface) != Some(control) {
                    return;
                }
                state.tearing_controls.remove(&data.surface);
                // Destroying the extension queues the default vsync hint for the next commit.
                state
                    .pending_tearing_hints
                    .insert(data.surface.clone(), false);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &WpTearingControlV1,
        data: &TearingControlData,
    ) {
        if state.tearing_controls.get(&data.surface) == Some(resource) {
            state.tearing_controls.remove(&data.surface);
        }
    }
}

impl Astera {
    pub(super) fn apply_pending_tearing_hint(&mut self, surface: &WlSurface) {
        let Some(asynchronous) = self.pending_tearing_hints.remove(surface) else {
            return;
        };
        if asynchronous {
            self.asynchronous_surfaces.insert(surface.clone());
        } else {
            self.asynchronous_surfaces.remove(surface);
        }
    }

    pub(super) fn remove_tearing_control_surface(&mut self, surface: &WlSurface) {
        self.tearing_controls.remove(surface);
        self.pending_tearing_hints.remove(surface);
        self.asynchronous_surfaces.remove(surface);
    }
}
