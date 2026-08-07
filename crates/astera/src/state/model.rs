use std::collections::{HashMap, HashSet};

use astera_core::{OutputId, Point, WindowId, WindowMode};
use smithay::{
    desktop::{LayerSurface, PopupManager},
    input::{Seat, SeatState, keyboard::KeyboardHandle, pointer::PointerHandle},
    output::Output as SmithayOutput,
    reexports::wayland_server::{
        DisplayHandle, backend::GlobalId, protocol::wl_surface::WlSurface,
    },
    wayland::{
        compositor::CompositorState,
        dmabuf::DmabufState,
        fractional_scale::FractionalScaleManagerState,
        idle_inhibit::IdleInhibitManagerState,
        keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState,
        output::OutputManagerState,
        pointer_gestures::PointerGesturesState,
        selection::data_device::DataDeviceState,
        shell::{
            wlr_layer::{Layer, WlrLayerShellState},
            xdg::{ToplevelSurface, XdgShellState, decoration::XdgDecorationState},
        },
        shm::ShmState,
        viewporter::ViewporterState,
        xdg_activation::XdgActivationState,
    },
};

use super::Astera;

/// Wayland protocol objects are grouped separately from desktop and interaction state. Keeping
/// this ownership boundary explicit makes protocol delegate additions independent of `Astera`'s
/// compositor policy fields.
pub struct ProtocolState {
    pub(super) display: DisplayHandle,
    pub(super) compositor_state: CompositorState,
    pub(super) xdg_shell_state: XdgShellState,
    pub(super) _xdg_decoration_state: XdgDecorationState,
    pub(super) xdg_activation_state: XdgActivationState,
    pub(super) layer_shell_state: WlrLayerShellState,
    pub(super) _fractional_scale_state: FractionalScaleManagerState,
    pub(super) _viewporter_state: ViewporterState,
    pub(super) _idle_inhibit_state: IdleInhibitManagerState,
    pub(super) keyboard_shortcuts_inhibit_state: KeyboardShortcutsInhibitState,
    pub(super) _pointer_gestures_state: PointerGesturesState,
    pub(super) _output_manager_state: OutputManagerState,
    pub(super) shm_state: ShmState,
    pub(super) seat_state: SeatState<Astera>,
    pub(super) data_device_state: DataDeviceState,
    pub(super) dmabuf_state: DmabufState,
    pub(super) popup_manager: PopupManager,
    pub(super) seat: Seat<Astera>,
    pub(super) keyboard: KeyboardHandle<Astera>,
    pub(super) pointer: PointerHandle<Astera>,
    pub(super) idle_inhibitors: HashMap<WlSurface, usize>,
}

#[derive(Clone)]
pub(super) struct MappedWindow {
    pub(super) id: WindowId,
    pub(super) surface: ToplevelSurface,
    /// Role existence and mapping are distinct: only a committed non-null buffer is mapped.
    pub(super) mapped: bool,
    /// Set when an activation was denied; cleared once the window legitimately receives focus.
    pub(super) urgent: bool,
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
    pub(super) id: u64,
    pub(super) surface: LayerSurface,
    pub(super) layer: Layer,
    pub(super) output: OutputId,
    pub(super) mapped: bool,
}

#[derive(Debug)]
pub(super) struct OutputRuntime {
    /// Smithay protocol object corresponding to the core model's stable OutputId.
    pub(super) wayland: SmithayOutput,
    pub(super) global: GlobalId,
    /// Surfaces that received wl_surface.enter for this output during the last refresh.
    pub(super) entered_surfaces: HashSet<WlSurface>,
    /// Surfaces confirmed visible by the most recent accepted render report.
    pub(super) presented_surfaces: HashSet<WlSurface>,
    /// Logical position in the compositor's output topology.
    pub(super) location: Point,
}
