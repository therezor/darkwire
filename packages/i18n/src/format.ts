/**
 * The locale-aware primitives, shared; the presentation is not.
 *
 * `packages/web` renders a token budget as `8,192` and the CLI renders the same
 * number as `1.2k`, and that difference is deliberate — a settings panel has a
 * column to spend and a terminal status line does not. So what is shared here is
 * the part that is *locale* knowledge (which separator, which unit word, which
 * plural category) and not the part that is *layout* judgement.
 *
 * Every function takes the locale explicitly. That is the entire fix for the
 * bug the old `formatTokens` was working around: it hand-rolled comma grouping
 * because `toLocaleString()` on a machine set to `de-DE` renders 8192 as
 * `8.192`, which reads as eight in the one panel whose job is making a budget
 * legible. The problem was never that grouping is locale-aware — it is that the
 * locale was *implicit*, and so was whatever the machine happened to be set to.
 * Passed in, it is the install's locale on every machine.
 *
 * The `Intl` constructors are memoised because they are expensive to build and
 * are called per row of a transcript.
 */

/** A number with the locale's grouping separator. */
export function formatNumber(value: number, locale: string): string {
  if (!Number.isFinite(value)) return '—';
  return numberFormat(locale).format(value);
}

/** A number shortened to three significant digits — `1.2K`, `8.2K`, `1.1M`. */
export function formatCompactNumber(value: number, locale: string): string {
  if (!Number.isFinite(value)) return '—';
  return compactNumberFormat(locale).format(value);
}

/**
 * An absolute date, for anything too old to phrase as an interval.
 *
 * `timeZone` is optional and omitting it means the host zone, which is what
 * every `Intl` constructor already does — so a caller that has no opinion is
 * unchanged. A caller that does have one passes the install's `ui.timezone`,
 * and the point of threading it this far is the same as threading the locale:
 * an implicit zone is whatever the machine happens to be set to, and two
 * machines then disagree about a date that is a single instant.
 */
export function formatDate(
  atMs: number,
  locale: string,
  timeZone?: string,
): string {
  if (!Number.isFinite(atMs)) return '—';
  return dateFormat(locale, timeZone).format(new Date(atMs));
}

/**
 * A date *and* a time, with the zone named.
 *
 * Separate from `formatDate` rather than an option on it, because the two
 * answer different questions and the wrong one is silently wrong: a file's
 * modified date reads better without a time, and a scheduled job's next run is
 * *only* about the time — rendered through `formatDate` it reads `8 Aug 2026`
 * and drops the one field it exists for.
 *
 * `timeZoneName: 'short'` is not decoration. Once an install can render in a
 * zone that is not the reader's own, an unlabelled `09:00` is a number the
 * reader will assume is their clock. The label is what makes it checkable.
 */
export function formatDateTime(
  atMs: number,
  locale: string,
  timeZone?: string,
): string {
  if (!Number.isFinite(atMs)) return '—';
  return dateTimeFormat(locale, timeZone).format(new Date(atMs));
}

/**
 * Which interval an instant falls into, without saying it in any language.
 *
 * Split from the wording because the two halves belong to different owners. The
 * *thresholds* are shared — a minute is a minute in every locale, and the rules
 * about when to stop counting are product decisions rather than language ones.
 * The *wording* is not: the sidebar says `just now` where the bare `Intl` output
 * says `now`, and that is a copy decision the web app is entitled to keep.
 *
 * `now` is a parameter rather than a `Date.now()` call so the boundaries can be
 * tested without a fake clock, and so a list re-rendering mid-scroll cannot show
 * two rows measured against two different instants.
 */
export type RelativeSpan =
  /** Inside the last minute. The caller supplies its own wording. */
  | { readonly kind: 'now' }
  /** Old enough that an interval reads worse than a date. */
  | { readonly kind: 'date' }
  /** Not a timestamp at all. */
  | { readonly kind: 'unknown' }
  | {
      readonly kind: 'ago';
      readonly value: number;
      readonly unit: Intl.RelativeTimeFormatUnit;
    };

export function relativeSpan(atMs: number, now: number): RelativeSpan {
  if (!Number.isFinite(atMs)) return { kind: 'unknown' };

  const elapsed = now - atMs;

  // A timestamp slightly in the *future* is the present rather than a negative
  // interval: the server and the browser have separate clocks, and a few
  // seconds of skew is normal rather than an error worth rendering.
  if (elapsed < 60_000) return { kind: 'now' };

  const minutes = Math.floor(elapsed / 60_000);
  if (minutes < 60) return { kind: 'ago', value: minutes, unit: 'minute' };

  const hours = Math.floor(minutes / 60);
  if (hours < 24) return { kind: 'ago', value: hours, unit: 'hour' };

  const days = Math.floor(hours / 24);
  // Past a week the relative form stops being informative — "23d ago" is worse
  // than a date, because nobody counts back three weeks in their head.
  return days < 7
    ? { kind: 'ago', value: days, unit: 'day' }
    : { kind: 'date' };
}

/**
 * An interval, worded.
 *
 * `narrow` by default because that is what both surfaces want — `5m ago` in a
 * sidebar row and in a terminal line alike. `long` is there for anywhere with
 * room to spell it out.
 */
