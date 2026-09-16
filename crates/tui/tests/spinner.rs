//! A frame per tick, never a timer.

use darkwire_tui::{SPINNER_FRAMES, SPINNER_INTERVAL_MS, spinner_frame, visible_width};

#[test]
fn walks_the_frames_in_order_and_wraps_round() {
    // A function of a tick rather than an object with a timer: the timer
    // belongs to whoever is waiting, and that is what keeps this synchronous.
    assert_eq!(spinner_frame(0), SPINNER_FRAMES[0]);
    assert_eq!(spinner_frame(1), SPINNER_FRAMES[1]);
    let count = i64::try_from(SPINNER_FRAMES.len()).unwrap();
    assert_eq!(spinner_frame(count), SPINNER_FRAMES[0]);
    assert_eq!(spinner_frame(count + 3), SPINNER_FRAMES[3]);
}

#[test]
fn accepts_a_negative_tick_rather_than_returning_nothing() {
    assert_eq!(spinner_frame(-1), *SPINNER_FRAMES.last().unwrap());
    assert_eq!(
        spinner_frame(i64::MIN),
        SPINNER_FRAMES[SPINNER_FRAMES.len() - 8]
    );
}

#[test]
fn is_one_column_wide_in_every_frame() {
    for frame in SPINNER_FRAMES {
        assert_eq!(visible_width(frame), 1);
    }
}

/// The interval a rotation reads well at: fast enough to move, slow enough to see.
const _: () = assert!(SPINNER_INTERVAL_MS > 0 && SPINNER_INTERVAL_MS < 250);
