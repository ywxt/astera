use std::sync::Mutex;

use smithay::{
    reexports::{
        wayland_protocols::xdg::toplevel_icon::v1::server::{
            xdg_toplevel_icon_manager_v1::{self, XdgToplevelIconManagerV1},
            xdg_toplevel_icon_v1::{self, XdgToplevelIconV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::{ClientId, GlobalId},
            protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
        },
    },
    wayland::shm::with_buffer_contents,
};

use super::Astera;

#[derive(Debug)]
pub(super) struct XdgToplevelIconState {
    _global: GlobalId,
}

#[derive(Clone, Debug, Default)]
pub(super) struct IconSnapshot {
    pub(super) name: Option<String>,
    pub(super) buffers: Vec<(i32, i32)>,
}

#[derive(Debug, Default)]
struct IconBuilder {
    immutable: bool,
    name: Option<String>,
    buffers: Vec<IconBuffer>,
}

#[derive(Debug)]
struct IconBuffer {
    resource: WlBuffer,
    size: i32,
    scale: i32,
}

#[derive(Debug, Default)]
pub(super) struct IconData(Mutex<IconBuilder>);

impl XdgToplevelIconState {
    pub(super) fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<Astera, XdgToplevelIconManagerV1, _>(1, ()),
        }
    }
}

impl GlobalDispatch<XdgToplevelIconManagerV1, ()> for Astera {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<XdgToplevelIconManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        manager.icon_size(64);
        manager.icon_size(128);
        manager.done();
    }
}

impl Dispatch<XdgToplevelIconManagerV1, ()> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &XdgToplevelIconManagerV1,
        request: xdg_toplevel_icon_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel_icon_manager_v1::Request::CreateIcon { id } => {
                let icon = data_init.init(id, IconData::default());
                state.toplevel_icon_resources.push(icon);
            }
            xdg_toplevel_icon_manager_v1::Request::SetIcon { toplevel, icon } => {
                let Some(surface) = state
                    .xdg_shell_state
                    .get_toplevel(&toplevel)
                    .map(|surface| surface.wl_surface().clone())
                else {
                    return;
                };
                let icon = icon.and_then(|icon| {
                    let data = icon.data::<IconData>()?;
                    let mut builder = data.0.lock().unwrap();
                    builder.immutable = true;
                    (builder.name.is_some() || !builder.buffers.is_empty()).then(|| IconSnapshot {
                        name: builder.name.clone(),
                        buffers: builder
                            .buffers
                            .iter()
                            .map(|buffer| (buffer.size, buffer.scale))
                            .collect(),
                    })
                });
                state.pending_toplevel_icons.insert(surface, icon);
            }
            xdg_toplevel_icon_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<XdgToplevelIconV1, IconData> for Astera {
    fn request(
        _state: &mut Self,
        _client: &Client,
        icon: &XdgToplevelIconV1,
        request: xdg_toplevel_icon_v1::Request,
        data: &IconData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel_icon_v1::Request::SetName { icon_name } => {
                let mut builder = data.0.lock().unwrap();
                if builder.immutable {
                    icon.post_error(
                        xdg_toplevel_icon_v1::Error::Immutable,
                        "icon cannot be changed after set_icon",
                    );
                    return;
                }
                builder.name = Some(icon_name);
            }
            xdg_toplevel_icon_v1::Request::AddBuffer { buffer, scale } => {
                let mut builder = data.0.lock().unwrap();
                if builder.immutable {
                    icon.post_error(
                        xdg_toplevel_icon_v1::Error::Immutable,
                        "icon cannot be changed after set_icon",
                    );
                    return;
                }
                let Ok(metadata) = with_buffer_contents(&buffer, |_, _, metadata| metadata) else {
                    icon.post_error(
                        xdg_toplevel_icon_v1::Error::InvalidBuffer,
                        "icon buffers must be backed by wl_shm",
                    );
                    return;
                };
                if metadata.width != metadata.height {
                    icon.post_error(
                        xdg_toplevel_icon_v1::Error::InvalidBuffer,
                        "icon buffers must be square",
                    );
                    return;
                }
                if let Some(existing) = builder
                    .buffers
                    .iter_mut()
                    .find(|existing| existing.size == metadata.width && existing.scale == scale)
                {
                    existing.resource = buffer;
                } else {
                    builder.buffers.push(IconBuffer {
                        resource: buffer,
                        size: metadata.width,
                        scale,
                    });
                }
            }
            xdg_toplevel_icon_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &XdgToplevelIconV1,
        _data: &IconData,
    ) {
        state
            .toplevel_icon_resources
            .retain(|icon| icon != resource);
    }
}

impl Astera {
    pub(super) fn apply_pending_toplevel_icon(&mut self, surface: &WlSurface) {
        let Some(icon) = self.pending_toplevel_icons.remove(surface) else {
            return;
        };
        let Some(window) = self
            .windows
            .iter_mut()
            .find(|window| window.surface.wl_surface() == surface)
        else {
            return;
        };
        let next_name = icon.as_ref().and_then(|icon| icon.name.clone());
        let next_buffers = icon
            .as_ref()
            .map(|icon| icon.buffers.clone())
            .unwrap_or_default();
        let changed = window.icon_name != next_name || window.icon_buffers != next_buffers;
        window.icon_name = next_name;
        window.icon_buffers = next_buffers;
        if changed {
            self.mark_public_dirty();
        }
    }

    pub(super) fn icon_buffer_destroyed(&mut self, buffer: &WlBuffer) {
        for icon in &self.toplevel_icon_resources {
            let Some(data) = icon.data::<IconData>() else {
                continue;
            };
            if data
                .0
                .lock()
                .unwrap()
                .buffers
                .iter()
                .any(|candidate| &candidate.resource == buffer)
            {
                icon.post_error(
                    xdg_toplevel_icon_v1::Error::NoBuffer,
                    "icon buffer was destroyed before the icon object",
                );
            }
        }
    }
}
