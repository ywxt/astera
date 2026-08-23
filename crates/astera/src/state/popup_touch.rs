use std::collections::HashMap;

use smithay::{
    backend::input::TouchSlot,
    desktop::{PopupGrab, PopupUngrabStrategy},
    input::touch::{
        DownEvent, GrabStartData, MotionEvent, OrientationEvent, ShapeEvent, TouchDownGrab,
        TouchGrab, TouchInnerHandle, UpEvent,
    },
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    utils::{Logical, Point, Serial},
};

use super::Astera;

type TouchFocus = Option<(WlSurface, Point<f64, Logical>)>;

pub(super) struct PopupTouchGrab {
    popup: PopupGrab<Astera>,
    start: GrabStartData<Astera>,
    focuses: HashMap<TouchSlot, TouchFocus>,
}

impl PopupTouchGrab {
    pub(super) fn new(popup: PopupGrab<Astera>, start: GrabStartData<Astera>) -> Self {
        let focuses = HashMap::from([(start.slot, start.focus.clone())]);
        Self {
            popup,
            start,
            focuses,
        }
    }

    fn accepts(&self, focus: &TouchFocus) -> bool {
        let grabbed_client = self
            .popup
            .current_grab()
            .and_then(|surface| surface.client())
            .map(|client| client.id());
        focus
            .as_ref()
            .and_then(|(surface, _)| surface.client())
            .map(|client| client.id())
            == grabbed_client
            && grabbed_client.is_some()
    }
}

impl TouchGrab<Astera> for PopupTouchGrab {
    fn down(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        focus: TouchFocus,
        event: &DownEvent,
        seq: Serial,
    ) {
        if self.popup.has_ended() || !self.accepts(&focus) {
            let _ = self.popup.ungrab(PopupUngrabStrategy::All);
            handle.unset_grab(self, data);
            handle.down(data, focus.clone(), event, seq);
            handle.set_grab(
                self,
                data,
                event.serial,
                TouchDownGrab {
                    start_data: GrabStartData {
                        focus,
                        slot: event.slot,
                        location: event.location,
                    },
                    touch_points: 1,
                },
            );
            return;
        }
        self.focuses.insert(event.slot, focus.clone());
        handle.down(data, focus, event, seq);
    }

    fn up(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        event: &UpEvent,
        seq: Serial,
    ) {
        if self.popup.has_ended() {
            handle.unset_grab(self, data);
        }
        self.focuses.remove(&event.slot);
        handle.up(data, event, seq);
    }

    fn motion(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        _focus: TouchFocus,
        event: &MotionEvent,
        seq: Serial,
    ) {
        if self.popup.has_ended() {
            handle.unset_grab(self, data);
        }
        handle.motion(
            data,
            self.focuses.get(&event.slot).cloned().flatten(),
            event,
            seq,
        );
    }

    fn frame(&mut self, data: &mut Astera, handle: &mut TouchInnerHandle<'_, Astera>, seq: Serial) {
        handle.frame(data, seq);
    }

    fn cancel(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        seq: Serial,
    ) {
        handle.cancel(data, seq);
        handle.unset_grab(self, data);
    }

    fn shape(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        event: &ShapeEvent,
        seq: Serial,
    ) {
        handle.shape(data, event, seq);
    }

    fn orientation(
        &mut self,
        data: &mut Astera,
        handle: &mut TouchInnerHandle<'_, Astera>,
        event: &OrientationEvent,
        seq: Serial,
    ) {
        handle.orientation(data, event, seq);
    }

    fn start_data(&self) -> &GrabStartData<Astera> {
        &self.start
    }

    fn unset(&mut self, data: &mut Astera) {
        if data.keyboard.has_grab(self.popup.serial())
            || self
                .popup
                .previous_serial()
                .is_some_and(|serial| data.keyboard.has_grab(serial))
        {
            let keyboard = data.keyboard.clone();
            keyboard.unset_grab(data);
            let serial = data.next_serial();
            keyboard.set_focus(data, self.popup.current_grab(), serial);
        }
    }
}
