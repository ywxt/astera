use astera_core::{OutputId, Point, WindowId, WindowMode};
use smithay::{
    output::Output as SmithayOutput,
    reexports::wayland_server::{backend::GlobalId, protocol::wl_surface::WlSurface},
    wayland::shell::{
        wlr_layer::{Layer, LayerSurface},
        xdg::ToplevelSurface,
    },
};

#[derive(Clone)]
pub(super) struct MappedWindow {
    pub(super) id: WindowId,
    pub(super) surface: ToplevelSurface,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DragState {
    pub(super) window: WindowId,
    pub(super) mode: WindowMode,
    /// Pointer-to-window offset captured at grab time to prevent the window from jumping.
    pub(super) grab_offset: (f64, f64),
    /// Latest candidate position; tiled geometry is committed only when the grab ends.
    pub(super) target: Point,
    /// Original position used to seed the radial solver with a movement direction.
    pub(super) start: Point,
}

#[derive(Clone, Debug)]
pub(super) struct MappedLayer {
    pub(super) surface: LayerSurface,
    pub(super) layer: Layer,
    pub(super) output: OutputId,
}

#[derive(Debug)]
pub(super) struct OutputRuntime {
    pub(super) wayland: SmithayOutput,
    pub(super) global: GlobalId,
    /// Surfaces that received wl_surface.enter for this output during the last refresh.
    pub(super) entered_surfaces: Vec<WlSurface>,
    /// Logical position in the compositor's output topology.
    pub(super) location: Point,
}
