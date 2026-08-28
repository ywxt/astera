use smithay::reexports::{
    wayland_protocols::ext::transient_seat::v1::server::{
        ext_transient_seat_manager_v1::{self, ExtTransientSeatManagerV1},
        ext_transient_seat_v1::{self, ExtTransientSeatV1},
    },
    wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
        backend::{ClientId, GlobalId},
    },
};

use super::{Astera, ClientState};

const MAX_TRANSIENT_SEATS: usize = 16;

#[derive(Debug)]
pub(super) struct TransientSeatState {
    _global: GlobalId,
}

#[derive(Debug)]
pub(super) struct TransientSeatRuntime {
    pub(super) protocol: ExtTransientSeatV1,
    pub(super) global: GlobalId,
}

impl TransientSeatState {
    pub(super) fn new(display: &DisplayHandle) -> Self {
        let global = display.create_global::<Astera, ExtTransientSeatManagerV1, ()>(1, ());
        Self { _global: global }
    }
}

impl GlobalDispatch<ExtTransientSeatManagerV1, ()> for Astera {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ExtTransientSeatManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _data: &()) -> bool {
        client
            .get_data::<ClientState>()
            .is_some_and(|state| state.trusted_input)
    }
}

impl Dispatch<ExtTransientSeatManagerV1, ()> for Astera {
    fn request(
        state: &mut Self,
        client: &Client,
        _manager: &ExtTransientSeatManagerV1,
        request: ext_transient_seat_manager_v1::Request,
        _data: &(),
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_transient_seat_manager_v1::Request::Create { seat } => {
                let protocol = data_init.init(seat, ());
                if state.transient_seats.len() >= MAX_TRANSIENT_SEATS {
                    protocol.denied();
                    return;
                }
                let name = format!("astera-transient-{}", state.next_transient_seat);
                state.next_transient_seat = state.next_transient_seat.wrapping_add(1);
                let seat = state.seat_state.new_wl_seat(display, name);
                let global = seat.global().expect("new wl_seat has a global");
                let Some(global_name) = client.global_name(display, global.clone()) else {
                    display.remove_global::<Self>(global);
                    protocol.denied();
                    return;
                };
                protocol.ready(global_name);
                state
                    .transient_seats
                    .push(TransientSeatRuntime { protocol, global });
            }
            ext_transient_seat_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ExtTransientSeatV1, ()> for Astera {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtTransientSeatV1,
        request: ext_transient_seat_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_transient_seat_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &ExtTransientSeatV1, _data: &()) {
        if let Some(index) = state
            .transient_seats
            .iter()
            .position(|runtime| runtime.protocol == *resource)
        {
            let runtime = state.transient_seats.swap_remove(index);
            state.display.remove_global::<Self>(runtime.global);
        }
    }
}
