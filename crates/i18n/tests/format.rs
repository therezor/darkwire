//! Number grouping, plural categories and the two interval helpers.

use darkwire_i18n::{
    DurationParts, DurationUnit, PluralCategory, RelativeSpan, SpanUnit, duration_parts,
    format_number, format_token_count, format_window_share, plural_category, relative_span,
};

#[test]
fn number_groups_with_the_separator_the_locale_uses() {
    // The whole point of taking the locale explicitly: a machine set to German
    // renders 8192 as `8.192`, and the panel showing a budget must not depend
    // on the machine.
    assert_eq!(format_number(8192, "en"), "8,192");
    assert_eq!(format_number(8192, "de"), "8.192");
    assert_eq!(format_number(8192, "de_DE.UTF-8"), "8.192");
}

#[test]
fn number_keeps_a_sign_and_every_group_boundary() {
    assert_eq!(format_number(-1500, "en"), "-1,500");
    assert_eq!(format_number(0, "en"), "0");
    assert_eq!(format_number(999, "en"), "999");
    assert_eq!(format_number(1000, "en"), "1,000");
    assert_eq!(format_number(1_234_567, "en"), "1,234,567");
    assert_eq!(format_number(i64::MIN, "en"), "-9,223,372,036,854,775,808");
}

/// The same table as `packages/i18n/test/format.test.ts`, so the terminal and
/// the web shorten a count identically.
const TOKEN_COUNTS: &[(u64, &str, &str)] = &[
    (0, "0", "0"),
    (950, "950", "950"),
    (999, "999", "999"),
    (1_000, "1k", "1k"),
    (1_234, "1.23k", "1,23k"),
    (8_192, "8.19k", "8,19k"),
    (9_995, "10k", "10k"),
    (41_213, "41.2k", "41,2k"),
    (65_536, "65.5k", "65,5k"),
    (99_950, "100k", "100k"),
    (131_072, "131k", "131k"),
    (200_000, "200k", "200k"),
    (999_499, "999k", "999k"),
    (999_950, "1M", "1M"),
    (1_048_576, "1.05M", "1,05M"),
    (12_345_678, "12.3M", "12,3M"),
];

#[test]
fn a_token_count_shortens_to_three_significant_digits() {
    for &(value, en, de) in TOKEN_COUNTS {
        assert_eq!(format_token_count(value, "en"), en, "{value} in en");
        assert_eq!(format_token_count(value, "de"), de, "{value} in de");
    }
}

#[test]
fn a_window_share_is_never_full_until_it_is_over() {
    assert_eq!(format_window_share(0, 65_536), "0%");
    assert_eq!(format_window_share(41_213, 65_536), "63%");
    assert_eq!(format_window_share(1, 65_536), "<1%");
    assert_eq!(format_window_share(65_535, 65_536), "99%");
    assert_eq!(format_window_share(65_536, 65_536), "100%");
    assert_eq!(format_window_share(73_400, 65_536), "112%");
    assert_eq!(format_window_share(10, 0), "0%");
}

#[test]
fn relative_span_picks_the_unit_a_person_would_have_picked() {
    let now = 1_785_000_000_000;
    let ago = |ms: i64| relative_span(now - ms, now);
    assert_eq!(
        ago(5 * 60_000),
        RelativeSpan::Ago {
            value: 5,
            unit: SpanUnit::Minute
        }
    );
    assert_eq!(
        ago(3 * 3_600_000),
        RelativeSpan::Ago {
            value: 3,
            unit: SpanUnit::Hour
        }
    );
    assert_eq!(
        ago(2 * 86_400_000),
        RelativeSpan::Ago {
            value: 2,
            unit: SpanUnit::Day
        }
    );
}

