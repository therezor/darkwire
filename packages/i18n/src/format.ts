/**
 * The locale-aware primitives, shared; the presentation is not.
 *
 * What is shared here is the part that is *locale* knowledge (which separator,
 * which unit word, which plural category) and the rules the web and the CLI must
 * agree on, such as how a token count shortens to `41.2k`. Which surface shows
 * the short form and which the exact one is layout judgement, and stays with the
 * surface.
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

/**
 * A token count shortened to three significant digits: `950`, `1.23k`, `41.2k`,
 * `131k`, `1.05M`.
 *
 * The same rule as `format_token_count` in `crates/i18n`, and the two share
 * their test vectors. Not `Intl`'s compact notation, which leaves German
 * thousands unabbreviated (`41.200`, a rounded number that reads as exact) and
 * counts Japanese in 万. The suffix is the SI one in every language; only the
 * decimal separator follows the locale.
 *
 * The unit is picked after rounding, so 999,950 is `1M` rather than `1000k`.
 * Trailing zeros are dropped: `1k`, not `1.00k`.
 */
export function formatTokenCount(value: number, locale: string): string {
  if (!Number.isFinite(value)) return '—';
  const whole = Math.round(Math.abs(value));
  const sign = value < 0 && whole > 0 ? '-' : '';
  if (whole < 1000) return `${sign}${String(whole)}`;

  const { units, decimals, suffix } = shortened(whole);
  const scale = 10 ** decimals;
  const fraction = String(units % scale)
    .padStart(decimals, '0')
    .replace(/0+$/, '');
  const integer = String(Math.floor(units / scale));
  return fraction === ''
    ? `${sign}${integer}${suffix}`
    : `${sign}${integer}${decimalSeparator(locale)}${fraction}${suffix}`;
}

/**
 * How much of a context window is used, as a whole percent: `63%`.
 *
 * Never `100%` while the request still fits, because a full bar that is not
 * over reads as a request about to fail. Never `0%` for a window holding
 * anything: that reads as empty. Past the window the real figure, so `112%`
 * says by how much. `format_window_share` in `crates/i18n` is the same rule.
 */
export function formatWindowShare(used: number, window: number): string {
  if (!(window > 0) || !(used > 0)) return '0%';
  if (used * 100 < window) return '<1%';
  const rounded = Math.round((used * 100) / window);
  return `${String(used < window ? Math.min(rounded, 99) : rounded)}%`;
}

/** Three significant digits of thousands or millions, as integer units. */
function shortened(value: number): {
  units: number;
  decimals: number;
  suffix: string;
} {
  let last = { units: 0, decimals: 0, suffix: 'M' };
  for (const [base, suffix] of [
    [1_000, 'k'],
    [1_000_000, 'M'],
  ] as const) {
    for (const decimals of [2, 1, 0]) {
      const step = base / 10 ** decimals;
      const units = Math.floor((value + step / 2) / step);
      last = { units, decimals, suffix };
      if (units < 1000) return last;
    }
  }
  return last;
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
const decimalSeparators = new Map<string, string>();
const dateFormats = new Map<string, Intl.DateTimeFormat>();
const dateTimeFormats = new Map<string, Intl.DateTimeFormat>();
const relativeTimeFormats = new Map<string, Intl.RelativeTimeFormat>();
const pluralRuleSets = new Map<string, Intl.PluralRules>();

function numberFormat(locale: string): Intl.NumberFormat {
  return cached(numberFormats, locale, () => new Intl.NumberFormat(locale));
}

function decimalSeparator(locale: string): string {
  return cached(
    decimalSeparators,
    locale,
    () =>
      numberFormat(locale)
        .formatToParts(1.5)
        .find((part) => part.type === 'decimal')?.value ?? '.',
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
