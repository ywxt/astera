use smithay::input::{
    Seat,
    touch::{
        DownEvent, GrabStartData, MotionEvent, OrientationEvent, ShapeEvent, TouchGrab,
        TouchHandle, TouchInnerHandle, UpEvent,
    },
};

use super::Astera;

/// Smithay's default touch grab groups every concurrent contact under the first down target.
/// Astera routes independent physical touchscreens to different outputs, so each slot must retain
/// its own focus instead. The inner touch state already tracks focus per slot; this grab forwards
/// directly to it without installing the global `TouchDownGrab`.
#[derive(Debug)]
struct IndependentTouchGrab;

impl TouchGrab<Astera> for IndependentTouchGrab {
    fn down(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        focus: Option<(
            smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
            smithay::utils::Point<f64, smithay::utils::Logical>,
        )>,
        event: &DownEvent,
        seq: smithay::utils::Serial,
    ) {
        handle.down(data, focus, event, seq);
    }

    fn up(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        event: &UpEvent,
        seq: smithay::utils::Serial,
    ) {
        handle.up(data, event, seq);
    }

    fn motion(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        focus: Option<(
            smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
            smithay::utils::Point<f64, smithay::utils::Logical>,
        )>,
        event: &MotionEvent,
        seq: smithay::utils::Serial,
    ) {
        handle.motion(data, focus, event, seq);
    }

    fn frame(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        seq: smithay::utils::Serial,
    ) {
        handle.frame(data, seq);
    }

    fn cancel(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        seq: smithay::utils::Serial,
    ) {
        handle.cancel(data, seq);
    }

    fn shape(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        event: &ShapeEvent,
        seq: smithay::utils::Serial,
    ) {
        handle.shape(data, event, seq);
    }

    fn orientation(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        event: &OrientationEvent,
        seq: smithay::utils::Serial,
    ) {
        handle.orientation(data, event, seq);
    }

    fn start_data(&self) -> &GrabStartData<Astera> {
        unreachable!("the default independent touch grab is never installed as an active grab")
    }

    fn unset(&mut self, _data: &mut Astera) {}
}

pub(super) fn add_touch(seat: &mut Seat<Astera>) -> TouchHandle<Astera> {
    seat.add_touch_with_default_grab(|| Box::new(IndependentTouchGrab))
}
