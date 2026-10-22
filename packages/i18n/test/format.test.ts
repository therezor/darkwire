import { describe, expect, it } from 'vitest';

import {
  durationParts,
  formatDate,
  formatNumber,
  formatRelativeSpan,
  formatTokenCount,
  formatWindowShare,
  pluralCategory,
  relativeSpan,
  type RelativeSpan,
} from '#src/format.js';

describe('formatNumber', () => {
  it('groups with the separator the locale actually uses', () => {
    // The whole point of taking the locale explicitly: the old hand-rolled
    // grouping existed because `toLocaleString()` picked up whatever the
    // *machine* was set to, and rendered 8192 as `8.192` on a German laptop.
    expect(formatNumber(8192, 'en')).toBe('8,192');
    expect(formatNumber(8192, 'de')).toBe('8.192');
  });

  it('keeps a sign and survives what is not a number', () => {
    expect(formatNumber(-1500, 'en')).toBe('-1,500');
    expect(formatNumber(Number.NaN, 'en')).toBe('—');
    expect(formatNumber(Number.POSITIVE_INFINITY, 'en')).toBe('—');
  });
});

describe('formatTokenCount', () => {
  // The same table as `crates/i18n/tests/format.rs`, so the web and the
  // terminal shorten a count identically.
  const vectors: ReadonlyArray<readonly [number, string, string]> = [
    [0, '0', '0'],
    [950, '950', '950'],
    [999, '999', '999'],
    [1_000, '1k', '1k'],
    [1_234, '1.23k', '1,23k'],
    [8_192, '8.19k', '8,19k'],
    [9_995, '10k', '10k'],
    [41_213, '41.2k', '41,2k'],
    [65_536, '65.5k', '65,5k'],
    [99_950, '100k', '100k'],
    [131_072, '131k', '131k'],
    [200_000, '200k', '200k'],
    [999_499, '999k', '999k'],
    [999_950, '1M', '1M'],
    [1_048_576, '1.05M', '1,05M'],
    [12_345_678, '12.3M', '12,3M'],
  ];

  it.each(vectors)('shortens %d to %s, and %s in German', (value, en, de) => {
    expect(formatTokenCount(value, 'en')).toBe(en);
    expect(formatTokenCount(value, 'de')).toBe(de);
  });

  it('keeps a sign and survives what is not a number', () => {
    expect(formatTokenCount(-41_213, 'en')).toBe('-41.2k');
    expect(formatTokenCount(-0.2, 'en')).toBe('0');
    expect(formatTokenCount(Number.NaN, 'en')).toBe('—');
  });
});

describe('formatWindowShare', () => {
  it('is never full until it is over, and never empty while holding anything', () => {
    expect(formatWindowShare(0, 65_536)).toBe('0%');
    expect(formatWindowShare(41_213, 65_536)).toBe('63%');
    expect(formatWindowShare(1, 65_536)).toBe('<1%');
    expect(formatWindowShare(65_535, 65_536)).toBe('99%');
    expect(formatWindowShare(65_536, 65_536)).toBe('100%');
    expect(formatWindowShare(73_400, 65_536)).toBe('112%');
    expect(formatWindowShare(10, 0)).toBe('0%');
  });
});

describe('formatDate', () => {
  const at = Date.parse('2026-07-29T12:00:00Z');

  it('orders the parts the way the locale does', () => {
    expect(formatDate(at, 'en-GB')).toBe('29 Jul 2026');
    expect(formatDate(at, 'en-US')).toBe('Jul 29, 2026');
  });

  it('is a dash rather than `Invalid Date`', () => {
    expect(formatDate(Number.NaN, 'en')).toBe('—');
  });
});

