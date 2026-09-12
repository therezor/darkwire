//! Cron parsing and the next-run search, including the DST cases and the parity
//! fixture.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use chrono::{DateTime, Datelike as _, TimeZone as _, Timelike as _, Utc};
use chrono_tz::Tz;
use ghostai_core::cron::{CronSpec, next_cron_run, parse_cron};
use ghostai_core::{ErrorKind, GhostError};
use proptest::prelude::*;
use serde_json::{Value, json};

const NEXT_RUN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/cron/next-run.json"
));

fn ms(iso: &str) -> i64 {
    DateTime::parse_from_rfc3339(iso)
        .unwrap()
        .timestamp_millis()
}

/// The instant a zone's wall clock reads this, for assertions.
fn local_of(instant_ms: i64, tz: &str) -> String {
    let zone: Tz = tz.parse().unwrap();
    zone.timestamp_millis_opt(instant_ms)
        .single()
        .unwrap()
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn next_local(expr: &str, tz: &str, from: &str) -> String {
    let spec = parse_cron(expr, Some(tz)).unwrap();
    let next = next_cron_run(&spec, ms(from)).expect("a next run");
    local_of(next, tz)
}

fn err(result: Result<CronSpec, GhostError>) -> GhostError {
    match result {
        Ok(spec) => panic!("expected an error, got {spec:?}"),
        Err(error) => error,
    }
}

mod parse {
    use super::*;

    #[test]
    fn parses_the_five_fields() {
        let spec = parse_cron("30 9 * * *", None).unwrap();
        assert_eq!(spec.minutes, vec![30]);
        assert_eq!(spec.hours, vec![9]);
        assert_eq!(spec.days_of_month.len(), 31);
        assert_eq!(spec.months.len(), 12);
        assert_eq!(spec.days_of_week.len(), 7);
        assert_eq!(spec.tz, None);
    }

    #[test]
    fn refuses_a_six_field_expression_by_name_rather_than_absorbing_it() {
        // Read as five fields plus a stray, `0 * * * * *` runs sixty times more
        // often than asked.
        assert!(
            err(parse_cron("0 * * * * *", None))
                .message
                .contains("Seconds and years are not supported")
        );
        assert!(
            err(parse_cron("* * * *", None))
                .message
                .contains("expected 5 fields")
        );
    }

    #[test]
    fn expands_ranges_lists_and_steps() {
        assert_eq!(
            parse_cron("0,30 * * * *", None).unwrap().minutes,
            vec![0, 30]
        );
        assert_eq!(
            parse_cron("*/15 * * * *", None).unwrap().minutes,
            vec![0, 15, 30, 45]
        );
        assert_eq!(
            parse_cron("10-13 * * * *", None).unwrap().minutes,
            vec![10, 11, 12, 13]
        );
        assert_eq!(
            parse_cron("10-20/5 * * * *", None).unwrap().minutes,
            vec![10, 15, 20]
        );
    }

    #[test]
    fn accepts_a_bare_start_with_a_step_running_to_the_end_of_the_range() {
        assert_eq!(
            parse_cron("0/20 * * * *", None).unwrap().minutes,
            vec![0, 20, 40]
        );
    }

    #[test]
    fn accepts_month_and_weekday_names_case_insensitively() {
        assert_eq!(parse_cron("0 0 1 JAN *", None).unwrap().months, vec![1]);
        assert_eq!(
            parse_cron("0 0 1 jan-mar *", None).unwrap().months,
            vec![1, 2, 3]
        );
        assert_eq!(
            parse_cron("0 0 * * mon,FRI", None).unwrap().days_of_week,
            vec![1, 5]
        );
    }

    #[test]
    fn folds_day_of_week_7_onto_sunday() {
        assert_eq!(parse_cron("0 0 * * 7", None).unwrap().days_of_week, vec![0]);
        assert_eq!(
            parse_cron("0 0 * * 0,7", None).unwrap().days_of_week,
            vec![0]
        );
    }

    #[test]
    fn records_whether_each_day_field_was_restricted() {
        // Identical sets, different meanings: this is what the OR rule reads.
        assert!(!parse_cron("0 0 * * *", None).unwrap().dow_restricted);
        assert!(parse_cron("0 0 * * 0-6", None).unwrap().dow_restricted);
        assert!(parse_cron("0 0 1-31 * *", None).unwrap().dom_restricted);
    }

    #[test]
    fn refuses_out_of_range_values_backwards_ranges_and_bad_steps() {
        let message = |expr: &str| err(parse_cron(expr, None)).message;
        assert!(message("60 * * * *").contains("minute must be between 0 and 59"));
        assert!(message("* 24 * * *").contains("hour must be between 0 and 23"));
        assert!(message("* * 0 * *").contains("day-of-month must be between 1 and 31"));
        assert!(message("* * * 13 *").contains("month must be between 1 and 12"));
        assert!(message("20-10 * * * *").contains("runs backwards"));
        assert!(message("*/0 * * * *").contains("positive whole number"));
        assert!(message("*/-2 * * * *").contains("positive whole number"));
        assert!(message("1,,2 * * * *").contains("empty term"));
    }

    #[test]
    fn refuses_a_value_that_is_not_a_number_or_a_known_name() {
        assert!(
            err(parse_cron("* * * * funday", None))
                .message
                .contains("not a value day-of-week accepts")
        );
        // The one spelling of a number: no sign, no leading zero.
        assert!(
            err(parse_cron("+5 * * * *", None))
                .message
                .contains("\"+5\" is not a value")
        );
        assert!(
            err(parse_cron("05 * * * *", None))
                .message
                .contains("\"05\" is not a value")
        );
    }

    #[test]
    fn fails_with_a_config_error_so_the_route_can_answer_422() {
        let error = err(parse_cron("nonsense", None));
        assert_eq!(error.kind, ErrorKind::Config);
        assert_eq!(error.details["expr"], "nonsense");
    }

    #[test]
    fn refuses_an_unknown_timezone_while_the_operator_still_has_the_request() {
        let error = err(parse_cron("0 9 * * *", Some("Mars/Olympus_Mons")));
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(error.message.contains("Unknown timezone"));
        assert_eq!(error.details["tz"], "Mars/Olympus_Mons");
    }

    #[test]
    fn keeps_the_zone_on_the_spec() {
        let spec = parse_cron("0 9 * * *", Some("Europe/Kyiv")).unwrap();
        assert_eq!(spec.tz, Some(chrono_tz::Europe::Kyiv));
    }
}

mod next_run {
    use super::*;

    #[test]
    fn finds_the_next_daily_occurrence_in_the_named_zone_not_the_host_zone() {
        // 08:00 UTC is 09:00 in Kyiv in winter, so a 09:00 Kyiv job is already past.
        assert_eq!(
            next_local("0 9 * * *", "Europe/Kyiv", "2026-01-15T08:00:00Z"),
            "2026-01-16 09:00"
        );
        assert_eq!(
            next_local("0 9 * * *", "Europe/Kyiv", "2026-01-15T06:00:00Z"),
            "2026-01-15 09:00"
        );
    }

    #[test]
    fn is_strictly_after_the_instant_given() {
        let spec = parse_cron("0 9 * * *", Some("UTC")).unwrap();
        assert_eq!(
            next_cron_run(&spec, ms("2026-01-15T09:00:00Z")),
            Some(ms("2026-01-16T09:00:00Z"))
        );
    }

    #[test]
    fn rolls_over_a_month_and_a_year_boundary() {
        assert_eq!(
            next_local("0 0 1 * *", "UTC", "2026-12-15T00:00:00Z"),
            "2027-01-01 00:00"
        );
        assert_eq!(
            next_local("0 0 * * *", "UTC", "2026-02-28T12:00:00Z"),
            "2026-03-01 00:00"
        );
    }

    #[test]
    fn finds_29_february_only_in_a_leap_year() {
        assert_eq!(
            next_local("0 0 29 2 *", "UTC", "2026-03-01T00:00:00Z"),
            "2028-02-29 00:00"
        );
    }

    #[test]
    fn uses_the_host_zone_when_none_is_named() {
        // Whatever the host zone, the answer satisfies the expression there.
        let spec = parse_cron("*/5 * * * *", None).unwrap();
        let from = ms("2026-06-01T00:00:00Z");
        let next = next_cron_run(&spec, from).unwrap();
        assert!(next > from && next - from <= 5 * 60_000);
        assert_eq!(next % 60_000, 0);
    }

    mod or_rule {
        use super::*;

        #[test]
        fn matches_either_when_both_are_restricted() {
            // "The 13th, and also every Friday", not "Friday the 13th".
            let spec = parse_cron("0 0 13 * 5", Some("UTC")).unwrap();
            let first = next_cron_run(&spec, ms("2026-11-01T00:00:00Z")).unwrap();
            // 6 November 2026 is a Friday and comes before the 13th.
            assert_eq!(local_of(first, "UTC"), "2026-11-06 00:00");
            assert_eq!(
                local_of(next_cron_run(&spec, first).unwrap(), "UTC"),
                "2026-11-13 00:00"
            );
        }

        #[test]
        fn applies_only_the_restricted_one_when_the_other_is_a_star() {
            assert_eq!(
                next_local("0 0 13 * *", "UTC", "2026-11-01T00:00:00Z"),
                "2026-11-13 00:00"
            );
            assert_eq!(
                next_local("0 0 * * 5", "UTC", "2026-11-01T00:00:00Z"),
                "2026-11-06 00:00"
            );
        }

        #[test]
        fn treats_an_explicit_full_weekday_range_as_restricted() {
            // `0-6` covers every weekday, so ORing it with the 13th matches daily.
            assert_eq!(
                next_local("0 0 13 * 0-6", "UTC", "2026-11-01T12:00:00Z"),
                "2026-11-02 00:00"
            );
        }
    }

    mod daylight_saving {
        use super::*;

        // London springs forward at 01:00 UTC on 29 March 2026: 01:00 -> 02:00
        // local, so 01:30 local does not exist that day.
        #[test]
        fn skips_a_wall_clock_time_the_zone_never_reaches() {
            assert_eq!(
                next_local("30 1 * * *", "Europe/London", "2026-03-28T12:00:00Z"),
                "2026-03-30 01:30"
            );
        }

        #[test]
        fn still_fires_on_a_day_whose_skipped_hour_is_not_the_scheduled_one() {
            assert_eq!(
                next_local("30 3 * * *", "Europe/London", "2026-03-28T12:00:00Z"),
                "2026-03-29 03:30"
            );
        }

        // New York falls back at 06:00 UTC on 1 November 2026: 02:00 -> 01:00
        // local, so 01:30 local happens twice.
        #[test]
        fn fires_once_on_an_ambiguous_time_at_the_earlier_instant() {
            let spec = parse_cron("30 1 * * *", Some("America/New_York")).unwrap();
            let first = next_cron_run(&spec, ms("2026-10-31T12:00:00Z")).unwrap();
            assert_eq!(local_of(first, "America/New_York"), "2026-11-01 01:30");
            // The earlier of the two, which is EDT (UTC-4), not EST (UTC-5).
            assert_eq!(first, ms("2026-11-01T05:30:00Z"));
            // And the next one is the following day, not the second 01:30.
            let second = next_cron_run(&spec, first).unwrap();
            assert_eq!(local_of(second, "America/New_York"), "2026-11-02 01:30");
        }

        // Kyiv falls back at 01:00 UTC on 25 October 2026: 04:00 -> 03:00 local,
        // so 03:30 local happens twice.
        #[test]
        fn fires_at_the_earlier_instant_east_of_utc_too() {
            let spec = parse_cron("30 3 * * *", Some("Europe/Kyiv")).unwrap();
            let first = next_cron_run(&spec, ms("2026-10-24T12:00:00Z")).unwrap();
            assert_eq!(local_of(first, "Europe/Kyiv"), "2026-10-25 03:30");
            // The earlier of the two, which is EEST (UTC+3), not EET (UTC+2).
            assert_eq!(first, ms("2026-10-25T00:30:00Z"));
            let second = next_cron_run(&spec, first).unwrap();
            assert_eq!(local_of(second, "Europe/Kyiv"), "2026-10-26 03:30");
        }

        #[test]
        fn skips_a_wall_clock_time_an_east_of_utc_zone_never_reaches() {
            // Kyiv goes 03:00 -> 04:00 on 29 March 2026, so 03:30 does not exist.
            let spec = parse_cron("30 3 * * *", Some("Europe/Kyiv")).unwrap();
            let at = next_cron_run(&spec, ms("2026-03-28T12:00:00Z")).unwrap();
            assert_eq!(local_of(at, "Europe/Kyiv"), "2026-03-30 03:30");
        }

        #[test]
        fn runs_an_hourly_job_once_per_wall_clock_hour_skipping_the_repeated_one() {
            // On a fall-back night an hourly job sees 01:00 once, so there is a
            // single two-hour gap in *real* time. Firing it twice would mean a
            // job that says "hourly" running 25 times that day.
            let spec = parse_cron("0 * * * *", Some("America/New_York")).unwrap();
            let mut at = ms("2026-11-01T04:00:00Z");
            let mut seen = Vec::new();
            for _ in 0..4 {
                at = next_cron_run(&spec, at).unwrap();
                seen.push(at);
            }
            let locals: Vec<String> = seen
                .iter()
                .map(|t| local_of(*t, "America/New_York"))
                .collect();
            assert_eq!(
                locals,
                [
                    "2026-11-01 01:00",
                    "2026-11-01 02:00",
                    "2026-11-01 03:00",
                    "2026-11-01 04:00"
                ]
            );
            let gaps: Vec<i64> = seen.windows(2).map(|pair| pair[1] - pair[0]).collect();
            assert_eq!(gaps, [7_200_000, 3_600_000, 3_600_000]);
        }
    }

    #[test]
    fn returns_none_for_an_expression_that_can_never_match() {
        // 30 February is legal to write and impossible to reach.
        let spec = parse_cron("0 0 30 2 *", Some("UTC")).unwrap();
        assert_eq!(next_cron_run(&spec, ms("2026-01-01T00:00:00Z")), None);
    }

    #[test]
    fn returns_none_rather_than_searching_forever_past_the_bound() {
        let spec = parse_cron("0 0 31 4 *", Some("UTC")).unwrap();
        assert_eq!(next_cron_run(&spec, ms("2026-01-01T00:00:00Z")), None);
    }
}

const EXPRESSIONS: [&str; 8] = [
    "* * * * *",
    "0 * * * *",
    "*/7 * * * *",
    "30 9 * * 1-5",
    "0 0 13 * 5",
    "15 3 1 * *",
    "0 12 * jan,jul *",
    "0 0 29 2 *",
];

const ZONES: [&str; 5] = [
    "UTC",
    "Europe/London",
    "America/New_York",
    "Asia/Kolkata",
    "Australia/Lord_Howe",
];

/// Whether an instant's local wall clock satisfies the spec.
fn satisfies(spec: &CronSpec, instant_ms: i64) -> bool {
    let zone = spec.tz.unwrap_or(chrono_tz::UTC);
    let local = zone.timestamp_millis_opt(instant_ms).single().unwrap();
    if !spec.minutes.contains(&local.minute()) || !spec.hours.contains(&local.hour()) {
        return false;
    }
    if !spec.months.contains(&local.month()) {
        return false;
    }
    let day_hit = spec.days_of_month.contains(&local.day());
    let weekday_hit = spec
        .days_of_week
        .contains(&local.weekday().num_days_from_sunday());
    if spec.dom_restricted && spec.dow_restricted {
        day_hit || weekday_hit
    } else {
        day_hit && weekday_hit
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn returns_an_instant_that_satisfies_the_expression(
        expr in proptest::sample::select(EXPRESSIONS.as_slice()),
        tz in proptest::sample::select(ZONES.as_slice()),
        from in ms("2024-01-01T00:00:00Z")..ms("2029-01-01T00:00:00Z"),
    ) {
        let spec = parse_cron(expr, Some(tz)).unwrap();
        if let Some(next) = next_cron_run(&spec, from) {
            prop_assert!(next > from);
            prop_assert!(satisfies(&spec, next));
        }
    }
}

// Minimality, without walking every minute in between: if some instant
// strictly between `from` and `next` satisfied the spec, asking from just
// before it would return that instant rather than `next`.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "a millisecond gap inside one year fits a float exactly"
    )]
    fn gives_the_same_answer_asked_again_from_anywhere_inside_the_gap(
        expr in proptest::sample::select(
            ["30 9 * * 1-5", "0 0 13 * 5", "*/7 * * * *", "0 2 * * *"].as_slice()
        ),
        tz in proptest::sample::select(["UTC", "Europe/London", "America/New_York"].as_slice()),
        from in ms("2026-01-01T00:00:00Z")..ms("2026-12-01T00:00:00Z"),
        fraction in 0.0_f64..=1.0,
    ) {
        let spec = parse_cron(expr, Some(tz)).unwrap();
        if let Some(next) = next_cron_run(&spec, from) {
            let inside = from + (((next - from - 1) as f64) * fraction).floor() as i64;
            prop_assert_eq!(next_cron_run(&spec, inside), Some(next));
        }
    }
}

