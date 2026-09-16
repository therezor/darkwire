/**
 * The small formatters the transcript needs.
 *
 * Here rather than inline in a component for one reason: every one of them has
 * a boundary that is easy to get wrong and invisible when it is — 59.9 seconds
 * rounding to "60s", a zero-byte file reading as "NaN B", a tool that finished
 * in under a millisecond reporting "0ms" as though it never ran. They are three
 * lines each and they have tests.
 *
 * **The locale is a parameter, never the machine's.** The arithmetic — where a
 * minute becomes an hour, when an interval stops beating a date — lives in
 * `@darkwire/i18n` because the CLI decides it identically. What stays here is the
 * *wording*, which the two surfaces deliberately disagree about: this renders
 * `8,192` where the terminal renders `1.2k`, and `just now` where bare `Intl`
 * would say `now`.
 */

import {
  durationParts,
  formatNumber,
  formatDate as formatDateIn,
  formatDateTime as formatDateTimeIn,
  relativeSpan,
} from '@darkwire/i18n';
import type { TFunction } from 'i18next';

/**
 * A duration a human reads at a glance.
 *
 * Sub-second stays in milliseconds because that is the resolution that
 * distinguishes a cache hit from a request; past a minute the seconds are
 * still shown, because "2m" for a 2m59s command reads as a rounding error.
 *
 * No locale: every form here is a number and a one-letter unit, and the
 * boundaries come from `durationParts` — which the CLI's `formatDuration` now
 * reads too, so the two cannot drift on where an hour begins.
 */
export function formatDuration(ms: number): string {
  const parts = durationParts(ms);
  if (parts === undefined) return '—';

  switch (parts.unit) {
    case 'ms':
      return `${String(parts.value)}ms`;
    case 'second':
      return parts.fractional
        ? `${parts.value.toFixed(1)}s`
        : `${String(parts.value)}s`;
    case 'minute':
      return `${String(parts.value)}m ${String(parts.remainder).padStart(2, '0')}s`;
    case 'hour':
      return `${String(parts.value)}h ${String(parts.remainder).padStart(2, '0')}m`;
  }
}

/**
 * How long ago something happened, for a list that is read at a glance.
 *
 * `now` is a parameter rather than a `Date.now()` call so the boundaries can be
 * tested without a fake clock, and so a list re-rendering mid-scroll cannot show
 * two rows measured against two different instants.
 *
 * A timestamp slightly in the *future* reads as "just now" rather than
 * "-3s ago": the server and the browser have separate clocks, and a few seconds
 * of skew is normal rather than an error worth rendering.
 *
 * Worded from the bundle rather than by `Intl.RelativeTimeFormat` directly: the
 * bare narrow output is `1 min. ago`, and these rows are a sidebar column where
 * `1m ago` is the density that was chosen. The *thresholds* are `relativeSpan`'s
 * and therefore shared; only the phrasing is the web's own.
 */
export function formatRelativeTime(
  atMs: number,
  now: number,
  locale: string,
  t: TFunction,
): string {
  const span = relativeSpan(atMs, now);

  switch (span.kind) {
    case 'unknown':
      return '—';
    case 'now':
      return t('time.justNow');
    case 'date':
      return formatDate(atMs, locale);
    case 'ago':
      return t(AGO_KEYS[span.unit] ?? 'time.daysAgo', { count: span.value });
  }
}

/**
 * Keyed by the unit `relativeSpan` reports rather than switched on, so a unit it
 * grows later is a missing key the strict handler reports — not a silent fall
 * through to days.
 */
const AGO_KEYS: Partial<
  Record<
    Intl.RelativeTimeFormatUnit,
    'time.minutesAgo' | 'time.hoursAgo' | 'time.daysAgo'
  >
> = {
  minute: 'time.minutesAgo',
  hour: 'time.hoursAgo',
  day: 'time.daysAgo',
};

/** An absolute date, in the install's locale and zone, for anything older than a week. */
export function formatDate(
  atMs: number,
  locale: string,
  timeZone?: string,
): string {
  return formatDateIn(atMs, locale, timeZone);
}

/**
 * A date *and* a time, with the zone named.
 *
 * The one to reach for whenever the time of day is the answer rather than
 * context — a scheduled job's next run, a run's start. `formatDate` drops it,
 * which is correct for a file's modified date and was silently wrong for
 * "Next run", where it rendered `8 Aug 2026` for a field that exists to say
 * *when*.
 */
export function formatDateTime(
  atMs: number,
  locale: string,
  timeZone?: string,
): string {
  return formatDateTimeIn(atMs, locale, timeZone);
}

/**
 * A token count with thousands separators.
 *
 * Through `Intl.NumberFormat` with the locale passed in. Grouping by regex is
 * the tempting alternative, because `toLocaleString()` on a machine set to
 * `de-DE` renders 8192 as `8.192`, a number that reads as eight in the one
 * panel whose job is a legible budget. The problem is not that grouping is
 * locale-aware; it is that the locale would be *implicit*, and so would
 * whatever the machine happens to be set to.
 */
export function formatTokens(tokens: number, locale: string): string {
  if (!Number.isFinite(tokens)) return '—';
  return formatNumber(Math.round(tokens), locale);
}

const UNITS = ['B', 'kB', 'MB', 'GB', 'TB'] as const;

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—';
  if (bytes < 1000) return `${String(Math.round(bytes))} B`;

  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }

  return `${value < 10 ? value.toFixed(1) : String(Math.round(value))} ${UNITS[unit] ?? 'B'}`;
}

/**
 * Tool arguments as one line, for the collapsed header.
 *
 * The arguments are whatever the model emitted — an object when the JSON
 * parsed, the raw string when it did not — so this has to survive both. It is a
 * preview and not a serialisation: it is truncated, and the expanded card shows
 * the real thing.
 */
export function summariseArgs(args: unknown, maxChars = 80): string {
  if (args === undefined || args === null) return '';

  const text = typeof args === 'string' ? args : stringify(args);
  const oneLine = text.replace(/\s+/g, ' ').trim();
  return oneLine.length <= maxChars
    ? oneLine
    : `${oneLine.slice(0, maxChars - 1)}…`;
}

/** Pretty JSON when it is JSON, and the string itself when it never was. */
export function formatArgs(args: unknown): string {
  if (typeof args === 'string') return args;
  return stringify(args, 2);
}

function stringify(value: unknown, space?: number): string {
  // `JSON.stringify` is typed as returning `string` and returns `undefined` for
  // exactly these two. Neither can arrive from a JSON wire format, but `args` is
  // `unknown`, so the guard is here rather than in the type system.
  if (value === undefined || typeof value === 'function') return typeof value;

  try {
    return JSON.stringify(value, null, space);
  } catch {
    // A cycle, or a BigInt. Neither can reach here from a JSON wire format,
    // but `args` is typed `unknown` and this is the honest handling of that.
    // Not `String(value)`: on the object that just failed to serialise, that
    // produces `[object Object]`, which tells the reader nothing at all.
    return '[unserialisable]';
  }
}
