use smithay::reexports::{
    wayland_protocols::wp::color_representation::v1::server::{
        wp_color_representation_manager_v1::{self, WpColorRepresentationManagerV1},
        wp_color_representation_surface_v1::{self, WpColorRepresentationSurfaceV1},
    },
    wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
        backend::{ClientId, GlobalId},
        protocol::wl_surface::WlSurface,
    },
};

use super::Astera;

#[derive(Debug)]
pub(super) struct ColorRepresentationState {
    _global: GlobalId,
}

#[derive(Debug)]
pub(super) struct ColorRepresentationSurfaceData {
    surface: WlSurface,
}

impl ColorRepresentationState {
    pub(super) fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<Astera, WpColorRepresentationManagerV1, _>(1, ()),
        }
    }
}

impl GlobalDispatch<WpColorRepresentationManagerV1, ()> for Astera {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<WpColorRepresentationManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        manager.supported_alpha_mode(
            wp_color_representation_surface_v1::AlphaMode::PremultipliedElectrical,
        );
        // Matrix/range and chroma-location metadata are deliberately not advertised: Astera's
        // generic GLES import path cannot currently override YCbCr sampling metadata.
        manager.done();
    }
}

impl Dispatch<WpColorRepresentationManagerV1, ()> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &WpColorRepresentationManagerV1,
        request: wp_color_representation_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wp_color_representation_manager_v1::Request::GetSurface { id, surface } => {
                let representation = data_init.init(
                    id,
                    ColorRepresentationSurfaceData {
                        surface: surface.clone(),
                    },
                );
                if state.color_representations.contains_key(&surface) {
                    manager.post_error(
                        wp_color_representation_manager_v1::Error::SurfaceExists,
                        "surface already has a color-representation object",
                    );
                    return;
                }
                state.color_representations.insert(surface, representation);
            }
            wp_color_representation_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<WpColorRepresentationSurfaceV1, ColorRepresentationSurfaceData> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &WpColorRepresentationSurfaceV1,
        request: wp_color_representation_surface_v1::Request,
        data: &ColorRepresentationSurfaceData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let active = state.color_representations.get(&data.surface) == Some(resource);
        match request {
            wp_color_representation_surface_v1::Request::Destroy => {
                if active {
                    state.color_representations.remove(&data.surface);
                    state.pending_color_alpha.insert(data.surface.clone(), None);
                }
            }
            wp_color_representation_surface_v1::Request::SetAlphaMode { alpha_mode } => {
                if !active {
                    resource.post_error(
                        wp_color_representation_surface_v1::Error::Inert,
                        "associated wl_surface has been destroyed",
                    );
                    return;
                }
                match alpha_mode {
                    WEnum::Value(
                        wp_color_representation_surface_v1::AlphaMode::PremultipliedElectrical,
                    ) => {
                        state
                            .pending_color_alpha
                            .insert(data.surface.clone(), Some(()));
                    }
                    WEnum::Unknown(_) | WEnum::Value(_) => resource.post_error(
                        wp_color_representation_surface_v1::Error::AlphaMode,
                        "alpha mode is not supported",
                    ),
                }
            }
            wp_color_representation_surface_v1::Request::SetCoefficientsAndRange { .. } => {
                if !active {
                    resource.post_error(
                        wp_color_representation_surface_v1::Error::Inert,
                        "associated wl_surface has been destroyed",
                    );
                } else {
                    resource.post_error(
                        wp_color_representation_surface_v1::Error::Coefficients,
                        "matrix coefficients and ranges are not supported",
                    );
                }
            }
            wp_color_representation_surface_v1::Request::SetChromaLocation { .. } => {
                if !active {
                    resource.post_error(
                        wp_color_representation_surface_v1::Error::Inert,
                        "associated wl_surface has been destroyed",
                    );
                } else {
                    resource.post_error(
                        wp_color_representation_surface_v1::Error::ChromaLocation,
                        "chroma locations are not supported",
                    );
                }
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &WpColorRepresentationSurfaceV1,
        data: &ColorRepresentationSurfaceData,
    ) {
        if state.color_representations.get(&data.surface) == Some(resource) {
            state.color_representations.remove(&data.surface);
            state.pending_color_alpha.insert(data.surface.clone(), None);
        }
    }
}

impl Astera {
    pub(super) fn apply_pending_color_representation(&mut self, surface: &WlSurface) {
        let Some(alpha) = self.pending_color_alpha.remove(surface) else {
            return;
        };
        if alpha.is_some() {
            self.electrical_alpha_surfaces.insert(surface.clone());
        } else {
            self.electrical_alpha_surfaces.remove(surface);
        }
    }

    pub(super) fn remove_color_representation_surface(&mut self, surface: &WlSurface) {
        self.color_representations.remove(surface);
        self.pending_color_alpha.remove(surface);
        self.electrical_alpha_surfaces.remove(surface);
    }
}
