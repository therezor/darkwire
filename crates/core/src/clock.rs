//! The injectable clock.
//!
//! Nothing in DarkWire reads the system time directly. Everything time-dependent
//! takes a `Clock`, so tests drive it by hand and nothing ever sleeps for real.
//!
//! The trait separates two kinds of time on purpose:
//!
//! - `now_ms` is wall-clock epoch milliseconds. It is what gets persisted and
//!   displayed, and it can jump backwards when NTP corrects the host.
//! - `monotonic` only ever moves forward. Every *duration* (elapsed turn time,
//!   token-bucket refill, timeout accounting) uses it, so an NTP step mid-turn
//!   cannot make a turn appear to have taken negative time or reset a rate limit.
//!
//! Conflating the two is the classic bug here, and it fails in production at
//! 3am rather than in a test.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;

use crate::errors::{Result, WireError};

/// Wall-clock and monotonic time, injected.
pub trait Clock: Send + Sync {
    /// Wall-clock epoch milliseconds. Persist and display this one.
    fn now_ms(&self) -> i64;
    /// Monotonic time from an arbitrary origin. Measure durations with this one.
    fn monotonic(&self) -> Duration;
}

/// The host's clocks.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

static PROCESS_START: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        // The one permitted read of the system clock; everything else goes
        // through this trait.
        #[allow(clippy::disallowed_methods)]
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    }

    fn monotonic(&self) -> Duration {
        PROCESS_START.elapsed()
    }
}

/// Sleeps for `delay`, or returns an `aborted` error as soon as `token` fires.
///
/// Tokio's timer is what tests pause, so this is the one place a wait is
/// expressed; a bespoke timeout beside it would be a second clock.
pub async fn sleep(delay: Duration, token: &CancellationToken) -> Result<()> {
    if token.is_cancelled() {
        return Err(WireError::aborted("Sleep"));
    }
    tokio::select! {
        () = token.cancelled() => Err(WireError::aborted("Sleep")),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}
