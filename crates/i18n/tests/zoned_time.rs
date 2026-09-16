//! Wall clock ⇄ instant, and the two days a year it is hard.
//!
//! The DST cases mirror the ones for the cron module in `darkwire-core` on
//! purpose. The two implementations do not share code, so what keeps them honest
//! is that they are held to the same answers.

use chrono::DateTime;
use darkwire_i18n::{instant_from_zoned_input, is_valid_time_zone, zoned_input_value};

const KYIV: &str = "Europe/Kyiv";

// A fixture that cannot parse is a failing test either way.
#[allow(clippy::unwrap_used)]
fn ms(iso: &str) -> i64 {
    DateTime::parse_from_rfc3339(iso)
        .unwrap()
        .timestamp_millis()
}

#[test]
fn input_value_reads_an_instant_as_the_wall_clock_in_the_zone_it_is_given() {
    // The same instant, three clocks. A host-zone conversion would render
    // whichever one the machine happens to be set to, which is the bug this
    // exists to remove.
    let at = ms("2026-01-15T06:30:00Z");
    assert_eq!(zoned_input_value(at, "UTC"), "2026-01-15T06:30");
    assert_eq!(zoned_input_value(at, KYIV), "2026-01-15T08:30");
    assert_eq!(zoned_input_value(at, "Asia/Tokyo"), "2026-01-15T15:30");
}

#[test]
fn input_value_pads_every_field_because_the_input_parses_by_position() {
    assert_eq!(
        zoned_input_value(ms("2026-03-05T04:07:00Z"), "UTC"),
        "2026-03-05T04:07"
    );
}

#[test]
fn input_value_is_blank_for_what_it_cannot_show() {
    assert_eq!(zoned_input_value(0, "Mars/Base"), "");
    assert_eq!(zoned_input_value(i64::MAX, "UTC"), "");
}

#[test]
fn input_value_crosses_a_date_boundary_rather_than_clamping_to_the_day() {
    // 22:30 UTC is already tomorrow in Tokyo. Keeping the date fixed would put
    // a job a day out from where the operator pointed.
    assert_eq!(
        zoned_input_value(ms("2026-01-15T22:30:00Z"), "Asia/Tokyo"),
        "2026-01-16T07:30"
    );
}

#[test]
fn instant_reads_a_wall_clock_in_the_zone_it_is_given() {
    assert_eq!(
        instant_from_zoned_input("2026-01-15T08:30", KYIV),
        Some(ms("2026-01-15T06:30:00Z"))
    );
    assert_eq!(
        instant_from_zoned_input("2026-01-15T08:30", "UTC"),
        Some(ms("2026-01-15T08:30:00Z"))
    );
}

#[test]
fn instant_round_trips_with_input_value_in_both_directions() {
    for iso in [
        "2026-01-15T06:30:00Z",
        "2026-07-15T06:30:00Z",
        "2026-12-31T23:00:00Z",
    ] {
        let at = ms(iso);
        assert_eq!(
            instant_from_zoned_input(&zoned_input_value(at, KYIV), KYIV),
            Some(at)
        );
    }
}

#[test]
fn instant_tolerates_a_seconds_component_the_browser_may_append() {
    // A `datetime-local` with a `step` under a minute emits `HH:mm:ss`. The
    // schedule is minute-resolution, so the extra field is read past rather
    // than treated as a parse failure.
    assert_eq!(
        instant_from_zoned_input("2026-01-15T08:30:00", KYIV),
        Some(ms("2026-01-15T06:30:00Z"))
    );
    assert_eq!(
        instant_from_zoned_input("  2026-01-15T08:30  ", KYIV),
        Some(ms("2026-01-15T06:30:00Z"))
    );
}

#[test]
fn instant_is_none_for_anything_that_is_not_a_wall_clock() {
    for bad in [
        "",
        "   ",
        "tomorrow",
        "2026-01-15",
        "15/01/2026 08:30",
        "2026-01-15 08:30",
        "2026-0a-15T08:30",
    ] {
        assert_eq!(instant_from_zoned_input(bad, KYIV), None, "{bad:?}");
    }
}

#[test]
fn instant_is_none_for_numbers_that_parse_but_are_not_a_date() {
    // Without the calendar check `2026-13-05` would overflow into a real
    // instant in a month nobody typed.
    assert_eq!(instant_from_zoned_input("2026-13-05T08:30", KYIV), None);
    assert_eq!(instant_from_zoned_input("2026-01-40T08:30", KYIV), None);
    assert_eq!(instant_from_zoned_input("2026-02-30T08:30", KYIV), None);
    assert_eq!(instant_from_zoned_input("2026-01-15T25:30", KYIV), None);
    assert_eq!(instant_from_zoned_input("2026-01-15T08:75", KYIV), None);
}

#[test]
fn instant_is_none_for_a_zone_nobody_knows() {
    assert_eq!(
        instant_from_zoned_input("2026-01-15T08:30", "Mars/Base"),
        None
    );
}

#[test]
fn instant_is_none_for_an_hour_the_zone_skipped() {
    // Kyiv goes 03:00 → 04:00 on 2026-03-29, so 03:30 never happens. Rounding
    // it into 04:30 would book an hour from where the operator pointed.
    assert_eq!(instant_from_zoned_input("2026-03-29T03:30", KYIV), None);
    // The minute either side of the gap is real and must still resolve.
    assert!(instant_from_zoned_input("2026-03-29T02:59", KYIV).is_some());
    assert!(instant_from_zoned_input("2026-03-29T04:00", KYIV).is_some());
}

#[test]
fn instant_resolves_an_ambiguous_hour_to_the_earlier_one_so_it_happens_once() {
    // Kyiv goes 04:00 → 03:00 on 2026-10-25, so 03:30 happens twice. The
    // earlier one wins; the alternative is a job that runs twice on one night.
    let at = instant_from_zoned_input("2026-10-25T03:30", KYIV);
    assert_eq!(at, Some(ms("2026-10-25T00:30:00Z")));
}

#[test]
fn instant_agrees_with_itself_across_the_transition_in_a_southern_hemisphere_zone() {
    // Sydney transitions the other way round in the calendar, which is what
    // catches an implementation that hard-codes the northern direction.
    assert_eq!(
        instant_from_zoned_input("2026-10-04T02:30", "Australia/Sydney"),
        None
    );
    assert!(instant_from_zoned_input("2026-04-05T02:30", "Australia/Sydney").is_some());
}

#[test]
fn valid_time_zone_accepts_a_real_zone_and_refuses_one_the_database_lacks() {
    assert!(is_valid_time_zone("UTC"));
    assert!(is_valid_time_zone(KYIV));
    assert!(!is_valid_time_zone("Mars/Base"));
    assert!(!is_valid_time_zone(""));
}
