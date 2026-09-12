//! Five-field cron, evaluated in an IANA timezone.
//!
//! Hand-written rather than a dependency, for the same reason the rest of this
//! crate is: a scheduled job is the one feature that must keep working on an
//! install that has never reached a registry, and "what fires at 2am" is not a
//! question worth answering by reading someone else's transitive tree.
//!
//! Three decisions carry the whole module.
//!
//! **The dialect is exactly five fields.** A six- or seven-field expression is
//! refused by name rather than absorbed. Every cron dialect that grew a seconds
//! column put it at the *front*, so reading `0 * * * * *` as a five-field
//! expression plus a stray does not run something slightly wrong: it runs it
//! sixty times more often than the operator asked for, and it does so silently.
//!
//! **Day-of-month and day-of-week are OR, not AND, but only when both are
//! restricted.** `0 0 13 * 5` is "the 13th, and also every Friday", not "Friday
//! the 13th". This is the single most misimplemented rule in cron and it is the
//! reason [`CronSpec`] carries `dom_restricted` / `dow_restricted` rather than
//! inferring intent from a full set: `*` and `0-6` produce identical sets and
//! mean different things.
//!
//! **Time arithmetic never happens in local time.** A wall-clock time is not a
//! quantity: it can fail to exist and it can happen twice, so nothing here adds
//! an hour to a local time and hopes. The search walks *calendar days*, which
//! advance identically in every zone, and converts a matched wall-clock slot to
//! an instant through the zone's rules. A slot that does not exist (spring
//! forward) is skipped; an ambiguous one (fall back) resolves to the earlier
//! instant, so it fires once.
//!
//! That last rule has a consequence worth stating rather than discovering: an
//! hourly job sees the repeated wall-clock hour once, so on a fall-back night
//! there is a single two-hour gap in real time. The alternative, firing on both
//! instants, means a job the operator wrote as "hourly" running twenty-five
//! times that day, and a heartbeat billing for it. Once is the answer.

use std::collections::BTreeSet;

