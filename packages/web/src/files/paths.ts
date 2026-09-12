/**
 * Workspace paths, as the browser has to handle them.
 *
 * Every path that crosses the API boundary is workspace-relative — the server
 * says so and `WorkspaceJail.relative` enforces it — but "relative" leaves two
 * spellings of the same directory. `GET /api/files` defaults its query to `.`
 * and answers with `path: ''` for the root, because `path.relative(root, root)`
 * is the empty string. So the browser sees both, and a component keying a cache
 * or a breadcrumb on the raw value gets two entries for one directory.
 *
 * Normalising here, once, is the whole reason this file exists. Nothing in it
 * decides whether a path is *allowed*: that is the jail's job, on the server,
 * for every caller. These functions decide what to display and what to ask for.
 */

import type { FileEntry } from '@ghostwire/protocol';

import {
  filterRows,
  sortRows,
  type Comparators,
  type SortOrder as GenericSortOrder,
} from '@/components/crud/sort.js';

/** The workspace root, in the one spelling this package uses. */
export const ROOT_PATH = '';

/**
 * The canonical form: no leading `./`, no trailing slash, `.` collapsed to the
 * empty string. Deliberately *not* a safety check — `../` is passed through
 * unchanged, because rejecting it here would put a second, weaker copy of the
 * jail's rules in a place no security test looks at.
 */
export function normalisePath(path: string): string {
  const trimmed = path
    .trim()
    .replace(/^\.\/+/, '')
    .replace(/\/+$/, '');
  return trimmed === '.' ? ROOT_PATH : trimmed;
}

interface Crumb {
  readonly label: string;
  readonly path: string;
}

/**
 * The trail from the workspace root down to `path`, root included.
 *
 * Root is always the first crumb and is always clickable, so a browser that
 * navigated six levels down has one target to get back rather than six presses
 * of a Back button that is also the browser's.
 */
export function breadcrumbs(path: string): Crumb[] {
  const normalised = normalisePath(path);
  const crumbs: Crumb[] = [{ label: 'workspace', path: ROOT_PATH }];
  if (normalised === ROOT_PATH) return crumbs;

  let accumulated = '';
  for (const segment of normalised.split('/')) {
    if (segment === '') continue;
    accumulated = accumulated === '' ? segment : `${accumulated}/${segment}`;
    crumbs.push({ label: segment, path: accumulated });
  }

  return crumbs;
}

/** `dir` and `name` as one path, without the `//` that `${dir}/${name}` gives at the root. */
export function joinPath(dir: string, name: string): string {
  const normalised = normalisePath(dir);
  return normalised === ROOT_PATH ? name : `${normalised}/${name}`;
}

/** The directory containing `path`, or the root when it is already a top-level entry. */
export function parentOf(path: string): string {
  const normalised = normalisePath(path);
  const cut = normalised.lastIndexOf('/');
  return cut === -1 ? ROOT_PATH : normalised.slice(0, cut);
}

/**
 * Whether to render this file as a picture, from the MIME type the *server*
 * assigned.
 *
 * Never from the extension, and the difference matters: the server's table is
 * deliberately small and answers `application/octet-stream` for anything it does
 * not know, which is also the point at which `/api/media/:token` switches to
 * `Content-Disposition: attachment`. Deciding here from the filename would put
 * an `<img>` around a response the server refuses to let a browser render, and
 * the reader would see a broken image rather than a download link.
 *
 * This is the *only* question the browser answers about a file's type. It used
 * to also decide "is this text", and that was wrong in the direction that
 * mattered: `.py`, `.ts` and `.css` are all `application/octet-stream` in the
 * server's table, so every source file in the workspace was declared
 * unpreviewable. Whether something is text is now decided by
 * `GET /api/files/text`, which looks at the bytes.
 */
export function isImage(mimeType: string | undefined): boolean {
  return mimeType?.startsWith('image/') === true;
}

/**
 * Files whose *name* is the type, because they have no extension.
 *
 * Short on purpose. It is not a registry of every extensionless convention —
 * it is the handful an agent's workspace actually accumulates, and anything
 * missing renders as plain monospace, which is a perfectly good way to read a
 * file.
 */
const NAMED_LANGUAGES: Readonly<Record<string, string>> = {
  dockerfile: 'dockerfile',
  makefile: 'makefile',
};

/**
 * The fence token a file's syntax highlighting should be asked for under.
 *
 * The *extension*, lowercased — `py`, `ts`, `yml` — and deliberately not a
 * resolved grammar name: `highlight.ts` already owns the alias table that maps
 * `py` to Python, and a second copy of it here would be the one that goes stale
 * when a grammar is added there. An extension that table does not know produces
 * no highlighting and no error.
 *
 * This is also why nothing here imports the highlighter: it is a string
 * function, so the editor can label its toolbar without pulling a grammar
 * engine into the entry chunk.
 */
export function languageForFile(name: string): string {
  const named = NAMED_LANGUAGES[name.toLowerCase()];
  if (named !== undefined) return named;

  const dot = name.lastIndexOf('.');
  // `> 0`, not `>= 0`: a leading dot is `.gitignore`, whose "extension" is the
  // whole name.
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : '';
}

export type SortKey = 'name' | 'size' | 'modified';

export type SortOrder = GenericSortOrder<SortKey>;

export const DEFAULT_SORT: SortOrder = { key: 'name', descending: false };

/** Directories rank above files, which is what keeps them at the top in both directions. */
function directoriesFirst(entry: FileEntry): number {
  return entry.isDirectory ? 0 : 1;
}

const COMPARE: Comparators<FileEntry, SortKey> = {
  name: (a, b) => a.name.localeCompare(b.name),
  size: (a, b) => a.sizeBytes - b.sizeBytes,
  modified: (a, b) => a.modifiedAtMs - b.modifiedAtMs,
};

/**
 * One directory's entries, ordered for reading.
 *
 * **Directories stay first in every order, including a reversed one** — see
 * `sortRows`, which is where that rule and the tie-breaking live now.
 *
 * The server already answers directories-first-then-name, so the default order
 * costs nothing and matches the listing exactly; this reorders only once a
 * reader has asked for a different column.
 */
export function sortEntries<T extends FileEntry>(
  entries: readonly T[],
  order: SortOrder,
): readonly T[] {
  return sortRows(entries, order, COMPARE as Comparators<T, SortKey>, {
    group: directoriesFirst,
    tiebreak: (a, b) => a.name.localeCompare(b.name),
  });
}

/** The entries whose name contains `query`, case-insensitively. */
export function filterEntries<T extends FileEntry>(
  entries: readonly T[],
  query: string,
): readonly T[] {
  return filterRows(entries, query, (entry) => entry.name);
}
