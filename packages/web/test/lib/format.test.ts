/**
 * The formatters, at their boundaries.
 *
 * Each of these is three lines with one place it can be wrong, and the wrong
 * version is invisible: 59.9 seconds reading as "60s", a zero-byte upload
 * reading as "NaN B", a tool whose arguments failed to serialise reading as
 * "[object Object]".
 */

import { createWebI18n } from '@ghostwire/i18n/web';
import { describe, expect, it } from 'vitest';

import {
  formatArgs,
  formatBytes,
  formatDate,
  formatDuration,
  formatRelativeTime,
  formatTokens,
  summariseArgs,
} from '@/lib/format.js';

/**
 * Pinned to English, and a real instance rather than a stub.
 *
 * The locale is a parameter on every formatter now, which is the whole point:
 * these assertions compare separators and wording, and inheriting the machine's
 * locale is what made `formatTokens` hand-roll its grouping in the first place.
 * A real `t` rather than `(key) => key` because the relative-time wording *is*
 * the thing under test — a stub would assert the keys exist, not that they read
 * correctly.
 */
const EN = 'en';
const t = createWebI18n(EN).getFixedT(null, 'web');

describe('formatDuration', () => {
  it('keeps milliseconds below a second', () => {
    expect(formatDuration(0)).toBe('0ms');
    expect(formatDuration(999)).toBe('999ms');
  });

  it('shows a decimal below ten seconds, where the difference is worth seeing', () => {
    expect(formatDuration(1000)).toBe('1.0s');
    expect(formatDuration(9949)).toBe('9.9s');
    expect(formatDuration(10_000)).toBe('10s');
    expect(formatDuration(59_999)).toBe('59s');
  });

  it('keeps the seconds past a minute', () => {
    // "2m" for a 2m59s command reads as a rounding error.
    expect(formatDuration(179_000)).toBe('2m 59s');
    expect(formatDuration(60_000)).toBe('1m 00s');
  });

  it('folds into hours, and refuses nonsense', () => {
    expect(formatDuration(3_900_000)).toBe('1h 05m');
    expect(formatDuration(-1)).toBe('—');
    expect(formatDuration(Number.NaN)).toBe('—');
  });
});

describe('formatBytes', () => {
  it('handles the empty file and the boundary', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(999)).toBe('999 B');
    expect(formatBytes(1000)).toBe('1.0 kB');
  });

  it('climbs the units and stops at the top of the table', () => {
    expect(formatBytes(1_500_000)).toBe('1.5 MB');
    expect(formatBytes(12_000_000)).toBe('12 MB');
    expect(formatBytes(5e15)).toBe('5000 TB');
  });

  it('refuses nonsense', () => {
    expect(formatBytes(-5)).toBe('—');
  });
});

describe('tool arguments', () => {
  it('collapse to one line for the collapsed header', () => {
    expect(summariseArgs({ command: 'ls\n-la' })).toBe(
      '{"command":"ls\\n-la"}',
    );
  });

  it('truncate rather than wrap', () => {
    const long = summariseArgs({ text: 'x'.repeat(200) }, 20);

    expect(long).toHaveLength(20);
    expect(long.endsWith('…')).toBe(true);
  });

  it('survive the string a model emitted when its JSON did not parse', () => {
    expect(summariseArgs('{"command": ')).toBe('{"command":');
    expect(formatArgs('{"command": ')).toBe('{"command": ');
  });

  it('are nothing at all when there are none', () => {
    expect(summariseArgs(undefined)).toBe('');
    expect(summariseArgs(null)).toBe('');
  });

  it('pretty-print for the expanded card', () => {
    expect(formatArgs({ a: 1 })).toBe('{\n  "a": 1\n}');
  });

  it('say so rather than printing [object Object] when serialisation fails', () => {
    const cycle: Record<string, unknown> = {};
    cycle.self = cycle;

    expect(formatArgs(cycle)).toBe('[unserialisable]');
  });
});

describe('formatRelativeTime', () => {
  const now = Date.UTC(2026, 6, 27, 12, 0, 0);

  it('reads anything under a minute as just now', () => {
    expect(formatRelativeTime(now - 1, now, EN, t)).toBe('just now');
    expect(formatRelativeTime(now - 59_999, now, EN, t)).toBe('just now');
  });

  it('steps up through minutes, hours and days', () => {
    expect(formatRelativeTime(now - 60_000, now, EN, t)).toBe('1m ago');
    expect(formatRelativeTime(now - 3_600_000, now, EN, t)).toBe('1h ago');
    expect(formatRelativeTime(now - 3 * 86_400_000, now, EN, t)).toBe('3d ago');
  });

  it('switches to a date past a week, where counting back stops working', () => {
    expect(formatRelativeTime(now - 8 * 86_400_000, now, EN, t)).toBe(
      formatDate(now - 8 * 86_400_000, EN),
    );
  });

  it('reads a timestamp from a slightly fast server as just now, not as negative', () => {
    // The browser and the server keep separate clocks. A few seconds of skew is
    // normal; "-3s ago" is not a thing to render.
    expect(formatRelativeTime(now + 5000, now, EN, t)).toBe('just now');
  });

  it('says nothing rather than Invalid Date for a broken value', () => {
    expect(formatRelativeTime(Number.NaN, now, EN, t)).toBe('—');
    expect(formatDate(Number.NaN, EN)).toBe('—');
  });
});

describe('formatTokens', () => {
  it('groups thousands with a separator the tests can rely on', () => {
    // Through `Intl.NumberFormat` with the locale passed in. The hand-rolled
    // grouping this replaced existed because a bare `toLocaleString()` on a
    // machine set to de-DE returns "8.192", which reads as eight in the one
    // panel whose job is a legible budget — but the fault was the *implicit*
    // locale, not the fact that grouping follows one.
    expect(formatTokens(8192, EN)).toBe('8,192');
    expect(formatTokens(1_234_567, EN)).toBe('1,234,567');
  });

  it('leaves small counts alone', () => {
    expect(formatTokens(0, EN)).toBe('0');
    expect(formatTokens(999, EN)).toBe('999');
  });

  it('survives a broken value', () => {
    expect(formatTokens(Number.NaN, EN)).toBe('—');
  });
});
