//! Test doubles for the injected seams, behind the `testkit` feature.

use std::sync::Mutex;
use std::time::Duration;

use crate::clock::Clock;

/// A clock tests move by hand. `now_ms` and `monotonic` advance together.
#[derive(Debug)]
pub struct ManualClock {
    state: Mutex<(i64, Duration)>,
}

impl ManualClock {
    /// A clock reading `now_ms` at the epoch given, monotonic origin zero.
    pub fn at(now_ms: i64) -> ManualClock {
        ManualClock {
            state: Mutex::new((now_ms, Duration::ZERO)),
        }
    }

    /// Moves both clocks forward by `delta`.
    pub fn advance(&self, delta: Duration) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.0 += i64::try_from(delta.as_millis()).unwrap_or(i64::MAX);
        state.1 += delta;
    }

    /// Sets the wall clock without moving the monotonic one, the NTP step.
    pub fn set_now_ms(&self, now_ms: i64) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0 = now_ms;
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0
    }

    fn monotonic(&self) -> Duration {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
    }
}
