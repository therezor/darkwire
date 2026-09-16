//! The system clock, the manual clock and the cancellable sleep.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::time::Duration;

use darkwire_core::testkit::ManualClock;
use darkwire_core::{Clock, ErrorKind, SystemClock, sleep};
use tokio_util::sync::CancellationToken;

mod system_clock {
    use super::*;

    #[test]
    fn reports_wall_clock_epoch_milliseconds() {
        assert!(SystemClock.now_ms() > 1_600_000_000_000);
    }

    #[test]
    fn reports_a_monotonic_reading_that_never_goes_backwards() {
        let first = SystemClock.monotonic();
        let second = SystemClock.monotonic();
        assert!(second >= first);
    }

    #[test]
    fn is_a_plain_default_value() {
        let clock = SystemClock;
        assert!(format!("{clock:?}").contains("SystemClock"));
    }
}

mod manual_clock {
    use super::*;

    #[test]
    fn starts_where_it_is_told_with_a_zero_monotonic_origin() {
        let clock = ManualClock::at(1_700_000_000_000);
        assert_eq!(clock.now_ms(), 1_700_000_000_000);
        assert_eq!(clock.monotonic(), Duration::ZERO);
    }

    #[test]
    fn advances_both_clocks_together() {
        let clock = ManualClock::at(1_000);
        clock.advance(Duration::from_millis(250));
        assert_eq!(clock.now_ms(), 1_250);
        assert_eq!(clock.monotonic(), Duration::from_millis(250));
    }

    #[test]
    fn steps_the_wall_clock_without_moving_the_monotonic_one() {
        // The NTP correction: durations measured on `monotonic` must not see it.
        let clock = ManualClock::at(1_000);
        clock.advance(Duration::from_millis(10));
        clock.set_now_ms(500);
        assert_eq!(clock.now_ms(), 500);
        assert_eq!(clock.monotonic(), Duration::from_millis(10));
    }
}

mod sleeping {
    use super::*;

    #[tokio::test]
    async fn resolves_after_the_delay() {
        let token = CancellationToken::new();
        let started = SystemClock.monotonic();
        sleep(Duration::from_millis(20), &token).await.unwrap();
        assert!(SystemClock.monotonic().saturating_sub(started) >= Duration::from_millis(20));
    }

    #[tokio::test]
    async fn fails_immediately_when_the_token_has_already_fired() {
        let token = CancellationToken::new();
        token.cancel();
        let started = SystemClock.monotonic();
        let error = sleep(Duration::from_secs(30), &token).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Aborted);
        assert!(error.is_aborted());
        assert!(SystemClock.monotonic().saturating_sub(started) < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn fails_when_the_token_fires_during_the_sleep() {
        let token = CancellationToken::new();
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            canceller.cancel();
        });
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            sleep(Duration::from_secs(30), &token),
        )
        .await
        .expect("the sleep should be cut short")
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Aborted);
    }

    #[tokio::test]
    async fn a_child_token_composes_a_timeout() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        parent.cancel();
        assert!(sleep(Duration::from_secs(30), &child).await.is_err());
    }
}