use chrono::{DateTime, Datelike as _, Local, MappedLocalTime, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use ghostai_protocol::json::js_trim;

use crate::errors::{ErrorKind, GhostError, Result};

/// Cap on the forward search. `0 0 30 2 *` matches nothing, ever.
const MAX_SEARCH_DAYS: u32 = 1464;

/// Cap on slots examined across the whole search.
///
/// The day walk is bounded above, but a pathological expression could still
/// ask for 1440 instant conversions a day for four years. This is the backstop
/// that keeps [`next_cron_run`] a function rather than a hang; a real
/// expression returns within a handful.
const MAX_SLOT_PROBES: u32 = 100_000;

const MONTH_NAMES: &[(&str, u32)] = &[
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

const DAY_NAMES: &[(&str, u32)] = &[
    ("sun", 0),
    ("mon", 1),
    ("tue", 2),
    ("wed", 3),
    ("thu", 4),
    ("fri", 5),
    ("sat", 6),
];

/// A parsed expression.
///
/// Values are sorted ascending lists rather than sets: every consumer iterates
/// them in order looking for the first match. Membership tests are over ranges
/// small enough (at most 60) that a linear scan is not worth a second structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronSpec {
    /// 0–59, ascending.
    pub minutes: Vec<u32>,
    /// 0–23, ascending.
    pub hours: Vec<u32>,
    /// 1–31, ascending.
    pub days_of_month: Vec<u32>,
    /// 1–12, ascending.
    pub months: Vec<u32>,
    /// 0–6, Sunday is 0, ascending.
    pub days_of_week: Vec<u32>,
    /// Whether the day-of-month field was anything but `*`. Drives the OR rule.
    pub dom_restricted: bool,
    /// Whether the day-of-week field was anything but `*`. Drives the OR rule.
    pub dow_restricted: bool,
    /// The zone. The host zone when absent.
    pub tz: Option<Tz>,
}

fn fail(expr: &str, detail: &str) -> GhostError {
    GhostError::new(
        ErrorKind::Config,
        format!("Invalid cron expression \"{expr}\": {detail}"),
    )
    .with_detail("expr", expr)
}

/// A whole number written the one way an operator means it: no sign, no
/// leading zero, no fraction. `05` and `+5` are refused so that a typo cannot
/// be read as a different value than the one on the page.
fn plain_integer(raw: &str) -> Option<i64> {
    let value: i64 = raw.parse().ok()?;
    (value.to_string() == js_trim(raw)).then_some(value)
}

fn parse_value(
    raw: &str,
    min: u32,
    max: u32,
    names: Option<&[(&str, u32)]>,
    expr: &str,
    field: &str,
) -> Result<u32> {
    let lowered = raw.to_lowercase();
    let by_name = names.and_then(|names| {
        names
            .iter()
            .find_map(|(name, value)| (*name == lowered).then_some(*value))
    });
    let value = match by_name {
        Some(value) => i64::from(value),
        None => plain_integer(raw)
            .ok_or_else(|| fail(expr, &format!("\"{raw}\" is not a value {field} accepts.")))?,
    };
    if value < i64::from(min) || value > i64::from(max) {
        return Err(fail(
            expr,
            &format!("{field} must be between {min} and {max}, got {raw}."),
        ));
    }
    u32::try_from(value)
        .map_err(|_| fail(expr, &format!("\"{raw}\" is not a value {field} accepts.")))
}

/// One field to the sorted values it matches.
///
/// Accepts a star, `a`, `a-b`, and a `/step` suffix on any of those. `a/n`, a
/// bare start with a step meaning "from a to the end of the range", is the one
/// common extension included, because operators reach for `0/15` as often as
/// the star form, and refusing it teaches nothing.
fn parse_field(
    raw: &str,
    min: u32,
    max: u32,
    names: Option<&[(&str, u32)]>,
    expr: &str,
    field: &str,
    normalise: fn(u32) -> u32,
) -> Result<Vec<u32>> {
    let mut matched = BTreeSet::new();

    for item in raw.split(',') {
        let term = js_trim(item);
        if term.is_empty() {
            return Err(fail(expr, &format!("{field} has an empty term.")));
        }

        let mut pieces = term.split('/');
        let range_part = pieces.next().unwrap_or_default();
        let step_part = pieces.next();
        if pieces.next().is_some() {
            return Err(fail(
                expr,
                &format!("{field} has more than one step in \"{term}\"."),
            ));
        }

        let step = match step_part {
            None => 1,
            Some(step_part) => match plain_integer(step_part) {
                Some(step) if step >= 1 => usize::try_from(step).unwrap_or(usize::MAX),
                _ => {
                    return Err(fail(
                        expr,
                        &format!(
                            "{field} has a step that is not a positive whole number: \"{step_part}\"."
                        ),
                    ));
                }
            },
        };

        let (from, to) = if range_part == "*" {
            (min, max)
        } else if range_part.contains('-') {
            let mut bounds = range_part.split('-');
            let lo = bounds.next().unwrap_or_default();
            let hi = bounds.next().unwrap_or_default();
            if bounds.next().is_some() {
                return Err(fail(
                    expr,
                    &format!("{field} has a malformed range \"{range_part}\"."),
                ));
            }
            let from = parse_value(lo, min, max, names, expr, field)?;
            let to = parse_value(hi, min, max, names, expr, field)?;
            if from > to {
                return Err(fail(
                    expr,
                    &format!("{field} range \"{range_part}\" runs backwards."),
                ));
            }
            (from, to)
        } else {
            let from = parse_value(range_part, min, max, names, expr, field)?;
            // A bare value with a step runs to the end of the range; without
            // one it is just itself.
            (from, if step_part.is_none() { from } else { max })
        };

        matched.extend((from..=to).step_by(step).map(normalise));
    }

    Ok(matched.into_iter().collect())
}

/// Parses a five-field expression, and validates `tz` while a caller is still
/// holding the request that supplied it.
///
/// Both failures are `config` rather than `invalid_input` so the REST layer
/// maps them to the same 422 naming the field: an unparseable schedule and an
/// unknown zone are the same mistake from the operator's side, and neither may
/// become a job that silently never fires.
pub fn parse_cron(expr: &str, tz: Option<&str>) -> Result<CronSpec> {
    let tz = match tz {
        None => None,
        Some(name) => Some(name.parse::<Tz>().map_err(|_| {
            GhostError::new(ErrorKind::Config, format!("Unknown timezone \"{name}\"."))
                .with_detail("tz", name)
        })?),
    };

    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        let count = fields.len();
        return Err(fail(
            expr,
            &if count > 5 {
                format!("expected 5 fields and got {count}. Seconds and years are not supported.")
            } else {
                format!("expected 5 fields and got {count}.")
            },
        ));
    }
    let [minute, hour, dom, month, dow] = fields[..] else {
        return Err(fail(expr, "expected 5 fields."));
    };

    Ok(CronSpec {
        minutes: parse_field(minute, 0, 59, None, expr, "minute", identity)?,
        hours: parse_field(hour, 0, 23, None, expr, "hour", identity)?,
        days_of_month: parse_field(dom, 1, 31, None, expr, "day-of-month", identity)?,
        months: parse_field(month, 1, 12, Some(MONTH_NAMES), expr, "month", identity)?,
        // 7 is Sunday in every dialect that accepts it, and 0 already is here.
        days_of_week: parse_field(dow, 0, 7, Some(DAY_NAMES), expr, "day-of-week", |v| v % 7)?,
        dom_restricted: dom != "*",
        dow_restricted: dow != "*",
        tz,
    })
}

