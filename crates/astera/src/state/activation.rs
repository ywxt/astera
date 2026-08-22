use std::{collections::VecDeque, time::Duration};

use smithay::reexports::wayland_server::backend::ClientId;
use smithay::{utils::Serial, wayland::xdg_activation::XdgActivationTokenData};

use super::{Astera, Clock};

const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REMEMBERED_INPUTS: usize = 64;

/// Input serials are capabilities: only a recent serial issued for real user input may authorize
/// an activation. Keeping a small bounded history also makes serial wraparound harmless here.
#[derive(Default)]
pub(super) struct ActivationTracker {
    inputs: VecDeque<(Serial, ClientId, std::time::Instant)>,
}

impl ActivationTracker {
    pub(super) fn remember(&mut self, serial: Serial, client: ClientId, now: std::time::Instant) {
        self.inputs.push_back((serial, client, now));
        while self.inputs.len() > MAX_REMEMBERED_INPUTS {
            self.inputs.pop_front();
        }
    }

    pub(super) fn authorize(
        &mut self,
        data: &XdgActivationTokenData,
        seat: &smithay::input::Seat<Astera>,
        clock: &dyn Clock,
    ) -> bool {
        let Some((serial, resource)) = data.serial.as_ref() else {
            return false;
        };
        let Some(client) = data.client_id.as_ref() else {
            return false;
        };
        seat.owns(resource)
            && capability_is_valid(
                &mut self.inputs,
                *serial,
                client,
                data.timestamp,
                clock.now(),
            )
    }

    pub(super) fn authorizes_input(
        &mut self,
        serial: Serial,
        client: &ClientId,
        now: std::time::Instant,
    ) -> bool {
        input_capability_is_valid(&mut self.inputs, serial, client, now)
    }
}

fn input_capability_is_valid<C: PartialEq>(
    inputs: &mut VecDeque<(Serial, C, std::time::Instant)>,
    serial: Serial,
    client: &C,
    now: std::time::Instant,
) -> bool {
    inputs.retain(|(_, _, issued)| now.saturating_duration_since(*issued) <= ACTIVATION_TIMEOUT);
    inputs
        .iter()
        .any(|(known, recipient, _)| *known == serial && recipient == client)
}

fn capability_is_valid<C: PartialEq>(
    inputs: &mut VecDeque<(Serial, C, std::time::Instant)>,
    serial: Serial,
    client: &C,
    token_timestamp: std::time::Instant,
    now: std::time::Instant,
) -> bool {
    inputs.retain(|(_, _, issued)| now.saturating_duration_since(*issued) <= ACTIVATION_TIMEOUT);
    now.saturating_duration_since(token_timestamp) <= ACTIVATION_TIMEOUT
        && inputs
            .iter()
            .any(|(known, recipient, _)| *known == serial && recipient == client)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn capability_requires_matching_client_and_serial() {
        let now = Instant::now();
        let mut inputs = VecDeque::from([(7.into(), "focused", now)]);
        assert!(capability_is_valid(
            &mut inputs,
            7.into(),
            &"focused",
            now,
            now,
        ));
        assert!(!capability_is_valid(
            &mut inputs,
            8.into(),
            &"focused",
            now,
            now,
        ));
        assert!(!capability_is_valid(
            &mut inputs,
            7.into(),
            &"attacker",
            now,
            now,
        ));
    }

    #[test]
    fn expired_token_and_input_are_rejected_and_pruned() {
        let now = Instant::now();
        let old = now - ACTIVATION_TIMEOUT - Duration::from_millis(1);
        let mut inputs = VecDeque::from([(7.into(), 1_u8, old)]);
        assert!(!capability_is_valid(&mut inputs, 7.into(), &1, old, now,));
        assert!(inputs.is_empty());
    }

    #[test]
    fn input_authorization_requires_a_recent_serial_for_the_same_client() {
        let now = Instant::now();
        let mut inputs = VecDeque::from([(11.into(), "focused", now)]);

        assert!(input_capability_is_valid(
            &mut inputs,
            11.into(),
            &"focused",
            now,
        ));
        assert!(!input_capability_is_valid(
            &mut inputs,
            12.into(),
            &"focused",
            now,
        ));
        assert!(!input_capability_is_valid(
            &mut inputs,
            11.into(),
            &"other",
            now,
        ));
        assert!(!input_capability_is_valid(
            &mut inputs,
            11.into(),
            &"focused",
            now + ACTIVATION_TIMEOUT + Duration::from_millis(1),
        ));
    }
}