#[test]
fn skips_every_minute_in_a_known_gap_checked_exhaustively() {
    // The exact check the property above trades away, over a window small
    // enough to pay for: a weekday-09:30 job asked from Saturday morning.
    let spec = parse_cron("30 9 * * 1-5", Some("UTC")).unwrap();
    let from = ms("2026-11-07T00:00:00Z");
    let next = next_cron_run(&spec, from).unwrap();
    let mut t = from + 60_000;
    while t < next {
        assert!(!satisfies(&spec, t), "{}", local_of(t, "UTC"));
        t += 60_000;
    }
    assert!(satisfies(&spec, next));
}

fn spec_json(spec: &CronSpec) -> Value {
    let mut value = json!({
        "minutes": spec.minutes,
        "hours": spec.hours,
        "daysOfMonth": spec.days_of_month,
        "months": spec.months,
        "daysOfWeek": spec.days_of_week,
        "domRestricted": spec.dom_restricted,
        "dowRestricted": spec.dow_restricted,
    });
    if let Some(tz) = spec.tz {
        value["tz"] = Value::from(tz.name());
    }
    value
}

#[test]
fn matches_the_next_run_fixture() {
    let fixture: Value = serde_json::from_str(NEXT_RUN).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 64);

    let mut errors = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let input = &case["input"];
        let expr = input["expr"].as_str().unwrap();
        let tz = input.get("tz").and_then(Value::as_str);
        let after = input.get("afterMs").and_then(Value::as_i64);

        let produced = match parse_cron(expr, tz) {
            Err(error) => {
                errors += 1;
                json!({"error": {"kind": error.kind.as_str(), "message": error.message}})
            }
            Ok(spec) => {
                let mut output = json!({"spec": spec_json(&spec)});
                if let Some(after) = after {
                    output["nextMs"] = next_cron_run(&spec, after).map_or(Value::Null, Value::from);
                }
                output
            }
        };
        assert_eq!(produced, case["output"], "case: {name}");
    }
    assert_eq!(errors, 20);
}

#[test]
fn the_fixture_instants_round_trip_through_utc() {
    // A sanity check on the reading of the fixture itself: `after` is
    // `afterMs` as ISO 8601.
    let fixture: Value = serde_json::from_str(NEXT_RUN).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        if let (Some(after), Some(iso)) = (
            case["input"].get("afterMs").and_then(Value::as_i64),
            case["input"].get("after").and_then(Value::as_str),
        ) {
            assert_eq!(
                Utc.timestamp_millis_opt(after).single().unwrap(),
                ms(iso).pipe_utc()
            );
        }
    }
}

trait PipeUtc {
    fn pipe_utc(self) -> DateTime<Utc>;
}

impl PipeUtc for i64 {
    fn pipe_utc(self) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(self).single().unwrap()
    }
}
