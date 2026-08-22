use smithay::input::{Seat, SeatHandler};
use smithay::reexports::{
    wayland_protocols::ext::idle_notify::v1::server::{
        ext_idle_notification_v1::{self, ExtIdleNotificationV1},
        ext_idle_notifier_v1::{self, ExtIdleNotifierV1},
    },
    wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, backend::ClientId,
    },
};
use smithay::utils::IsAlive;
use std::{
    collections::BTreeMap,
    hash::{DefaultHasher, Hash, Hasher},
    time::{Duration, Instant},
};

use super::Astera;

impl Astera {
    pub(super) fn idle_seat_key(&self) -> u64 {
        seat_key(&self.seat)
    }

    pub(super) fn refresh_idle_inhibition(&mut self) {
        use smithay::{
            desktop::utils::surface_primary_scanout_output, wayland::compositor::with_states,
        };

        self.idle_inhibitors.retain(|surface, _| surface.alive());
        let inhibited = self.idle_inhibitors.keys().any(|surface| {
            let belongs_to_current_scene = self.output_runtime.values().any(|runtime| {
                runtime.entered_surfaces.contains(surface)
                    && runtime.presented_surfaces.contains(surface)
            });
            belongs_to_current_scene
                && with_states(surface, |states| {
                    surface_primary_scanout_output(surface, states).is_some()
                })
        });
        let events = self.idle_runtime.set_inhibited(inhibited, self.clock.now());
        self.send_idle_events(events);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IdleEvent {
    Idled(u64),
    Resumed(u64),
}

#[derive(Debug)]
struct Timer {
    seat: u64,
    timeout: Duration,
    deadline: Option<Instant>,
    idle: bool,
    ignore_inhibitor: bool,
}

#[derive(Default, Debug)]
pub(super) struct IdleRuntime {
    timers: BTreeMap<u64, Timer>,
    inhibited: bool,
}

#[derive(Debug)]
pub(super) struct IdleNotificationData {
    pub(super) id: u64,
}

impl GlobalDispatch<ExtIdleNotifierV1, ()> for Astera {
    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ExtIdleNotifierV1>,
        _: &(),
        data: &mut DataInit<'_, Self>,
    ) {
        data.init(resource, ());
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for Astera {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ExtIdleNotifierV1,
        request: ext_idle_notifier_v1::Request,
        _: &(),
        _: &DisplayHandle,
        data: &mut DataInit<'_, Self>,
    ) {
        let (new, timeout, seat, ignore) = match request {
            ext_idle_notifier_v1::Request::GetIdleNotification { id, timeout, seat } => {
                (id, timeout, seat, false)
            }
            ext_idle_notifier_v1::Request::GetInputIdleNotification { id, timeout, seat } => {
                (id, timeout, seat, true)
            }
            ext_idle_notifier_v1::Request::Destroy => return,
            _ => return,
        };
        let id = state.next_idle_notification;
        state.next_idle_notification = id.wrapping_add(1).max(1);
        let resource = data.init(new, IdleNotificationData { id });
        state.idle_notifications.insert(id, resource);
        // Multiple wl_seat resources may represent the same Smithay seat. Hash the underlying
        // Seat handle rather than the protocol object ID so every binding shares one activity
        // timeline, while timers for distinct seats remain isolated.
        let Some(seat) = Seat::<Astera>::from_resource(&seat).map(|seat| seat_key(&seat)) else {
            // A wl_seat not created by SeatState cannot receive activity through Astera. Keep the
            // already-created notification inert instead of aliasing it to a real seat.
            return;
        };
        state.idle_runtime.insert(
            id,
            seat,
            Duration::from_millis(u64::from(timeout)),
            ignore,
            state.clock.now(),
        );
    }
}

fn seat_key<D: SeatHandler>(seat: &Seat<D>) -> u64 {
    let mut hasher = DefaultHasher::new();
    seat.hash(&mut hasher);
    hasher.finish()
}

impl Dispatch<ExtIdleNotificationV1, IdleNotificationData> for Astera {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &ExtIdleNotificationV1,
        _: ext_idle_notification_v1::Request,
        _: &IdleNotificationData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
    fn destroyed(
        state: &mut Self,
        _: ClientId,
        _: &ExtIdleNotificationV1,
        data: &IdleNotificationData,
    ) {
        state.idle_notifications.remove(&data.id);
        state.idle_runtime.remove(data.id);
    }
}

impl IdleRuntime {
    pub(super) fn insert(
        &mut self,
        id: u64,
        seat: u64,
        timeout: Duration,
        ignore_inhibitor: bool,
        now: Instant,
    ) {
        let paused = self.inhibited && !ignore_inhibitor;
        self.timers.insert(
            id,
            Timer {
                seat,
                timeout,
                deadline: (!paused).then_some(now + timeout),
                idle: false,
                ignore_inhibitor,
            },
        );
    }

    pub(super) fn remove(&mut self, id: u64) {
        self.timers.remove(&id);
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.timers
            .values()
            .filter_map(|timer| timer.deadline)
            .min()
    }

    pub(super) fn activity(&mut self, seat: u64, now: Instant) -> Vec<IdleEvent> {
        let mut events = Vec::new();
        for (id, timer) in &mut self.timers {
            if timer.seat != seat {
                continue;
            }
            if timer.deadline.is_some_and(|deadline| deadline <= now) && !timer.idle {
                timer.idle = true;
                events.push(IdleEvent::Idled(*id));
            }
            if timer.idle {
                timer.idle = false;
                events.push(IdleEvent::Resumed(*id));
            }
            timer.deadline =
                (!(self.inhibited && !timer.ignore_inhibitor)).then_some(now + timer.timeout);
        }
        events
    }

    pub(super) fn set_inhibited(&mut self, inhibited: bool, now: Instant) -> Vec<IdleEvent> {
        if self.inhibited == inhibited {
            return Vec::new();
        }
        self.inhibited = inhibited;
        let mut events = Vec::new();
        for (id, timer) in &mut self.timers {
            if timer.ignore_inhibitor {
                continue;
            }
            if inhibited {
                timer.deadline = None;
                if timer.idle {
                    timer.idle = false;
                    events.push(IdleEvent::Resumed(*id));
                }
            } else {
                timer.deadline = Some(now + timer.timeout);
            }
        }
        events
    }

    pub(super) fn process_due(&mut self, now: Instant) -> Vec<IdleEvent> {
        let mut events = Vec::new();
        for (id, timer) in &mut self.timers {
            if timer.deadline.is_some_and(|deadline| deadline <= now) {
                timer.deadline = None;
                if !timer.idle {
                    timer.idle = true;
                    events.push(IdleEvent::Idled(*id));
                }
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inhibition_pauses_regular_timer_without_busy_deadline() {
        let now = Instant::now();
        let mut idle = IdleRuntime::default();
        idle.insert(1, 0, Duration::from_secs(5), false, now);
        idle.set_inhibited(true, now + Duration::from_secs(1));
        assert_eq!(idle.deadline(), None);
        assert!(idle.process_due(now + Duration::from_secs(20)).is_empty());
        idle.set_inhibited(false, now + Duration::from_secs(20));
        assert_eq!(idle.deadline(), Some(now + Duration::from_secs(25)));
    }

    #[test]
    fn input_idle_ignores_inhibitor_and_activity_resumes_once() {
        let now = Instant::now();
        let mut idle = IdleRuntime::default();
        idle.set_inhibited(true, now);
        idle.insert(7, 9, Duration::from_secs(1), true, now);
        assert_eq!(
            idle.process_due(now + Duration::from_secs(1)),
            vec![IdleEvent::Idled(7)]
        );
        assert_eq!(
            idle.activity(9, now + Duration::from_secs(2)),
            vec![IdleEvent::Resumed(7)]
        );
        assert!(idle.activity(9, now + Duration::from_secs(2)).is_empty());
    }

    #[test]
    fn overdue_activity_preserves_idled_then_resumed_order() {
        let now = Instant::now();
        let mut idle = IdleRuntime::default();
        idle.insert(3, 4, Duration::from_secs(1), false, now);
        assert_eq!(
            idle.activity(4, now + Duration::from_secs(2)),
            vec![IdleEvent::Idled(3), IdleEvent::Resumed(3)]
        );
    }

    #[test]
    fn activity_only_resets_timers_for_the_matching_seat() {
        let now = Instant::now();
        let mut idle = IdleRuntime::default();
        idle.insert(1, 10, Duration::from_secs(1), false, now);
        idle.insert(2, 20, Duration::from_secs(1), false, now);
        assert_eq!(
            idle.process_due(now + Duration::from_secs(1)),
            vec![IdleEvent::Idled(1), IdleEvent::Idled(2)]
        );
        assert_eq!(
            idle.activity(10, now + Duration::from_secs(2)),
            vec![IdleEvent::Resumed(1)]
        );
        assert!(idle.process_due(now + Duration::from_secs(2)).is_empty());
        assert_eq!(
            idle.activity(20, now + Duration::from_secs(2)),
            vec![IdleEvent::Resumed(2)]
        );
    }
}
