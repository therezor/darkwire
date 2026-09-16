/**
 * Wall clock ⇄ instant, and the two days a year it is hard.
 *
 * The DST cases mirror the ones in `darkwire-core`'s `cron.rs` tests on purpose.
 * The two implementations cannot share code — one is Rust and the other runs in
 * the browser — so what keeps them honest is that they are held to the same
 * answers.
 */

import { describe, expect, it } from 'vitest';

import {
  instantFromZonedInput,
  isValidTimeZone,
  zonedInputValue,
} from '#src/zoned-time.js';

const KYIV = 'Europe/Kyiv';

describe('zonedInputValue', () => {
  it('reads an instant as the wall clock in the zone it is given', () => {
    // The same instant, three clocks. A `Date`-based conversion would render
    // whichever one the machine happens to be set to, which is the bug this
    // exists to remove.
    const atMs = Date.parse('2026-01-15T06:30:00Z');
    expect(zonedInputValue(atMs, 'UTC')).toBe('2026-01-15T06:30');
    expect(zonedInputValue(atMs, KYIV)).toBe('2026-01-15T08:30');
    expect(zonedInputValue(atMs, 'Asia/Tokyo')).toBe('2026-01-15T15:30');
  });

  it('pads every field, because the input parses by position', () => {
    expect(zonedInputValue(Date.parse('2026-03-05T04:07:00Z'), 'UTC')).toBe(
      '2026-03-05T04:07',
    );
  });

  it('is blank for a non-instant rather than rendering NaN into the box', () => {
    expect(zonedInputValue(Number.NaN, 'UTC')).toBe('');
    expect(zonedInputValue(Number.POSITIVE_INFINITY, 'UTC')).toBe('');
  });

  it('crosses a date boundary rather than clamping to the day', () => {
    // 22:30 UTC is already tomorrow in Tokyo. Keeping the date fixed would put
    // a job a day out from where the operator pointed.
    expect(
      zonedInputValue(Date.parse('2026-01-15T22:30:00Z'), 'Asia/Tokyo'),
    ).toBe('2026-01-16T07:30');
  });
});

describe('instantFromZonedInput', () => {
  it('reads a wall clock in the zone it is given', () => {
    expect(instantFromZonedInput('2026-01-15T08:30', KYIV)).toBe(
      Date.parse('2026-01-15T06:30:00Z'),
    );
    expect(instantFromZonedInput('2026-01-15T08:30', 'UTC')).toBe(
      Date.parse('2026-01-15T08:30:00Z'),
    );
  });

  it('round-trips with zonedInputValue in both directions', () => {
    for (const iso of [
      '2026-01-15T06:30:00Z',
      '2026-07-15T06:30:00Z',
      '2026-12-31T23:00:00Z',
    ]) {
      const atMs = Date.parse(iso);
      expect(instantFromZonedInput(zonedInputValue(atMs, KYIV), KYIV)).toBe(
        atMs,
      );
    }
  });

  it('tolerates a seconds component the browser may append', () => {
    // A `datetime-local` with a `step` under a minute emits `HH:mm:ss`. The
    // schedule is minute-resolution, so the extra field is read past rather
    // than treated as a parse failure.
    expect(instantFromZonedInput('2026-01-15T08:30:00', KYIV)).toBe(
      Date.parse('2026-01-15T06:30:00Z'),
    );
  });

  it('is null for anything that is not a wall clock', () => {
    for (const bad of [
      '',
      '   ',
      'tomorrow',
      '2026-01-15',
      '15/01/2026 08:30',
    ]) {
      expect(instantFromZonedInput(bad, KYIV)).toBeNull();
    }
  });

  it('is null for numbers that parse but are not a date', () => {
    // `Date.UTC` overflows month 13 into the next year rather than refusing, so
    // without the range check this would silently book a real instant in a
    // month nobody typed.
    expect(instantFromZonedInput('2026-13-05T08:30', KYIV)).toBeNull();
    expect(instantFromZonedInput('2026-01-40T08:30', KYIV)).toBeNull();
    expect(instantFromZonedInput('2026-01-15T25:30', KYIV)).toBeNull();
    expect(instantFromZonedInput('2026-01-15T08:75', KYIV)).toBeNull();
  });

  describe('the two days a year a wall clock is not a time', () => {
    it('is null for an hour the zone skipped', () => {
      // Kyiv goes 03:00 → 04:00 on 2026-03-29, so 03:30 never happens. Rounding
      // it into 04:30 would book an hour from where the operator pointed.
      expect(instantFromZonedInput('2026-03-29T03:30', KYIV)).toBeNull();
      // The minute either side of the gap is real and must still resolve.
      expect(instantFromZonedInput('2026-03-29T02:59', KYIV)).not.toBeNull();
      expect(instantFromZonedInput('2026-03-29T04:00', KYIV)).not.toBeNull();
    });

    it('resolves an ambiguous hour to the earlier instant, so it happens once', () => {
      // Kyiv goes 04:00 → 03:00 on 2026-10-25, so 03:30 happens twice. The
      // earlier one wins; the alternative is a job that runs twice on one night.
      const at = instantFromZonedInput('2026-10-25T03:30', KYIV);
      expect(at).not.toBeNull();
      expect(new Date(at ?? 0).toISOString()).toBe('2026-10-25T00:30:00.000Z');
    });

    it('agrees with itself across the transition in a southern-hemisphere zone', () => {
      // Sydney transitions the other way round in the calendar, which is what
      // catches an implementation that hard-codes the northern direction.
      expect(
        instantFromZonedInput('2026-10-04T02:30', 'Australia/Sydney'),
      ).toBeNull();
      expect(
        instantFromZonedInput('2026-04-05T02:30', 'Australia/Sydney'),
      ).not.toBeNull();
    });
  });
});

describe('isValidTimeZone', () => {
  it('accepts a real zone and refuses one Intl does not know', () => {
    expect(isValidTimeZone('UTC')).toBe(true);
    expect(isValidTimeZone(KYIV)).toBe(true);
    expect(isValidTimeZone('Mars/Base')).toBe(false);
    expect(isValidTimeZone('')).toBe(false);
  });
});
