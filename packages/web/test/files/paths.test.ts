/**
 * Workspace paths.
 *
 * The whole file exists because one directory has two spellings on the wire —
 * the listing route defaults its query to `.` and answers with `''` — so every
 * case here is about the two collapsing to one. A breadcrumb built from the raw
 * value would show an empty first crumb for `./a`, and a query key built from it
 * would cache the root twice.
 */

import { describe, expect, it } from 'vitest';

import type { FileEntry } from '@ghostwire/protocol';

import {
  breadcrumbs,
  DEFAULT_SORT,
  filterEntries,
  isImage,
  joinPath,
  normalisePath,
  parentOf,
  ROOT_PATH,
  sortEntries,
} from '@/files/paths.js';

describe('normalisePath', () => {
  it('collapses every spelling of the root', () => {
    expect(normalisePath('')).toBe(ROOT_PATH);
    expect(normalisePath('.')).toBe(ROOT_PATH);
    expect(normalisePath('./')).toBe(ROOT_PATH);
    expect(normalisePath('  ')).toBe(ROOT_PATH);
  });

  it('strips a leading ./ and a trailing slash', () => {
    expect(normalisePath('./a/b/')).toBe('a/b');
    expect(normalisePath('a/b')).toBe('a/b');
  });

  it('leaves a traversal alone, because judging it is the jail’s job', () => {
    // A second, weaker copy of the jail's rules in a component is worse than
    // none: it would be the one nobody security-tests.
    expect(normalisePath('../etc')).toBe('../etc');
  });
});

describe('breadcrumbs', () => {
  it('starts at the workspace even at the root', () => {
    expect(breadcrumbs(ROOT_PATH)).toEqual([
      { label: 'workspace', path: ROOT_PATH },
    ]);
  });

  it('accumulates a clickable path per segment', () => {
    expect(breadcrumbs('notes/2026/july')).toEqual([
      { label: 'workspace', path: '' },
      { label: 'notes', path: 'notes' },
      { label: '2026', path: 'notes/2026' },
      { label: 'july', path: 'notes/2026/july' },
    ]);
  });

  it('is unchanged by the other spelling of the same directory', () => {
    expect(breadcrumbs('./notes/')).toEqual(breadcrumbs('notes'));
  });
});

describe('joinPath', () => {
  it('does not produce a leading slash at the root', () => {
    expect(joinPath(ROOT_PATH, 'a.txt')).toBe('a.txt');
    expect(joinPath('.', 'a.txt')).toBe('a.txt');
  });

  it('joins inside a directory', () => {
    expect(joinPath('notes', 'a.txt')).toBe('notes/a.txt');
  });
});

describe('parentOf', () => {
  it('walks up one level, stopping at the root', () => {
    expect(parentOf('a/b/c.txt')).toBe('a/b');
    expect(parentOf('a.txt')).toBe(ROOT_PATH);
    expect(parentOf(ROOT_PATH)).toBe(ROOT_PATH);
  });
});

describe('isImage', () => {
  it('reads the type the server assigned', () => {
    expect(isImage('image/png')).toBe(true);
    expect(isImage('image/svg+xml')).toBe(true);
  });

  it('says no to anything the server would not render inline', () => {
    // `application/octet-stream` is exactly what the server answers for a type
    // it will not let a browser render inline, so an `<img>` around one would
    // draw a broken image over a refusal.
    expect(isImage('application/octet-stream')).toBe(false);
    expect(isImage(undefined)).toBe(false);
  });

  /**
   * The bug this replaced `previewKind` to fix. Every source file in the
   * workspace is `application/octet-stream` in the server's small MIME table,
   * so a browser-side "is this text" check declared all of them unpreviewable.
   * Nothing here answers that question any more — `GET /api/files/text` does,
   * from the bytes.
   */
  it('does not try to decide whether a source file is text', () => {
    expect(isImage('application/octet-stream')).toBe(false);
  });
});

describe('sortEntries', () => {
  const entry = (
    name: string,
    overrides: Partial<FileEntry> = {},
  ): FileEntry => ({
    path: name,
    name,
    isDirectory: false,
    sizeBytes: 0,
    modifiedAtMs: 0,
    ...overrides,
  });

  const listing: readonly FileEntry[] = [
    entry('src', { isDirectory: true }),
    entry('big.log', { sizeBytes: 9000, modifiedAtMs: 100 }),
    entry('a.txt', { sizeBytes: 10, modifiedAtMs: 300 }),
    entry('docs', { isDirectory: true }),
    entry('m.md', { sizeBytes: 500, modifiedAtMs: 200 }),
  ];

  const names = (order: Parameters<typeof sortEntries>[1]): readonly string[] =>
    sortEntries(listing, order).map((item) => item.name);

  it('matches the order the server already answered with, by default', () => {
    expect(names(DEFAULT_SORT)).toEqual([
      'docs',
      'src',
      'a.txt',
      'big.log',
      'm.md',
    ]);
  });

  it('keeps directories first even when the order is reversed', () => {
    // They are not big files or old files, they are where to go next. A
    // "largest first" that scattered them would turn navigating into searching.
    expect(names({ key: 'size', descending: true }).slice(0, 2)).toEqual([
      'docs',
      'src',
    ]);
    expect(names({ key: 'name', descending: true }).slice(0, 2)).toEqual([
      'src',
      'docs',
    ]);
  });

  it('sorts by size and by time within the files', () => {
    expect(names({ key: 'size', descending: true }).slice(2)).toEqual([
      'big.log',
      'm.md',
      'a.txt',
    ]);
    expect(names({ key: 'modified', descending: true }).slice(2)).toEqual([
      'a.txt',
      'm.md',
      'big.log',
    ]);
  });

  it('breaks ties by name rather than leaving them to the sort', () => {
    // Eight zero-byte files a turn just created must not shuffle on refetch.
    const tied = [entry('c'), entry('a'), entry('b')];
    expect(
      sortEntries(tied, { key: 'size', descending: true }).map(
        (item) => item.name,
      ),
    ).toEqual(['a', 'b', 'c']);
  });

  it('does not reorder the array it was given', () => {
    const before = listing.map((item) => item.name);
    sortEntries(listing, { key: 'size', descending: true });
    expect(listing.map((item) => item.name)).toEqual(before);
  });
});

describe('filterEntries', () => {
  const entries: readonly FileEntry[] = [
    {
      path: 'Notes.md',
      name: 'Notes.md',
      isDirectory: false,
      sizeBytes: 1,
      modifiedAtMs: 0,
    },
    {
      path: 'report.csv',
      name: 'report.csv',
      isDirectory: false,
      sizeBytes: 1,
      modifiedAtMs: 0,
    },
  ];

  it('matches anywhere in the name, ignoring case', () => {
    expect(filterEntries(entries, 'note').map((item) => item.name)).toEqual([
      'Notes.md',
    ]);
    expect(filterEntries(entries, 'CSV').map((item) => item.name)).toEqual([
      'report.csv',
    ]);
  });

  it('is the whole listing when nothing was typed', () => {
    expect(filterEntries(entries, '')).toEqual(entries);
    expect(filterEntries(entries, '   ')).toEqual(entries);
  });
});
