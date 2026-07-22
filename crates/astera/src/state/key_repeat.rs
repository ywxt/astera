use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use astera_config::{Action, Modifiers};
use smithay::backend::input::Keycode;

#[derive(Clone)]
struct HeldRepeat {
    /// Physical key that owns this repeat slot. Keycodes remain stable if the layout changes.
    keycode: Keycode,
    /// Exact non-locking modifiers required to keep this repeat alive.
    modifiers: Modifiers,
    /// Cloned action so a configuration reload cannot invalidate an in-flight reference.
    action: Action,
    /// Monotonic deadline for the next execution.
    next_at: Instant,
}

#[derive(Default)]
pub(super) struct KeyRepeatState {
    /// Keys whose press was consumed by the compositor.
    ///
    /// Their matching releases must also be consumed; otherwise a client could receive a
    /// release without ever seeing the corresponding press.
    intercepted: BTreeSet<Keycode>,
    /// Repeat-enabled bindings that are still physically held, in activation order.
    ///
    /// Only the last entry repeats. Releasing it resumes the previously held binding, which
    /// matches the single-key repeat behavior users expect from a keyboard.
    held: Vec<HeldRepeat>,
}

impl KeyRepeatState {
    pub(super) fn intercept(&mut self, keycode: Keycode) {
        // Record this before returning Intercept from Smithay's keyboard filter.
        self.intercepted.insert(keycode);
    }

    pub(super) fn release(&mut self, keycode: Keycode, rate: u32) -> bool {
        let intercepted = self.intercepted.remove(&keycode);
        // Remember whether removing this key exposes an older repeat candidate.
        let was_active = self.held.last().is_some_and(|held| held.keycode == keycode);
        self.held.retain(|held| held.keycode != keycode);
        if was_active {
            if let Some(previous) = self.held.last_mut() {
                // Resume after a full interval instead of firing the older action immediately.
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
        // A second press for the same key replaces stale state instead of creating two timers.
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
        // Modifier changes cancel a repeat even while the trigger key remains physically held.
        self.held
            .retain(|held| held.modifiers == modifiers && self.intercepted.contains(&held.keycode));
        let held = self.held.last_mut()?;
        if now < held.next_at {
            return None;
        }
        // Schedule from `now` so a delayed compositor frame cannot cause a burst of actions.
        held.next_at = now + repeat_interval(rate);
        Some(held.action.clone())
    }

    pub(super) fn cancel_repeats(&mut self) {
        // Intercepted keys are intentionally retained so their later releases remain balanced.
        self.held.clear();
    }
}

fn repeat_interval(rate: u32) -> Duration {
    Duration::from_secs_f64(1.0 / rate as f64)
}
