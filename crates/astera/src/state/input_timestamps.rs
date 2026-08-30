use smithay::{
    input::{keyboard::KeyboardHandle, pointer::PointerHandle, touch::TouchHandle},
    reexports::{
        wayland_protocols::wp::input_timestamps::zv1::server::{
            zwp_input_timestamps_manager_v1::{self, ZwpInputTimestampsManagerV1},
            zwp_input_timestamps_v1::{self, ZwpInputTimestampsV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
            backend::{ClientId, GlobalId},
            protocol::{wl_keyboard::WlKeyboard, wl_pointer::WlPointer, wl_touch::WlTouch},
        },
    },
};

use super::Astera;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InputTimestampKind {
    Keyboard,
    Pointer,
    Touch,
}

#[derive(Debug)]
enum InputTimestampTarget {
    Keyboard(WlKeyboard),
    Pointer(WlPointer),
    Touch(WlTouch),
}

impl InputTimestampTarget {
    fn kind(&self) -> InputTimestampKind {
        match self {
            Self::Keyboard(_) => InputTimestampKind::Keyboard,
            Self::Pointer(_) => InputTimestampKind::Pointer,
            Self::Touch(_) => InputTimestampKind::Touch,
        }
    }

    fn alive(&self) -> bool {
        match self {
            Self::Keyboard(resource) => resource.is_alive(),
            Self::Pointer(resource) => resource.is_alive(),
            Self::Touch(resource) => resource.is_alive(),
        }
    }

    fn client(&self) -> Option<Client> {
        match self {
            Self::Keyboard(resource) => resource.client(),
            Self::Pointer(resource) => resource.client(),
            Self::Touch(resource) => resource.client(),
        }
    }

    fn belongs_to_main_seat(
        &self,
        keyboard: &KeyboardHandle<Astera>,
        pointer: &PointerHandle<Astera>,
        touch: &TouchHandle<Astera>,
    ) -> bool {
        match self {
            Self::Keyboard(resource) => {
                KeyboardHandle::from_resource(resource).as_ref() == Some(keyboard)
            }
            Self::Pointer(resource) => {
                PointerHandle::from_resource(resource).as_ref() == Some(pointer)
            }
            Self::Touch(resource) => TouchHandle::from_resource(resource).as_ref() == Some(touch),
        }
    }
}

#[derive(Debug)]
pub(super) struct InputTimestampSubscription {
    pub(super) protocol: ZwpInputTimestampsV1,
    target: InputTimestampTarget,
}

#[derive(Debug)]
pub(super) struct InputTimestampState {
    _global: GlobalId,
}

impl InputTimestampState {
    pub(super) fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<Astera, ZwpInputTimestampsManagerV1, _>(1, ()),
        }
    }
}

impl GlobalDispatch<ZwpInputTimestampsManagerV1, ()> for Astera {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpInputTimestampsManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpInputTimestampsManagerV1, ()> for Astera {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwpInputTimestampsManagerV1,
        request: zwp_input_timestamps_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_input_timestamps_manager_v1::Request::GetKeyboardTimestamps { id, keyboard } => {
                let protocol = data_init.init(id, ());
                state
                    .input_timestamp_subscriptions
                    .push(InputTimestampSubscription {
                        protocol,
                        target: InputTimestampTarget::Keyboard(keyboard),
                    });
            }
            zwp_input_timestamps_manager_v1::Request::GetPointerTimestamps { id, pointer } => {
                let protocol = data_init.init(id, ());
                state
                    .input_timestamp_subscriptions
                    .push(InputTimestampSubscription {
                        protocol,
                        target: InputTimestampTarget::Pointer(pointer),
                    });
            }
            zwp_input_timestamps_manager_v1::Request::GetTouchTimestamps { id, touch } => {
                let protocol = data_init.init(id, ());
                state
                    .input_timestamp_subscriptions
                    .push(InputTimestampSubscription {
                        protocol,
                        target: InputTimestampTarget::Touch(touch),
                    });
            }
            zwp_input_timestamps_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwpInputTimestampsV1, ()> for Astera {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwpInputTimestampsV1,
        request: zwp_input_timestamps_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwp_input_timestamps_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &ZwpInputTimestampsV1, _data: &()) {
        state
            .input_timestamp_subscriptions
            .retain(|subscription| subscription.protocol != *resource);
    }
}

impl Astera {
    pub(super) fn send_input_timestamp(
        &mut self,
        kind: InputTimestampKind,
        client: Option<&ClientId>,
        time_usec: u64,
    ) {
        let Some(client) = client else {
            return;
        };
        let keyboard = self.keyboard.clone();
        let pointer = self.pointer.clone();
        let touch = self.touch.clone();
        self.input_timestamp_subscriptions.retain(|subscription| {
            if !subscription.protocol.is_alive() || !subscription.target.alive() {
                return false;
            }
            if subscription.target.kind() == kind
                && subscription
                    .target
                    .belongs_to_main_seat(&keyboard, &pointer, &touch)
                && subscription
                    .target
                    .client()
                    .is_some_and(|target| &target.id() == client)
            {
                let seconds = time_usec / 1_000_000;
                subscription.protocol.timestamp(
                    (seconds >> 32) as u32,
                    seconds as u32,
                    ((time_usec % 1_000_000) * 1_000) as u32,
                );
            }
            true
        });
    }
}
