import { describe, expect, it } from 'vitest';

import {
  durationParts,
  formatCompactNumber,
  formatDate,
  formatNumber,
  formatRelativeSpan,
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

describe('formatCompactNumber', () => {
  it('shortens for a line that has no room to spare', () => {
    expect(formatCompactNumber(1200, 'en')).toBe('1.2K');
    expect(formatCompactNumber(88, 'en')).toBe('88');
    expect(formatCompactNumber(Number.NaN, 'en')).toBe('—');
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
