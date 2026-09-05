use std::collections::HashMap;

use rand::distr::{Alphanumeric, SampleString};
use smithay::{
    reexports::{
        wayland_protocols::xdg::foreign::zv2::server::{
            zxdg_exported_v2::{self, ZxdgExportedV2},
            zxdg_exporter_v2::{self, ZxdgExporterV2},
            zxdg_imported_v2::{self, ZxdgImportedV2},
            zxdg_importer_v2::{self, ZxdgImporterV2},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::{ClientId, GlobalId},
            protocol::wl_surface::WlSurface,
        },
    },
    wayland::{
        compositor::with_states,
        shell::{
            is_toplevel_equivalent, is_valid_parent,
            xdg::{XdgShellHandler, XdgToplevelSurfaceData},
        },
    },
};

use super::Astera;

#[derive(Debug)]
pub(super) struct XdgForeignState {
    exported: HashMap<String, Exported>,
    _exporter: GlobalId,
    _importer: GlobalId,
}

#[derive(Debug)]
struct Exported {
    surface: WlSurface,
    imports: HashMap<ZxdgImportedV2, Option<WlSurface>>,
}

#[derive(Debug)]
pub(super) struct ExportedData {
    handle: String,
}

#[derive(Debug)]
pub(super) struct ImportedData {
    handle: String,
}

impl XdgForeignState {
    pub(super) fn new(display: &DisplayHandle) -> Self {
        Self {
            exported: HashMap::new(),
            _exporter: display.create_global::<Astera, ZxdgExporterV2, _>(1, ()),
            _importer: display.create_global::<Astera, ZxdgImporterV2, _>(1, ()),
        }
    }
}

impl GlobalDispatch<ZxdgExporterV2, ()> for Astera {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgExporterV2>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZxdgExporterV2, ()> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZxdgExporterV2,
        request: zxdg_exporter_v2::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zxdg_exporter_v2::Request::ExportToplevel { id, surface } => {
                let handle = loop {
                    let candidate = Alphanumeric.sample_string(&mut rand::rng(), 32);
                    if !state.xdg_foreign_state.exported.contains_key(&candidate) {
                        break candidate;
                    }
                };
                // export_toplevel carries a new_id. Initialize it before reporting an invalid
                // surface so wayland-server can tear down the offending client without finding
                // an uninitialized child object and panicking.
                let exported = data_init.init(
                    id,
                    ExportedData {
                        handle: handle.clone(),
                    },
                );
                if !is_toplevel_equivalent(&surface) {
                    resource.post_error(
                        zxdg_exporter_v2::Error::InvalidSurface,
                        "only xdg-toplevel equivalent surfaces can be exported",
                    );
                    return;
                }
                state.xdg_foreign_state.exported.insert(
                    handle.clone(),
                    Exported {
                        surface,
                        imports: HashMap::new(),
                    },
                );
                exported.handle(handle);
            }
            zxdg_exporter_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZxdgExportedV2, ExportedData> for Astera {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZxdgExportedV2,
        _request: zxdg_exported_v2::Request,
        _data: &ExportedData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        _resource: &ZxdgExportedV2,
        data: &ExportedData,
    ) {
        let Some(exported) = state.xdg_foreign_state.exported.remove(&data.handle) else {
            return;
        };
        for (imported, child) in exported.imports {
            if let Some(child) = child {
                clear_parent(state, &child, &exported.surface);
            }
            imported.destroyed();
        }
    }
}

impl GlobalDispatch<ZxdgImporterV2, ()> for Astera {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgImporterV2>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZxdgImporterV2, ()> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZxdgImporterV2,
        request: zxdg_importer_v2::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zxdg_importer_v2::Request::ImportToplevel { id, handle } => {
                let imported = data_init.init(
                    id,
                    ImportedData {
                        handle: handle.clone(),
                    },
                );
                if let Some(exported) = state.xdg_foreign_state.exported.get_mut(&handle) {
                    exported.imports.insert(imported, None);
                } else {
                    imported.destroyed();
                }
            }
            zxdg_importer_v2::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZxdgImportedV2, ImportedData> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZxdgImportedV2,
        request: zxdg_imported_v2::Request,
        data: &ImportedData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let zxdg_imported_v2::Request::SetParentOf { surface: child } = request else {
            return;
        };
        let Some(parent) = state
            .xdg_foreign_state
            .exported
            .get(&data.handle)
            .map(|exported| exported.surface.clone())
        else {
            return;
        };
        if !is_toplevel_equivalent(&child) || !is_valid_parent(&child, &parent) {
            resource.post_error(
                zxdg_imported_v2::Error::InvalidSurface,
                "invalid xdg-foreign parent relationship",
            );
            return;
        }

        remove_child_relationship(state, &child);
        let changed = with_states(&child, |states| {
            let Some(role) = states.data_map.get::<XdgToplevelSurfaceData>() else {
                return false;
            };
            let mut role = role.lock().unwrap();
            let changed = role.parent.as_ref() != Some(&parent);
            role.parent = Some(parent);
            changed
        });
        if let Some(exported) = state.xdg_foreign_state.exported.get_mut(&data.handle)
            && let Some(relationship) = exported.imports.get_mut(resource)
        {
            *relationship = Some(child.clone());
        }
        if changed {
            notify_parent_changed(state, &child);
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &ZxdgImportedV2,
        data: &ImportedData,
    ) {
        let Some(exported) = state.xdg_foreign_state.exported.get_mut(&data.handle) else {
            return;
        };
        let parent = exported.surface.clone();
        let child = exported.imports.remove(resource).flatten();
        if let Some(child) = child {
            clear_parent(state, &child, &parent);
        }
    }
}

pub(super) fn remove_child_relationship(state: &mut Astera, child: &WlSurface) {
    for exported in state.xdg_foreign_state.exported.values_mut() {
        for relationship in exported.imports.values_mut() {
            if relationship.as_ref() == Some(child) {
                *relationship = None;
            }
        }
    }
}

fn clear_parent(state: &mut Astera, child: &WlSurface, parent: &WlSurface) {
    let changed = with_states(child, |states| {
        let Some(role) = states.data_map.get::<XdgToplevelSurfaceData>() else {
            return false;
        };
        let mut role = role.lock().unwrap();
        if role.parent.as_ref() != Some(parent) {
            return false;
        }
        role.parent = None;
        true
    });
    if changed {
        notify_parent_changed(state, child);
    }
}

fn notify_parent_changed(state: &mut Astera, child: &WlSurface) {
    if let Some(toplevel) = state
        .xdg_shell_state
        .toplevel_surfaces()
        .iter()
        .find(|toplevel| toplevel.wl_surface() == child)
        .cloned()
    {
        <Astera as XdgShellHandler>::parent_changed(state, toplevel);
    }
}
