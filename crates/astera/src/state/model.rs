use std::collections::{HashMap, HashSet};

use astera_core::{OutputId, Point, Rect, Size, WindowId, WindowMode};
use smithay::{
    backend::input::TouchSlot,
    desktop::{LayerSurface, PopupManager},
    input::{
        Seat, SeatState, keyboard::KeyboardHandle, pointer::PointerHandle, touch::TouchHandle,
    },
    output::Output as SmithayOutput,
    reexports::wayland_server::{
        DisplayHandle, backend::GlobalId, protocol::wl_surface::WlSurface,
    },
    wayland::{
        alpha_modifier::AlphaModifierState,
        commit_timing::CommitTimingManagerState,
        compositor::CompositorState,
        content_type::ContentTypeState,
        cursor_shape::CursorShapeManagerState,
        dmabuf::DmabufState,
        fifo::FifoManagerState,
        foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelListState},
        fractional_scale::FractionalScaleManagerState,
        idle_inhibit::IdleInhibitManagerState,
        input_method::{InputMethodManagerState, PopupSurface as InputMethodPopupSurface},
        keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitState,
        output::OutputManagerState,
        pointer_gestures::PointerGesturesState,
        presentation::PresentationState,
        security_context::SecurityContextState,
        selection::{
            data_device::DataDeviceState,
            ext_data_control::DataControlState as ExtDataControlState,
            primary_selection::PrimarySelectionState,
        },
        shell::{
            wlr_layer::{Layer, WlrLayerShellState},
            xdg::{
                ToplevelSurface, XdgShellState, decoration::XdgDecorationState,
                dialog::XdgDialogState,
            },
        },
        shm::ShmState,
        single_pixel_buffer::SinglePixelBufferState,
        tablet_manager::TabletManagerState,
        text_input::TextInputManagerState,
        viewporter::ViewporterState,
        virtual_keyboard::VirtualKeyboardManagerState,
        xdg_activation::XdgActivationState,
        xdg_system_bell::XdgSystemBellState,
    },
};

use super::{
    Astera, color_representation::ColorRepresentationState, ext_workspace::ExtWorkspaceState,
    tearing_control::TearingControlState, transient_seat::TransientSeatState,
    xdg_foreign::XdgForeignState, xdg_toplevel_drag::XdgToplevelDragState,
    xdg_toplevel_icon::XdgToplevelIconState, xdg_toplevel_tag::XdgToplevelTagState,
};

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
    pub(super) _tablet_manager_state: TabletManagerState,
    pub(super) _cursor_shape_state: CursorShapeManagerState,
    pub(super) _text_input_state: TextInputManagerState,
    pub(super) _input_method_state: InputMethodManagerState,
    pub(super) _virtual_keyboard_state: VirtualKeyboardManagerState,
    pub(super) _output_manager_state: OutputManagerState,
    pub(super) shm_state: ShmState,
    pub(super) seat_state: SeatState<Astera>,
    pub(super) data_device_state: DataDeviceState,
    pub(super) primary_selection_state: PrimarySelectionState,
    pub(super) ext_data_control_state: ExtDataControlState,
    pub(super) _single_pixel_buffer_state: SinglePixelBufferState,
    pub(super) _alpha_modifier_state: AlphaModifierState,
    pub(super) foreign_toplevel_list_state: ForeignToplevelListState,
    pub(super) _content_type_state: ContentTypeState,
    pub(super) _xdg_dialog_state: XdgDialogState,
    pub(super) _fifo_manager_state: FifoManagerState,
    pub(super) _commit_timing_manager_state: CommitTimingManagerState,
    pub(super) _security_context_state: SecurityContextState,
    pub(super) _presentation_state: PresentationState,
    pub(super) xdg_foreign_state: XdgForeignState,
    pub(super) _xdg_system_bell_state: XdgSystemBellState,
    pub(super) _xdg_toplevel_tag_state: XdgToplevelTagState,
    pub(super) _xdg_toplevel_icon_state: XdgToplevelIconState,
    pub(super) _xdg_toplevel_drag_state: XdgToplevelDragState,
    pub(super) _tearing_control_state: TearingControlState,
    pub(super) _ext_workspace_state: ExtWorkspaceState,
    pub(super) _transient_seat_state: TransientSeatState,
    pub(super) _color_representation_state: ColorRepresentationState,
    pub(super) dmabuf_state: DmabufState,
    pub(super) popup_manager: PopupManager,
    pub(super) seat: Seat<Astera>,
    pub(super) keyboard: KeyboardHandle<Astera>,
    pub(super) pointer: PointerHandle<Astera>,
    pub(super) touch: TouchHandle<Astera>,
    pub(super) idle_inhibitors: HashMap<WlSurface, usize>,
}

#[derive(Clone)]
pub(super) struct MappedWindow {
    pub(super) id: WindowId,
    pub(super) surface: ToplevelSurface,
    /// Role existence and mapping are distinct: only a committed non-null buffer is mapped.
    pub(super) mapped: bool,
    /// Mode requested before the first non-null buffer commit. XDG permits clients to request
    /// maximize/fullscreen before the surface has entered the desktop model.
    pub(super) initial_mode: Option<WindowMode>,
    /// Set when an activation was denied; cleared once the window legitimately receives focus.
    pub(super) urgent: bool,
    pub(super) tag: Option<String>,
    pub(super) description: Option<String>,
    pub(super) icon_name: Option<String>,
    pub(super) icon_buffers: Vec<(i32, i32)>,
    pub(super) foreign_toplevel: ForeignToplevelHandle,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DragState {
    pub(super) window: WindowId,
    pub(super) output: OutputId,
    pub(super) mode: WindowMode,
    pub(super) kind: DragKind,
    pub(super) source: DragSource,
    /// Pointer-to-window offset captured at grab time to prevent the window from jumping.
    pub(super) grab_offset: (f64, f64),
    /// Latest candidate position; tiled geometry is committed only when the grab ends.
    pub(super) pointer_start: (f64, f64),
    pub(super) min_size: Size,
    pub(super) max_size: Size,
    pub(super) target: Rect,
    /// Original geometry used for resize and to seed the radial solver.
    pub(super) start: Rect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DragSource {
    Pointer,
    Touch(TouchSlot),
    Dnd,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum DragKind {
    Move,
    Resize(ResizeEdges),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ResizeEdges {
    pub(super) top: bool,
    pub(super) bottom: bool,
    pub(super) left: bool,
    pub(super) right: bool,
}

#[derive(Clone, Debug)]
pub(super) struct MappedLayer {
    pub(super) id: u64,
    pub(super) surface: LayerSurface,
    pub(super) layer: Layer,
    pub(super) output: OutputId,
    pub(super) mapped: bool,
}

#[derive(Clone, Debug)]
pub(super) struct MappedInputMethodPopup {
    pub(super) surface: InputMethodPopupSurface,
}

#[derive(Clone, Debug)]
pub(super) enum ActivePointerGesture {
    Swipe(WlSurface),
    Pinch(WlSurface),
    Hold(WlSurface),
}

impl ActivePointerGesture {
    pub(super) fn surface(&self) -> &WlSurface {
        match self {
            Self::Swipe(surface) | Self::Pinch(surface) | Self::Hold(surface) => surface,
        }
    }
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
