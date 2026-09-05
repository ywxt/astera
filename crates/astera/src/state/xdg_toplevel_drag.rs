use smithay::reexports::{
    wayland_protocols::xdg::toplevel_drag::v1::server::{
        xdg_toplevel_drag_manager_v1::{self, XdgToplevelDragManagerV1},
        xdg_toplevel_drag_v1::{self, XdgToplevelDragV1},
    },
    wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        backend::{ClientId, GlobalId},
        protocol::{wl_data_source::WlDataSource, wl_surface::WlSurface},
    },
};

use super::{Astera, model::DragSource};

#[derive(Debug)]
pub(super) struct XdgToplevelDragState {
    _global: GlobalId,
}

#[derive(Clone, Debug)]
pub(super) struct AttachedToplevel {
    pub(super) surface: WlSurface,
    x_offset: i32,
    y_offset: i32,
}

#[derive(Debug)]
pub(super) struct ToplevelDragRuntime {
    resource: XdgToplevelDragV1,
    manager: XdgToplevelDragManagerV1,
    active: bool,
    pub(super) attached: Option<AttachedToplevel>,
}

#[derive(Debug)]
pub(super) struct ToplevelDragData {
    source: WlDataSource,
}

impl XdgToplevelDragState {
    pub(super) fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<Astera, XdgToplevelDragManagerV1, _>(1, ()),
        }
    }
}

impl GlobalDispatch<XdgToplevelDragManagerV1, ()> for Astera {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<XdgToplevelDragManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<XdgToplevelDragManagerV1, ()> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &XdgToplevelDragManagerV1,
        request: xdg_toplevel_drag_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel_drag_manager_v1::Request::GetXdgToplevelDrag { id, data_source } => {
                // Initialize the new_id before reporting an error so client teardown cannot make
                // wayland-server panic on an uninitialized object.
                let resource = data_init.init(
                    id,
                    ToplevelDragData {
                        source: data_source.clone(),
                    },
                );
                if state.used_selection_sources.contains(&data_source)
                    || state.used_dnd_sources.contains(&data_source)
                    || state.toplevel_drags.contains_key(&data_source)
                {
                    manager.post_error(
                        xdg_toplevel_drag_manager_v1::Error::InvalidSource,
                        "data source already has a non-toplevel-drag purpose",
                    );
                    return;
                }
                state.toplevel_drags.insert(
                    data_source,
                    ToplevelDragRuntime {
                        resource,
                        manager: manager.clone(),
                        active: false,
                        attached: None,
                    },
                );
            }
            xdg_toplevel_drag_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<XdgToplevelDragV1, ToplevelDragData> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        drag: &XdgToplevelDragV1,
        request: xdg_toplevel_drag_v1::Request,
        data: &ToplevelDragData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_toplevel_drag_v1::Request::Attach {
                toplevel,
                x_offset,
                y_offset,
            } => {
                let Some(runtime) = state.toplevel_drags.get(&data.source) else {
                    return;
                };
                if runtime.resource != *drag {
                    return;
                }
                if runtime.attached.is_some() {
                    drag.post_error(
                        xdg_toplevel_drag_v1::Error::ToplevelAttached,
                        "a toplevel is already attached to this drag",
                    );
                    return;
                }
                let Some(surface) = state
                    .xdg_shell_state
                    .get_toplevel(&toplevel)
                    .map(|surface| surface.wl_surface().clone())
                else {
                    return;
                };
                state.toplevel_drags.get_mut(&data.source).unwrap().attached =
                    Some(AttachedToplevel {
                        surface,
                        x_offset,
                        y_offset,
                    });
                state.maybe_begin_toplevel_drag(&data.source);
            }
            xdg_toplevel_drag_v1::Request::Destroy => {
                let Some(runtime) = state.toplevel_drags.get(&data.source) else {
                    return;
                };
                if runtime.resource != *drag {
                    return;
                }
                if runtime.active || !state.used_dnd_sources.contains(&data.source) {
                    drag.post_error(
                        xdg_toplevel_drag_v1::Error::OngoingDrag,
                        "toplevel drag cannot be destroyed until DnD has ended",
                    );
                    return;
                }
                state.toplevel_drags.remove(&data.source);
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &XdgToplevelDragV1,
        data: &ToplevelDragData,
    ) {
        if state
            .toplevel_drags
            .get(&data.source)
            .is_some_and(|runtime| runtime.resource == *resource)
        {
            state.remove_toplevel_drag_source(&data.source);
        }
    }
}

impl Astera {
    pub(super) fn reject_toplevel_drag_selection(&self, source: &WlDataSource) -> bool {
        let Some(runtime) = self.toplevel_drags.get(source) else {
            return false;
        };
        runtime.manager.post_error(
            xdg_toplevel_drag_manager_v1::Error::InvalidSource,
            "toplevel-drag data source cannot be used for selection",
        );
        true
    }

    pub(super) fn start_toplevel_drag(&mut self, source: &WlDataSource) {
        self.used_dnd_sources.insert(source.clone());
        let Some(runtime) = self.toplevel_drags.get_mut(source) else {
            return;
        };
        runtime.active = true;
        self.maybe_begin_toplevel_drag(source);
    }

    pub(super) fn finish_toplevel_drag(&mut self) {
        let active = self
            .toplevel_drags
            .iter()
            .find_map(|(source, runtime)| runtime.active.then(|| source.clone()));
        let Some(source) = active else {
            return;
        };
        if let Some(runtime) = self.toplevel_drags.get_mut(&source) {
            runtime.active = false;
        }
        if self.drag.is_some_and(|drag| drag.source == DragSource::Dnd) {
            self.finish_drag();
        }
    }

    pub(super) fn detach_toplevel_drag_surface(&mut self, surface: &WlSurface) {
        for runtime in self.toplevel_drags.values_mut() {
            if runtime
                .attached
                .as_ref()
                .is_some_and(|attached| &attached.surface == surface)
            {
                runtime.attached = None;
            }
        }
    }

    pub(super) fn remove_toplevel_drag_source(&mut self, source: &WlDataSource) {
        let was_active = self
            .toplevel_drags
            .remove(source)
            .is_some_and(|runtime| runtime.active);
        self.used_selection_sources.remove(source);
        self.used_dnd_sources.remove(source);
        if was_active && self.drag.is_some_and(|drag| drag.source == DragSource::Dnd) {
            self.cancel_drag();
        }
    }

    pub(super) fn maybe_begin_toplevel_drag(&mut self, source: &WlDataSource) {
        let Some(attached) = self
            .toplevel_drags
            .get(source)
            .filter(|runtime| runtime.active)
            .and_then(|runtime| runtime.attached.clone())
        else {
            return;
        };
        let Some(window) = self
            .windows
            .iter()
            .find(|window| window.mapped && window.surface.wl_surface() == &attached.surface)
            .map(|window| window.id)
        else {
            return;
        };
        if self.drag.is_some() {
            return;
        }
        self.begin_drag(Some((window, DragSource::Dnd, self.pointer_location)));
        let scale = self
            .visual_geometry(window)
            .map(|(_, _, scale, _)| scale)
            .unwrap_or(1.0);
        if let Some(drag) = self.drag.as_mut() {
            drag.grab_offset = (
                f64::from(attached.x_offset) * scale,
                f64::from(attached.y_offset) * scale,
            );
            self.update_drag(self.pointer_location);
        }
    }
}
