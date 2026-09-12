/**
 * The cursor that survives a reload.
 *
 * Every failure mode here produces the same symptom — a reload that shows a
 * blank transcript instead of the turn that was streaming — and none of them
 * produces an error. So the cases are the ones where the honest answer is 0:
 * a different session, a corrupt entry, and storage that throws.
 */

import { describe, expect, it } from 'vitest';

import { clearCursor, readCursor, writeCursor } from '@/lib/cursor.js';

/** Storage in a Map, so the assertions are about the module and not jsdom. */
function memory(initial: Record<string, string> = {}): Storage {
  const entries = new Map(Object.entries(initial));
  return {
    get length(): number {
      return entries.size;
    },
    clear: () => {
      entries.clear();
    },
    getItem: (key: string) => entries.get(key) ?? null,
    key: (index: number) => [...entries.keys()][index] ?? null,
    removeItem: (key: string) => {
      entries.delete(key);
    },
    setItem: (key: string, value: string) => {
      entries.set(key, value);
    },
  };
}

/** Storage that throws on every access, as it does under Safari private mode. */
const hostile: Storage = {
  get length(): number {
    throw new Error('denied');
  },
  clear: () => {
    throw new Error('denied');
  },
  getItem: () => {
    throw new Error('denied');
  },
  key: () => {
    throw new Error('denied');
  },
  removeItem: () => {
    throw new Error('denied');
  },
  setItem: () => {
    throw new Error('denied');
  },
};

describe('the reconnect cursor', () => {
  it('round-trips for the session it was written for', () => {
    const storage = memory();

    writeCursor('web:1', 42, storage);

    expect(readCursor('web:1', storage)).toBe(42);
  });

  it('round-trips a zero, which is not the same as an absence', () => {
    const storage = memory();

    // "This tab has been here and nothing is in storage yet" — a reload during
    // a session's first turn, and the case that most needs the ring.
    writeCursor('web:1', 0, storage);

    expect(readCursor('web:1', storage)).toBe(0);
  });

  it('is absent for a different session', () => {
    const storage = memory();
    writeCursor('web:1', 42, storage);

    // A sequence number from another conversation addresses nothing in this
    // one's replay ring.
    expect(readCursor('web:2', storage)).toBeUndefined();
  });

  it('is absent when nothing was ever written', () => {
    expect(readCursor('web:1', memory())).toBeUndefined();
  });

  it('is absent for an entry that is not a cursor', () => {
    expect(
      readCursor('web:1', memory({ 'ghostai.cursor': 'not json' })),
    ).toBeUndefined();
    expect(
      readCursor(
        'web:1',
        memory({ 'ghostai.cursor': '{"sessionKey":"web:1"}' }),
      ),
    ).toBeUndefined();
    expect(
      readCursor(
        'web:1',
        memory({ 'ghostai.cursor': '{"sessionKey":"web:1","lastSeq":-2}' }),
      ),
    ).toBeUndefined();
    expect(
      readCursor(
        'web:1',
        memory({ 'ghostai.cursor': '{"sessionKey":"web:1","lastSeq":1.5}' }),
      ),
    ).toBeUndefined();
  });

  it('survives storage that refuses to answer', () => {
    // A tab that cannot rebuild its in-flight turn beats a tab that throws
    // while rendering one.
    expect(readCursor('web:1', hostile)).toBeUndefined();
    expect(() => {
      writeCursor('web:1', 4, hostile);
    }).not.toThrow();
    expect(() => {
      clearCursor(hostile);
    }).not.toThrow();
  });

  it('survives storage that is not there at all', () => {
    expect(readCursor('web:1', undefined)).toBeUndefined();
    expect(() => {
      writeCursor('web:1', 4, undefined);
    }).not.toThrow();
  });

  it('clears', () => {
    const storage = memory();
    writeCursor('web:1', 5, storage);

    clearCursor(storage);

    expect(readCursor('web:1', storage)).toBeUndefined();
  });
});
