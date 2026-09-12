/**
 * The list ordering every CRUD screen shares.
 *
 * Three properties are asserted rather than the obvious "it sorts": a group
 * that survives a reversal, a tie broken deterministically, and a new column
 * starting in the direction that column is read. Each is a rule that a
 * hand-rolled comparator on a second screen gets wrong silently — the list
 * still sorts, it just sorts differently from the one next to it.
 */

import { describe, expect, it } from 'vitest';

import {
  filterRows,
  sortBy,
  sortRows,
  type Comparators,
} from '@/components/crud/sort.js';

interface Row {
  readonly name: string;
  readonly size: number;
  readonly folder: boolean;
}

type Key = 'name' | 'size';

const COMPARE: Comparators<Row, Key> = {
  name: (a, b) => a.name.localeCompare(b.name),
  size: (a, b) => a.size - b.size,
};

const rows: readonly Row[] = [
  { name: 'zebra', size: 10, folder: false },
  { name: 'apple', size: 30, folder: false },
  { name: 'docs', size: 0, folder: true },
  { name: 'mango', size: 10, folder: false },
];

const names = (result: readonly Row[]): readonly string[] =>
  result.map((row) => row.name);

describe('sortRows', () => {
  it('orders by the chosen column, ascending', () => {
    expect(
      names(sortRows(rows, { key: 'name', descending: false }, COMPARE)),
    ).toEqual(['apple', 'docs', 'mango', 'zebra']);
  });

  it('reverses when descending', () => {
    expect(
      names(sortRows(rows, { key: 'name', descending: true }, COMPARE)),
    ).toEqual(['zebra', 'mango', 'docs', 'apple']);
  });

  it('does not mutate the array it was given', () => {
    const original = [...rows];
    sortRows(rows, { key: 'size', descending: true }, COMPARE);
    expect(rows).toEqual(original);
  });

  it('keeps a group at the top in both directions', () => {
    const group = (row: Row): number => (row.folder ? 0 : 1);

    // The rule this exists for: a folder is not a small file or an old file, so
    // "largest first" must not scatter it through the list.
    for (const descending of [false, true]) {
      const sorted = sortRows(rows, { key: 'size', descending }, COMPARE, {
        group,
      });
      expect(sorted[0]?.name, `descending=${String(descending)}`).toBe('docs');
    }
  });

  it('breaks a tie with the tiebreak rather than leaving it to the sort', () => {
    // `zebra` and `mango` are both size 10 and arrive in that order. Without a
    // tiebreak they would hold it; with one they are ordered by name, so a
    // refetch that returns them the other way round renders the same.
    const sorted = sortRows(rows, { key: 'size', descending: false }, COMPARE, {
      tiebreak: (a, b) => a.name.localeCompare(b.name),
    });

    expect(names(sorted)).toEqual(['docs', 'mango', 'zebra', 'apple']);
  });

  it('leaves a tie alone when no tiebreak is given', () => {
    const sorted = sortRows(rows, { key: 'size', descending: false }, COMPARE);
    expect(names(sorted).slice(1, 3)).toEqual(['zebra', 'mango']);
  });
});

describe('filterRows', () => {
  it('matches case-insensitively on the chosen text', () => {
    expect(names(filterRows(rows, 'AN', (row) => row.name))).toEqual(['mango']);
  });

  it('returns the rows untouched when the query is blank', () => {
    // Identity, not a copy: an empty filter is the common case and re-allocating
    // on every keystroke of an empty box would defeat every downstream memo.
    expect(filterRows(rows, '   ', (row) => row.name)).toBe(rows);
  });

  it('matches nothing rather than everything when there is no hit', () => {
    expect(filterRows(rows, 'nothing', (row) => row.name)).toEqual([]);
  });
});

describe('sortBy', () => {
  const ASCENDING_FIRST: readonly Key[] = ['name'];

  it('starts a text column ascending', () => {
    expect(sortBy('name', ASCENDING_FIRST)).toEqual({
      key: 'name',
      descending: false,
    });
  });

  it('starts a numeric column descending, because the question is “what is big”', () => {
    expect(sortBy('size', ASCENDING_FIRST)).toEqual({
      key: 'size',
      descending: true,
    });
  });
});