#[test]
fn relative_span_treats_the_last_minute_and_a_clock_skewed_forward_as_now() {
    // The server and the client have separate clocks, and a few seconds of
    // skew is normal rather than an error worth rendering as "-3s ago".
    let now = 1_785_000_000_000;
    assert_eq!(relative_span(now, now), RelativeSpan::Now);
    assert_eq!(relative_span(now - 30_000, now), RelativeSpan::Now);
    assert_eq!(relative_span(now + 3000, now), RelativeSpan::Now);
    assert_eq!(relative_span(i64::MAX, i64::MIN), RelativeSpan::Now);
}

#[test]
fn relative_span_gives_up_on_intervals_once_counting_back_stops_being_useful() {
    let now = 1_785_000_000_000;
    assert!(matches!(
        relative_span(now - 6 * 86_400_000, now),
        RelativeSpan::Ago { .. }
    ));
    assert_eq!(
        relative_span(now - 30 * 86_400_000, now),
        RelativeSpan::Date
    );
}

#[test]
fn duration_stays_in_milliseconds_below_a_second() {
    // That is what distinguishes a cache hit from a request.
    assert_eq!(
        duration_parts(850.0),
        Some(DurationParts {
            unit: DurationUnit::Ms,
            value: 850.0,
            remainder: 0,
            fractional: false
        })
    );
}

#[test]
fn duration_keeps_one_decimal_below_ten_seconds_and_drops_it_above() {
    assert_eq!(
        duration_parts(9400.0),
        Some(DurationParts {
            unit: DurationUnit::Second,
            value: 9.4,
            remainder: 0,
            fractional: true
        })
    );
    assert_eq!(
        duration_parts(42_000.0),
        Some(DurationParts {
            unit: DurationUnit::Second,
            value: 42.0,
            remainder: 0,
            fractional: false
        })
    );
}

#[test]
fn duration_carries_the_remainder_so_2m59s_does_not_read_as_a_rounding_error() {
    assert_eq!(
        duration_parts(179_000.0),
        Some(DurationParts {
            unit: DurationUnit::Minute,
            value: 2.0,
            remainder: 59,
            fractional: false
        })
    );
}

#[test]
fn duration_has_an_hour_branch() {
    // A three-hour turn once rendered as `180m 00s`.
    assert_eq!(
        duration_parts(3.0 * 3_600_000.0 + 5.0 * 60_000.0),
        Some(DurationParts {
            unit: DurationUnit::Hour,
            value: 3.0,
            remainder: 5,
            fractional: false
        })
    );
}

#[test]
fn duration_refuses_a_duration_that_is_not_one() {
    assert_eq!(duration_parts(-1.0), None);
    assert_eq!(duration_parts(f64::NAN), None);
    assert_eq!(duration_parts(f64::INFINITY), None);
}

#[test]
fn duration_does_not_report_a_sub_millisecond_call_as_never_having_run() {
    assert_eq!(duration_parts(0.4).map(|p| p.value), Some(0.0));
    assert_eq!(duration_parts(0.6).map(|p| p.value), Some(1.0));
}

#[test]
fn plural_gives_english_its_two_categories() {
    assert_eq!(plural_category(1_i64, "en"), PluralCategory::One);
    assert_eq!(plural_category(0_i64, "en"), PluralCategory::Other);
    assert_eq!(plural_category(5_i64, "en"), PluralCategory::Other);
}

#[test]
fn plural_gives_polish_the_four_it_has() {
    // The case a hand-rolled `count == 1` cannot express, and the reason plural
    // handling belongs in CLDR rules rather than at a call site.
    assert_eq!(plural_category(1_i64, "pl"), PluralCategory::One);
    assert_eq!(plural_category(2_i64, "pl"), PluralCategory::Few);
    assert_eq!(plural_category(5_i64, "pl"), PluralCategory::Many);
}

#[test]
fn plural_falls_back_to_the_default_locale_for_a_tag_that_is_not_one() {
    assert_eq!(plural_category(1_i64, "not a locale!"), PluralCategory::One);
    assert_eq!(plural_category(1_i64, "pl_PL.UTF-8"), PluralCategory::One);
}
