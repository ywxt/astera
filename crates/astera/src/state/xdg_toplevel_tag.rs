use super::Astera;
use smithay::reexports::{
    wayland_protocols::xdg::{
        shell::server::xdg_toplevel::XdgToplevel,
        toplevel_tag::v1::server::xdg_toplevel_tag_manager_v1::{self, XdgToplevelTagManagerV1},
    },
    wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, backend::GlobalId,
    },
};

#[derive(Debug)]
pub(super) struct XdgToplevelTagState {
    _global: GlobalId,
}

impl XdgToplevelTagState {
    pub(super) fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<Astera, XdgToplevelTagManagerV1, _>(1, ()),
        }
    }
}

impl GlobalDispatch<XdgToplevelTagManagerV1, ()> for Astera {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<XdgToplevelTagManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<XdgToplevelTagManagerV1, ()> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &XdgToplevelTagManagerV1,
        request: xdg_toplevel_tag_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel_tag_manager_v1::Request::SetToplevelTag { toplevel, tag } => {
                if let Some(window) = window_for_toplevel(state, &toplevel) {
                    window.tag = Some(tag);
                    state.mark_public_dirty();
                }
            }
            xdg_toplevel_tag_manager_v1::Request::SetToplevelDescription {
                toplevel,
                description,
            } => {
                if let Some(window) = window_for_toplevel(state, &toplevel) {
                    window.description = Some(description);
                    state.mark_public_dirty();
                }
            }
            xdg_toplevel_tag_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

fn window_for_toplevel<'a>(
    state: &'a mut Astera,
    toplevel: &XdgToplevel,
) -> Option<&'a mut super::model::MappedWindow> {
    let surface = state.xdg_shell_state.get_toplevel(toplevel)?;
    state
        .windows
        .iter_mut()
        .find(|window| window.surface == surface)
}
