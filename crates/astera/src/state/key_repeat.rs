use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use astera_config::{Action, Modifiers};
use smithay::backend::input::Keycode;

#[derive(Clone)]
struct HeldRepeat {
    keycode: Keycode,
    modifiers: Modifiers,
    action: Action,
    next_at: Instant,
}

#[derive(Default)]
pub(super) struct KeyRepeatState {
    intercepted: BTreeSet<Keycode>,
    held: Vec<HeldRepeat>,
}

impl KeyRepeatState {
    pub(super) fn intercept(&mut self, keycode: Keycode) {
        self.intercepted.insert(keycode);
    }

    pub(super) fn release(&mut self, keycode: Keycode, rate: u32) -> bool {
        let intercepted = self.intercepted.remove(&keycode);
        let was_active = self.held.last().is_some_and(|held| held.keycode == keycode);
        self.held.retain(|held| held.keycode != keycode);
        if was_active {
            if let Some(previous) = self.held.last_mut() {
                previous.next_at = Instant::now() + repeat_interval(rate);
            }
        }
        intercepted
    }

    pub(super) fn register(
        &mut self,
        keycode: Keycode,
        modifiers: Modifiers,
        action: Action,
        delay_ms: u64,
    ) {
        self.held.retain(|held| held.keycode != keycode);
        self.held.push(HeldRepeat {
            keycode,
            modifiers,
            action,
            next_at: Instant::now() + Duration::from_millis(delay_ms),
        });
    }

    pub(super) fn next_action(
        &mut self,
        now: Instant,
        modifiers: Modifiers,
        rate: u32,
    ) -> Option<Action> {
        self.held
            .retain(|held| held.modifiers == modifiers && self.intercepted.contains(&held.keycode));
        let held = self.held.last_mut()?;
        if now < held.next_at {
            return None;
        }
        held.next_at = now + repeat_interval(rate);
        Some(held.action.clone())
    }

    pub(super) fn cancel_repeats(&mut self) {
        self.held.clear();
    }
}

fn repeat_interval(rate: u32) -> Duration {
    Duration::from_secs_f64(1.0 / rate as f64)
}