describe('relativeSpan', () => {
  const now = Date.parse('2026-07-29T12:00:00Z');
  const ago = (ms: number): RelativeSpan => relativeSpan(now - ms, now);

  it('picks the unit a person would have picked', () => {
    expect(ago(5 * 60_000)).toEqual({ kind: 'ago', value: 5, unit: 'minute' });
    expect(ago(3 * 3_600_000)).toEqual({ kind: 'ago', value: 3, unit: 'hour' });
    expect(ago(2 * 86_400_000)).toEqual({ kind: 'ago', value: 2, unit: 'day' });
  });

  it('treats the last minute, and a clock skewed forward, as now', () => {
    // The server and the browser have separate clocks, and a few seconds of
    // skew is normal rather than an error worth rendering as "-3s ago".
    expect(ago(0).kind).toBe('now');
    expect(ago(30_000).kind).toBe('now');
    expect(relativeSpan(now + 3000, now).kind).toBe('now');
  });

  it('gives up on intervals once counting back stops being useful', () => {
    expect(ago(6 * 86_400_000).kind).toBe('ago');
    expect(ago(30 * 86_400_000).kind).toBe('date');
  });

  it('says so rather than guessing when the input is not a timestamp', () => {
    expect(relativeSpan(Number.NaN, now).kind).toBe('unknown');
  });
});

describe('formatRelativeSpan', () => {
  it('words an interval narrowly by default, which is what both surfaces want', () => {
    // Byte-identical to the wording the sidebar shipped before this layer
    // existed — the point of `narrow` over the `long` default.
    const span: RelativeSpan = { kind: 'ago', value: 5, unit: 'minute' };

    expect(formatRelativeSpan(span, 'en')).toBe('5m ago');
    expect(formatRelativeSpan(span, 'en', 'long')).toBe('5 minutes ago');
    expect(
      formatRelativeSpan({ kind: 'ago', value: 3, unit: 'hour' }, 'en'),
    ).toBe('3h ago');
    expect(
      formatRelativeSpan({ kind: 'ago', value: 2, unit: 'day' }, 'en'),
    ).toBe('2d ago');
  });

  it('leaves the wording to the caller for everything that is not an interval', () => {
    // `just now` and the date fallback are copy decisions, not locale ones.
    expect(formatRelativeSpan({ kind: 'now' }, 'en')).toBeUndefined();
    expect(formatRelativeSpan({ kind: 'date' }, 'en')).toBeUndefined();
    expect(formatRelativeSpan({ kind: 'unknown' }, 'en')).toBeUndefined();
  });
});

describe('durationParts', () => {
  it('stays in milliseconds below a second, where the resolution matters', () => {
    // That is what distinguishes a cache hit from a request.
    expect(durationParts(850)).toEqual({
      unit: 'ms',
      value: 850,
      remainder: 0,
      fractional: false,
    });
  });

  it('keeps one decimal below ten seconds and drops it above', () => {
    expect(durationParts(9400)).toEqual({
      unit: 'second',
      value: 9.4,
      remainder: 0,
      fractional: true,
    });
    expect(durationParts(42_000)).toEqual({
      unit: 'second',
      value: 42,
      remainder: 0,
      fractional: false,
    });
  });

  it('carries the remainder, so 2m59s does not read as a rounding error', () => {
    expect(durationParts(179_000)).toEqual({
      unit: 'minute',
      value: 2,
      remainder: 59,
      fractional: false,
    });
  });

  it('has an hour branch — the one the CLI copy was missing', () => {
    // `packages/cli` rendered a three-hour turn as `180m 00s`.
    expect(durationParts(3 * 3_600_000 + 5 * 60_000)).toEqual({
      unit: 'hour',
      value: 3,
      remainder: 5,
      fractional: false,
    });
  });

  it('refuses a duration that is not one', () => {
    expect(durationParts(-1)).toBeUndefined();
    expect(durationParts(Number.NaN)).toBeUndefined();
  });

  it('does not report a sub-millisecond call as never having run', () => {
    expect(durationParts(0.4)?.value).toBe(0);
    expect(durationParts(0.6)?.value).toBe(1);
  });
});

describe('pluralCategory', () => {
  it('gives English its two categories', () => {
    expect(pluralCategory(1, 'en')).toBe('one');
    expect(pluralCategory(0, 'en')).toBe('other');
    expect(pluralCategory(5, 'en')).toBe('other');
  });

  it('gives Polish the four it actually has', () => {
    // The case a hand-rolled `count === 1 ? '' : 's'` cannot express, and the
    // reason plural handling belongs in Intl rather than at a call site.
    expect(pluralCategory(1, 'pl')).toBe('one');
    expect(pluralCategory(2, 'pl')).toBe('few');
    expect(pluralCategory(5, 'pl')).toBe('many');
  });
});
