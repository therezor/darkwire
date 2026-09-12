/**
 * Sorting and filtering a list, for every screen that shows one.
 *
 * Files had both first, typed to `FileEntry`, which is why Agents shipped with
 * a hand-rolled filter and no sorting at all: the functions that already did
 * the job could not be called on anything else. These are the same two
 * functions with the row type lifted out, and `files/paths.ts` now re-exports
 * its own specialisations of them.
 *
 * Two properties are worth stating, because both are the kind that is invisible
 * until a list flickers:
 *
 *  - **A group survives the direction.** Files keeps directories at the top in
 *    *every* order — they are not small files or old files, they are the places
 *    to go next, and a "largest first" that scattered them through the list
 *    turns navigating into searching. So `group` is compared before the column
 *    and is never multiplied by the direction.
 *  - **A tie is broken explicitly, not left to the sort's stability.** Eight
 *    zero-byte files created by one turn compare equal on size, and a run that
 *    ordered them by whatever the previous array happened to hold would
 *    reshuffle them on every refetch.
 */

/** Which column a list is ordered by, and which way. */
export interface SortOrder<K extends string> {
  readonly key: K;
  readonly descending: boolean;
}

/** One ascending comparison per sortable column. */
export type Comparators<T, K extends string> = Readonly<
  Record<K, (a: T, b: T) => number>
>;

export interface SortRowsOptions<T> {
  /**
   * A rank applied before the column and in both directions. Rows sort into
   * ascending rank groups, and the chosen column orders within each.
   */
  readonly group?: (row: T) => number;
  /** Breaks a tie the column could not, so equal rows hold their order. */
  readonly tiebreak?: (a: T, b: T) => number;
}

export function sortRows<T, K extends string>(
  rows: readonly T[],
  order: SortOrder<K>,
  compare: Comparators<T, K>,
  options: SortRowsOptions<T> = {},
): readonly T[] {
  const direction = order.descending ? -1 : 1;
  const { group, tiebreak } = options;

  return [...rows].sort((a, b) => {
    if (group !== undefined) {
      const ranked = group(a) - group(b);
      if (ranked !== 0) return ranked;
    }

    const ordered = compare[order.key](a, b) * direction;
    if (ordered !== 0) return ordered;

    return tiebreak?.(a, b) ?? 0;
  });
}

/**
 * The rows whose text contains `query`, case-insensitively.
 *
 * A filter over what is already loaded rather than a request. Every list this
 * is used on is one page of results that is already in memory; a filter that
 * went to the server would be a different feature, and a slower one.
 */
export function filterRows<T>(
  rows: readonly T[],
  query: string,
  haystack: (row: T) => string,
): readonly T[] {
  const needle = query.trim().toLowerCase();
  if (needle === '') return rows;
  return rows.filter((row) => haystack(row).toLowerCase().includes(needle));
}

/**
 * The order a column opens in when it is chosen.
 *
 * Names go from A, but sizes, counts and times go largest and newest first,
 * because "what is big" and "what just changed" are the questions being asked
 * of those columns. `ascendingFirst` names the text ones. Written as a list
 * rather than a per-column flag so the four lists cannot disagree about it,
 * which is the whole reason this is a function and not four lines inlined into
 * each screen.
 *
 * It does not take the current order and flip when handed the column already in
 * force. That is what a column *heading* does, being both label and toggle;
 * `ListSort` asks the two questions separately, so choosing a column is only
 * ever choosing a column.
 */
export function sortBy<K extends string>(
  key: K,
  ascendingFirst: readonly K[],
): SortOrder<K> {
  return { key, descending: !ascendingFirst.includes(key) };
}
