//! The token bucket, driven by hand.
//!
//! This is the whole reason the limiter is hand-rolled rather than taken off
//! the shelf: every one available reads the wall clock, and a limit tested by
//! sleeping through its window is a test that is either slow or flaky. Here a
//! minute moves in one call and the boundary itself is asserted on.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::time::Duration;

use ghostai_core::testkit::ManualClock;
use ghostai_server::rate_limit::{Quota, RateLimiter, WINDOW_MS};

const NOW: i64 = 1_700_000_000_000;

fn limiter(max: u32) -> (Arc<ManualClock>, Arc<RateLimiter>) {
    let clock = Arc::new(ManualClock::at(NOW));
    let limiter = RateLimiter::new(Quota::per_minute(max), Arc::clone(&clock) as Arc<_>);
    (clock, limiter)
}

#[test]
fn a_caller_under_the_limit_is_never_refused() {
    let (_clock, limiter) = limiter(3);
    for _ in 0..3 {
        assert_eq!(limiter.check("1.2.3.4"), None);
    }
}

#[test]
fn the_request_after_the_limit_is_refused() {
    let (_clock, limiter) = limiter(2);
    assert_eq!(limiter.check("1.2.3.4"), None);
    assert_eq!(limiter.check("1.2.3.4"), None);
    assert!(limiter.check("1.2.3.4").is_some());
}

#[test]
fn a_refusal_says_how_long_is_left_of_the_window() {
    let (clock, limiter) = limiter(1);
    assert_eq!(limiter.check("1.2.3.4"), None);
    clock.advance(Duration::from_secs(20));
    assert_eq!(limiter.check("1.2.3.4"), Some(WINDOW_MS - 20_000));
}

#[test]
fn a_refusal_does_not_consume() {
    // Otherwise a client polling through a closed window would hold it closed
    // forever, which turns a rate limit into a lockout.
    let (clock, limiter) = limiter(1);
    assert_eq!(limiter.check("1.2.3.4"), None);
    for _ in 0..10 {
        assert!(limiter.check("1.2.3.4").is_some());
    }
    clock.advance(Duration::from_millis(u64::try_from(WINDOW_MS).unwrap()));
    assert_eq!(limiter.check("1.2.3.4"), None);
}

#[test]
fn the_window_reopens_exactly_at_the_boundary() {
    let (clock, limiter) = limiter(1);
    assert_eq!(limiter.check("1.2.3.4"), None);

    clock.advance(Duration::from_millis(u64::try_from(WINDOW_MS).unwrap() - 1));
    assert!(limiter.check("1.2.3.4").is_some(), "one millisecond early");

    clock.advance(Duration::from_millis(1));
    assert_eq!(limiter.check("1.2.3.4"), None, "on the boundary");
}

#[test]
fn two_callers_have_two_budgets() {
    let (_clock, limiter) = limiter(1);
    assert_eq!(limiter.check("1.2.3.4"), None);
    assert_eq!(limiter.check("5.6.7.8"), None);
    assert!(limiter.check("1.2.3.4").is_some());
    assert!(limiter.check("5.6.7.8").is_some());
}

#[test]
fn a_reset_forgets_every_count() {
    let (_clock, limiter) = limiter(1);
    assert_eq!(limiter.check("1.2.3.4"), None);
    assert!(limiter.check("1.2.3.4").is_some());
    limiter.reset();
    assert_eq!(limiter.check("1.2.3.4"), None);
}

#[test]
fn an_expired_entry_is_dropped_rather_than_kept_forever() {
    // The map is only ever read by key, so an expired entry costs memory and
    // nothing else — which is exactly why a long-lived process must not hold
    // one per address it has ever seen.
    let (clock, limiter) = limiter(5);
    for address in 0..50u8 {
        assert_eq!(limiter.check(&format!("10.0.0.{address}")), None);
    }
    assert!(format!("{limiter:?}").contains("tracked: 50"));

    clock.advance(Duration::from_millis(u64::try_from(WINDOW_MS).unwrap() + 1));
    assert_eq!(limiter.check("10.0.0.0"), None);
    assert!(format!("{limiter:?}").contains("tracked: 1"));
}

#[test]
fn a_zero_quota_refuses_everything() {
    let (_clock, limiter) = limiter(0);
    assert!(limiter.check("1.2.3.4").is_some());
}

#[test]
fn a_clock_stepped_backwards_does_not_open_the_window() {
    // An NTP step is the realistic cause, and saturating arithmetic is what
    // keeps it from reading as "the window elapsed".
    let (clock, limiter) = limiter(1);
    assert_eq!(limiter.check("1.2.3.4"), None);
    clock.set_now_ms(NOW - 10_000);
    assert!(limiter.check("1.2.3.4").is_some());
}