fn identity(value: u32) -> u32 {
    value
}

/// Whether a calendar day satisfies the month and the day-of-* rule.
fn day_matches(spec: &CronSpec, date: NaiveDate) -> bool {
    if !spec.months.contains(&date.month()) {
        return false;
    }

    let day_hit = spec.days_of_month.contains(&date.day());
    let weekday_hit = spec
        .days_of_week
        .contains(&date.weekday().num_days_from_sunday());

    // The rule: restricted on both sides means either may satisfy it.
    // Restricted on one means the other is `*`, whose set is full, so the AND
    // below is the same answer written once.
    if spec.dom_restricted && spec.dow_restricted {
        day_hit || weekday_hit
    } else {
        day_hit && weekday_hit
    }
}

/// The first instant strictly after `after_ms` that the expression matches, or
/// `None` when there is none within the search bound.
///
/// `None` is a real answer, not a failure: `0 0 30 2 *` is a legal expression
/// that never matches, and a caller writes it down as "unscheduled, here is
/// why" rather than retrying forever.
///
/// The walk is over calendar days rather than minutes because a day is the
/// coarsest unit the day-of-* rules decide, and because a calendar day advances
/// the same way in every zone, the one piece of local-time arithmetic that is
/// safe. Only a day that matches pays for instant conversion.
pub fn next_cron_run(spec: &CronSpec, after_ms: i64) -> Option<i64> {
    match spec.tz {
        Some(tz) => next_in_zone(spec, after_ms, &tz),
        None => next_in_zone(spec, after_ms, &Local),
    }
}

fn next_in_zone<Z: TimeZone>(spec: &CronSpec, after_ms: i64, zone: &Z) -> Option<i64> {
    let after = DateTime::<Utc>::from_timestamp_millis(after_ms)?;
    let mut date = after.with_timezone(zone).date_naive();
    let mut probes = 0;

    for _ in 0..MAX_SEARCH_DAYS {
        if day_matches(spec, date) {
            for &hour in &spec.hours {
                for &minute in &spec.minutes {
                    probes += 1;
                    if probes > MAX_SLOT_PROBES {
                        return None;
                    }
                    let wall = date.and_hms_opt(hour, minute, 0)?;
                    // `None` is a wall-clock time the zone skipped. Not an
                    // error, and not a reason to stop: the job simply has no
                    // occurrence in the hour that did not happen, and the next
                    // slot is the right answer.
                    if let Some(instant) = instant_of_local(zone, wall)
                        && instant > after_ms
                    {
                        return Some(instant);
                    }
                }
            }
        }
        date = date.succ_opt()?;
    }

    None
}

/// The instant at which the local wall clock reads exactly this, or `None`
/// when the time does not exist. Where the time exists twice, the **earlier**
/// instant wins, so a job at 01:30 on a fall-back night runs once rather than
/// twice.
fn instant_of_local<Z: TimeZone>(zone: &Z, wall: chrono::NaiveDateTime) -> Option<i64> {
    match zone.from_local_datetime(&wall) {
        MappedLocalTime::Single(instant) => Some(instant.timestamp_millis()),
        MappedLocalTime::Ambiguous(earlier, later) => {
            Some(earlier.timestamp_millis().min(later.timestamp_millis()))
        }
        MappedLocalTime::None => None,
    }
}