export function formatRelativeSpan(
  span: RelativeSpan,
  locale: string,
  style: Intl.RelativeTimeFormatStyle = 'narrow',
): string | undefined {
  if (span.kind !== 'ago') return undefined;
  return relativeTimeFormat(locale, style).format(-span.value, span.unit);
}

/**
 * A duration broken into its parts, for a caller to word.
 *
 * Returns numbers rather than a string because the two surfaces disagree about
 * the wording and agree about the arithmetic — and the arithmetic is the half
 * with the boundaries that are easy to get wrong and invisible when they are:
 * 59.9 seconds rounding to "60s", a sub-millisecond tool call reporting "0ms"
 * as though it never ran.
 */
interface DurationParts {
  readonly unit: 'ms' | 'second' | 'minute' | 'hour';
  /** The leading number, already rounded for display. */
  readonly value: number;
  /** The remainder in the next unit down, for `2m 30s`. Zero above an hour. */
  readonly remainder: number;
  /** True below ten seconds, where one decimal is worth showing. */
  readonly fractional: boolean;
}

export function durationParts(ms: number): DurationParts | undefined {
  if (!Number.isFinite(ms) || ms < 0) return undefined;
  if (ms < 1000) {
    return {
      unit: 'ms',
      value: Math.round(ms),
      remainder: 0,
      fractional: false,
    };
  }

  const totalSeconds = Math.floor(ms / 1000);
  if (totalSeconds < 60) {
    // One decimal below ten seconds, where the difference is worth seeing.
    const fractional = totalSeconds < 10;
    return {
      unit: 'second',
      value: fractional ? Math.round((ms / 1000) * 10) / 10 : totalSeconds,
      remainder: 0,
      fractional,
    };
  }

  const minutes = Math.floor(totalSeconds / 60);
  if (minutes < 60) {
    return {
      unit: 'minute',
      value: minutes,
      remainder: totalSeconds % 60,
      fractional: false,
    };
  }

  const hours = Math.floor(minutes / 60);
  return {
    unit: 'hour',
    value: hours,
    remainder: minutes % 60,
    fractional: false,
  };
}

/**
 * Which CLDR plural category a count falls into for this locale.
 *
 * i18next resolves plurals through the same `Intl.PluralRules`, so this exists
 * for the places that build a string outside a resource — not as a second
 * pluralisation scheme.
 */
export function pluralCategory(
  count: number,
  locale: string,
): Intl.LDMLPluralRule {
  return pluralRules(locale).select(count);
}

// Memoised constructors

const numberFormats = new Map<string, Intl.NumberFormat>();
const compactNumberFormats = new Map<string, Intl.NumberFormat>();
const dateFormats = new Map<string, Intl.DateTimeFormat>();
const dateTimeFormats = new Map<string, Intl.DateTimeFormat>();
const relativeTimeFormats = new Map<string, Intl.RelativeTimeFormat>();
const pluralRuleSets = new Map<string, Intl.PluralRules>();

function numberFormat(locale: string): Intl.NumberFormat {
  return cached(numberFormats, locale, () => new Intl.NumberFormat(locale));
}

function compactNumberFormat(locale: string): Intl.NumberFormat {
  return cached(
    compactNumberFormats,
    locale,
    () =>
      new Intl.NumberFormat(locale, {
        notation: 'compact',
        compactDisplay: 'short',
        maximumSignificantDigits: 2,
      }),
  );
}

/**
 * The zone as part of the memo key, not just as an option.
 *
 * Keyed on both because the cache is keyed on everything the constructor was
 * given — a map keyed on the locale alone would hand a caller asking for Tokyo
 * the formatter built earlier for UTC, and the bug would be invisible until
 * someone compared two rows.
 */
function formatterKey(locale: string, timeZone: string | undefined): string {
  return timeZone === undefined ? locale : `${locale}/${timeZone}`;
}

/** `{ timeZone }` only when there is one — passing `undefined` is not the same as omitting it. */
function zoneOption(timeZone: string | undefined): { timeZone?: string } {
  return timeZone === undefined ? {} : { timeZone };
}

function dateFormat(locale: string, timeZone?: string): Intl.DateTimeFormat {
  return cached(
    dateFormats,
    formatterKey(locale, timeZone),
    () =>
      new Intl.DateTimeFormat(locale, {
        ...zoneOption(timeZone),
        year: 'numeric',
        month: 'short',
        day: 'numeric',
      }),
  );
}

function dateTimeFormat(
  locale: string,
  timeZone?: string,
): Intl.DateTimeFormat {
  return cached(
    dateTimeFormats,
    formatterKey(locale, timeZone),
    () =>
      new Intl.DateTimeFormat(locale, {
        ...zoneOption(timeZone),
        year: 'numeric',
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
        timeZoneName: 'short',
      }),
  );
}

function relativeTimeFormat(
  locale: string,
  style: Intl.RelativeTimeFormatStyle,
): Intl.RelativeTimeFormat {
  return cached(
    relativeTimeFormats,
    `${locale}/${style}`,
    () => new Intl.RelativeTimeFormat(locale, { numeric: 'auto', style }),
  );
}

function pluralRules(locale: string): Intl.PluralRules {
  return cached(pluralRuleSets, locale, () => new Intl.PluralRules(locale));
}

function cached<T>(store: Map<string, T>, key: string, build: () => T): T {
  const existing = store.get(key);
  if (existing !== undefined) return existing;

  const built = build();
  store.set(key, built);
  return built;
}
