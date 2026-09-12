//! The locale-aware primitives, shared; the presentation is not.
//!
//! A settings panel renders a token budget as `8,192` and a terminal status line
//! renders the same interval as `2m 30s` rather than a sentence, and that
//! difference is deliberate — a panel has a column to spend and a status line
//! does not. So what is shared here is the part that is *locale* knowledge
//! (which separator, which plural category, which unit) and not the part that
//! is *layout* judgement.
//!
//! Every function takes the locale explicitly. That is the entire fix for the
//! bug a hand-rolled comma grouper once worked around: a machine set to `de-DE`
//! rendered 8192 as `8.192`, which reads as eight in the one panel whose job is
//! making a budget legible. The problem was never that grouping is locale-aware
//! — it is that the locale was *implicit*, and so was whatever the machine
//! happened to be set to. Passed in, it is the install's locale on every machine.
//!
//! Only the primitives the terminal needs are here: number grouping, plural
//! categories, and the two pure arithmetic helpers behind "2m 30s" and "5m ago".
//! Absolute dates, compact numbers and worded intervals need CLDR pattern data
//! the terminal has no caller for, and are left to the web layer.

pub use icu_plurals::PluralCategory;
use icu_plurals::{PluralOperands, PluralRules, PluralRulesOptions};

use crate::locale::{DEFAULT_LOCALE, normalise_locale};

/// A number with the locale's grouping separator: `8,192` in `en`, `8.192` in `de`.
///
/// The separator table holds the languages this crate has been asked to render;
/// an unknown locale gets the default's separator rather than nothing, because
/// a wrong separator is legible and a missing one is a different number.
#[must_use]
pub fn format_number(value: i64, locale: &str) -> String {
    let separator = grouping_separator(locale);
    let digits = value.unsigned_abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    for (index, digit) in digits.chars().enumerate() {
        let remaining = digits.len() - index;
        if index > 0 && remaining.is_multiple_of(3) {
            grouped.push_str(separator);
        }
        grouped.push(digit);
    }
    if value < 0 {
        format!("-{grouped}")
    } else {
        grouped
    }
}

/// The thousands separator for a language. A table rather than a library:
/// this is the whole of what the terminal needs, and CLDR number data is a
/// dependency worth taking when a second locale ships, not before.
fn grouping_separator(locale: &str) -> &'static str {
    let normalised = normalise_locale(Some(locale));
    match normalised.split('-').next().unwrap_or("") {
        "de" => ".",
        _ => ",",
    }
}

/// Which CLDR plural category a count falls into for this locale.
///
/// The translator resolves `_one` / `_other` through the same rules, so this
/// exists for the places that build a string outside a resource — not as a
/// second pluralisation scheme.
pub fn plural_category<C: Into<PluralOperands>>(count: C, locale: &str) -> PluralCategory {
    plural_rules(locale).map_or(PluralCategory::Other, |rules| rules.category_for(count))
}

/// Cardinal plural rules for a locale, falling back to the default locale's
/// when the tag does not parse or has no data. `None` only when even the
/// default has none, which the embedded data rules out.
#[must_use]
pub fn plural_rules(locale: &str) -> Option<PluralRules> {
    let tag = normalise_locale(Some(locale));
    let parsed = icu_locale_core::Locale::try_from_str(&tag)
        .or_else(|_| icu_locale_core::Locale::try_from_str(DEFAULT_LOCALE.as_str()))
        .ok()?;
    PluralRules::try_new((&parsed).into(), PluralRulesOptions::default()).ok()
}

/// Which interval an instant falls into, without saying it in any language.
///
/// Split from the wording because the two halves belong to different owners.
/// The *thresholds* are shared — a minute is a minute in every locale, and the
/// rules about when to stop counting are product decisions rather than language
/// ones. The *wording* is not: a sidebar says `just now`, a terminal may say
/// `now`, and that is a copy decision each surface is entitled to keep.
///
/// `now_ms` is a parameter rather than a clock read so the boundaries can be
/// tested without a fake clock, and so a list rendered in one pass cannot show
/// two rows measured against two different instants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelativeSpan {
    /// Inside the last minute. The caller supplies its own wording.
    Now,
    /// Old enough that an interval reads worse than a date.
    Date,
    /// A whole number of units ago.
    Ago {
        /// How many of `unit`.
        value: i64,
        /// The unit a person would have picked.
        unit: SpanUnit,
    },
}

