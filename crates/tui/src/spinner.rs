//! The one animated thing in this crate.
//!
//! A pure function of a tick rather than an object that owns a timer, because
//! the timer belongs to whoever is waiting — it is the caller that knows when
//! the work started and when it stopped, and a component holding an interval
//! is a component a test has to wait for. Feeding it a counter keeps every
//! assertion about it synchronous, and keeps this crate away from the clock.

/// Braille dots, which are one column wide and animate by rotation rather
/// than by changing width — so the row they sit on never reflows under them.
pub const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How often a caller should advance the tick for the rotation to read well.
pub const SPINNER_INTERVAL_MS: u64 = 80;

/// The frame for a tick. Any integer, including a negative one.
pub fn spinner_frame(tick: i64) -> &'static str {
    let count = i64::try_from(SPINNER_FRAMES.len()).unwrap_or(1);
    let at = usize::try_from(tick.rem_euclid(count)).unwrap_or(0);
    SPINNER_FRAMES.get(at).copied().unwrap_or(SPINNER_FRAMES[0])
}
