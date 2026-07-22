use std::time::Instant;

pub(super) trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

#[derive(Default)]
pub(super) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[cfg(test)]
pub(super) mod testing {
    use std::{
        sync::Mutex,
        time::{Duration, Instant},
    };

    use super::Clock;

    pub(crate) struct ManualClock(Mutex<Instant>);

    impl ManualClock {
        pub(crate) fn new(now: Instant) -> Self {
            Self(Mutex::new(now))
        }

        pub(crate) fn advance(&self, duration: Duration) {
            let mut now = self.0.lock().expect("manual clock lock poisoned");
            *now += duration;
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            *self.0.lock().expect("manual clock lock poisoned")
        }
    }
}
