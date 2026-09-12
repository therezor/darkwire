//! Wall-clock time in a named zone, and back to an instant.
//!
//! This is the *input* half of the timezone story; `format` is the output half.
//! A `datetime-local` field hands back `2026-08-01T14:30` — a wall-clock
//! reading with no zone attached — and something has to decide which clock that
//! was. Left implicit, the answer is the machine's zone, silently, which is
//! exactly what the install-wide `ui.timezone` exists to remove: the field would
//! then mean one thing while the row it renders back means another.
//!
//! **This duplicates the wall-clock resolution in `ghostai-core`'s cron module,
//! on purpose.** The scheduler and the browser-facing input agree about what a
//! wall clock means without one depending on the other, and the two are kept
//! honest by having the same DST cases in both test files rather than by sharing
//! code across a layer boundary.
//!
//! The DST rules are the same, and they are the reason this is not four lines:
//!
//! - A wall-clock time the zone **skipped** (spring forward) is `None`. It is
//!   not an error and not a near-miss to round into the next hour — it is a time
//!   that did not happen, and a caller shows the operator that rather than
//!   booking something an hour from where they pointed.
//! - A wall-clock time that happened **twice** (fall back) resolves to the
//!   earlier instant, so a job written for it runs once.

use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use chrono_tz::Tz;

/// Whether the zone database knows this name. Case-sensitive, as the IANA names
/// are. Cheap enough to call before trusting a config value.
#[must_use]
pub fn is_valid_time_zone(time_zone: &str) -> bool {
    Tz::from_str(time_zone).is_ok()
}

/// An instant as the `YYYY-MM-DDTHH:mm` a `datetime-local` input wants, read in
/// `time_zone` rather than the machine's.
///
/// Returns `""` for an unknown zone or an instant outside the representable
/// range, which is what an empty input reads as — a field that cannot show the
/// value should be blank rather than garbage.
#[must_use]
pub fn zoned_input_value(at_ms: i64, time_zone: &str) -> String {
    let Ok(zone) = Tz::from_str(time_zone) else {
        return String::new();
    };
    let Some(instant) = DateTime::from_timestamp_millis(at_ms) else {
        return String::new();
    };
    instant
        .with_timezone(&zone)
        .format("%Y-%m-%dT%H:%M")
        .to_string()
}

/// A `datetime-local` value read as a wall clock in `time_zone`, as an instant
/// in milliseconds.
///
/// `None` means one of two things a caller must tell apart from success and
/// need not tell apart from each other: the text was not a `YYYY-MM-DDTHH:mm`
/// at all, or it named a wall-clock time the zone skipped. Both are "this is
/// not a real moment", and both deserve the same field error.
///
/// A seconds component after the minute is read past rather than refused: a
/// `datetime-local` with a `step` under a minute emits `HH:mm:ss`, and the
/// schedule is minute-resolution.
#[must_use]
pub fn instant_from_zoned_input(value: &str, time_zone: &str) -> Option<i64> {
    let zone = Tz::from_str(time_zone).ok()?;
    let wall_clock = parse_wall_clock(value.trim())?;
    // `earliest` is the fall-back rule: of two instants that read as this wall
    // clock, the first. A skipped time yields nothing at all.
    let instant = zone.from_local_datetime(&wall_clock).earliest()?;
    Some(instant.timestamp_millis())
}

/// The leading `YYYY-MM-DDTHH:mm` of a value, as a naive date-time.
///
/// Hand-checked by position rather than parsed by pattern so that `2026-13-40`
/// is refused as a month and a day rather than overflowing into a real date the
/// operator did not type; the calendar constructors reject what the digits
/// alone cannot.
fn parse_wall_clock(value: &str) -> Option<NaiveDateTime> {
    let bytes = value.as_bytes();
    if bytes.len() < 16
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
    {
        return None;
    }
    let year = field(value, 0..4)?;
    let month = field(value, 5..7)?;
    let day = field(value, 8..10)?;
    let hour = field(value, 11..13)?;
    let minute = field(value, 14..16)?;
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
    Some(NaiveDateTime::new(date, time))
}

/// A run of ASCII digits at `range`, or `None` when anything else is there.
fn field<N: FromStr>(value: &str, range: std::ops::Range<usize>) -> Option<N> {
    let digits = value.get(range)?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}
