import { afterEach, describe, expect, it, vi } from 'vitest';

import { newUuid } from '#src/uuid.js';

/** The canonical form, with the version and variant nibbles pinned. */
const UUID_V7 =
  /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

describe('newUuid', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('mints a canonically formatted v7', () => {
    expect(newUuid()).toMatch(UUID_V7);
  });

  it('sets the version to 7 and the variant to 0b10', () => {
    for (let attempt = 0; attempt < 200; attempt += 1) {
      const uuid = newUuid();
      // Byte 6's high nibble, and byte 8's two high bits.
      expect(uuid[14]).toBe('7');
      expect(['8', '9', 'a', 'b']).toContain(uuid[19]);
    }
  });

  /**
   * The whole reason this is v7 rather than v4: the first 48 bits are the clock,
   * so a later id is a larger string. Ordering *within* one millisecond is
   * deliberately not guaranteed — see the module docblock — so the clock is
   * advanced between the two.
   */
  it('sorts in creation order as a plain string', () => {
    vi.useFakeTimers();

    vi.setSystemTime(new Date('2020-01-01T00:00:00.000Z'));
    const first = newUuid();

    vi.setSystemTime(new Date('2020-01-01T00:00:00.001Z'));
    const second = newUuid();

    vi.setSystemTime(new Date('2026-08-01T00:00:00.000Z'));
    const third = newUuid();

    expect([third, first, second].sort()).toEqual([first, second, third]);
  });

  it('encodes the timestamp in the leading 48 bits', () => {
    vi.useFakeTimers();
    const ms = Date.UTC(2026, 7, 1, 12, 34, 56, 789);
    vi.setSystemTime(new Date(ms));

    const uuid = newUuid();
    const timestamp = Number.parseInt(uuid.slice(0, 8) + uuid.slice(9, 13), 16);

    expect(timestamp).toBe(ms);
  });

  it('does not repeat itself', () => {
    const minted = new Set(Array.from({ length: 1000 }, () => newUuid()));
    expect(minted.size).toBe(1000);
  });
});