/// The units a relative interval is phrased in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanUnit {
    /// Under an hour.
    Minute,
    /// Under a day.
    Hour,
    /// Under a week; past that a date reads better.
    Day,
}

/// The interval between `at_ms` and `now_ms`, in the unit a person would pick.
#[must_use]
pub fn relative_span(at_ms: i64, now_ms: i64) -> RelativeSpan {
    let elapsed = now_ms.saturating_sub(at_ms);

    // A timestamp slightly in the *future* is the present rather than a negative
    // interval: the server and the client have separate clocks, and a few
    // seconds of skew is normal rather than an error worth rendering.
    if elapsed < 60_000 {
        return RelativeSpan::Now;
    }

    let minutes = elapsed / 60_000;
    if minutes < 60 {
        return RelativeSpan::Ago {
            value: minutes,
            unit: SpanUnit::Minute,
        };
    }

    let hours = minutes / 60;
    if hours < 24 {
        return RelativeSpan::Ago {
            value: hours,
            unit: SpanUnit::Hour,
        };
    }

    let days = hours / 24;
    // Past a week the relative form stops being informative — "23d ago" is worse
    // than a date, because nobody counts back three weeks in their head.
    if days < 7 {
        RelativeSpan::Ago {
            value: days,
            unit: SpanUnit::Day,
        }
    } else {
        RelativeSpan::Date
    }
}

/// The unit a duration is led with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationUnit {
    /// Below a second, where the resolution distinguishes a cache hit from a request.
    Ms,
    /// Below a minute.
    Second,
    /// Below an hour, with a seconds remainder.
    Minute,
    /// An hour or more, with a minutes remainder.
    Hour,
}

/// A duration broken into its parts, for a caller to word.
///
/// Numbers rather than a string because the surfaces disagree about the wording
/// and agree about the arithmetic — and the arithmetic is the half with the
/// boundaries that are easy to get wrong and invisible when they are: 59.9
/// seconds rounding to "60s", a sub-millisecond tool call reporting "0ms" as
/// though it never ran.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DurationParts {
    /// The unit `value` is in.
    pub unit: DurationUnit,
    /// The leading number, already rounded for display.
    pub value: f64,
    /// The remainder in the next unit down, for `2m 30s`. Zero below a minute.
    pub remainder: i64,
    /// True below ten seconds, where one decimal is worth showing.
    pub fractional: bool,
}

/// Splits a duration in milliseconds. `None` for a negative or non-finite one.
#[must_use]
pub fn duration_parts(ms: f64) -> Option<DurationParts> {
    if !ms.is_finite() || ms < 0.0 {
        return None;
    }
    if ms < 1000.0 {
        return Some(DurationParts {
            unit: DurationUnit::Ms,
            value: ms.round(),
            remainder: 0,
            fractional: false,
        });
    }

    // Whole seconds of any duration a process could measure fit an i64.
    #[allow(clippy::cast_possible_truncation)]
    let total_seconds = (ms / 1000.0).floor() as i64;
    if total_seconds < 60 {
        // One decimal below ten seconds, where the difference is worth seeing.
        let fractional = total_seconds < 10;
        #[allow(clippy::cast_precision_loss)] // total_seconds < 60
        let whole = total_seconds as f64;
        let value = if fractional {
            (ms / 100.0).round() / 10.0
        } else {
            whole
        };
        return Some(DurationParts {
            unit: DurationUnit::Second,
            value,
            remainder: 0,
            fractional,
        });
    }

    let minutes = total_seconds / 60;
    if minutes < 60 {
        #[allow(clippy::cast_precision_loss)] // minutes < 60
        let value = minutes as f64;
        return Some(DurationParts {
            unit: DurationUnit::Minute,
            value,
            remainder: total_seconds % 60,
            fractional: false,
        });
    }

    let hours = minutes / 60;
    #[allow(clippy::cast_precision_loss)] // hours is a small count, exact in f64
    let value = hours as f64;
    Some(DurationParts {
        unit: DurationUnit::Hour,
        value,
        remainder: minutes % 60,
        fractional: false,
    })
}
