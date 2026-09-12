/**
 * Filter, sort and page an in-memory list — the whole of what a CRUD page does
 * between "the rows arrived" and "here is what to render".
 *
 * Four pages had this written out: agents, workspaces, providers and automation
 * jobs. Twelve lines each, identical apart from the haystack and the
 * comparators, and one of those lines is load-bearing in a way that does not
 * look it:
 *
 * ```ts
 * resetOn: `${filter}|${sort.key}|${String(sort.descending)}`
 * ```
 *
 * That string is what sends a reader back to page 1 when they type in the search
 * box — `usePagination` explains what happens when it is wrong, and it was
 * retyped four times, which is four chances to forget the sort half of it and
 * strand somebody on page 4 of 1.
 *
 * **`haystack`, `group` and `tiebreak` are deliberately not memo dependencies.**
 * Every call site passes them as inline arrows, so including them would rebuild
 * the list on every render and the memo would be decoration. They must be pure
 * functions of the row, which all four are. `comparators` *is* a dependency,
 * because one of the four builds it from a formatter that changes with the
 * locale — a page whose sort silently kept the old language's collation is the
 * bug that would hide here.
 */

import { useMemo, useState } from 'react';

import {
  filterRows,
  sortRows,
  type Comparators,
  type SortOrder,
  type SortRowsOptions,
} from './sort.js';
import { pageRows, usePagination, type Pagination } from './use-pagination.js';

interface ListPageOptions<T, K extends string> extends SortRowsOptions<T> {
  /** The whole list, before anything is narrowed. */
  readonly rows: readonly T[];
  /**
   * The column the page opens on.
   *
   * `NoInfer` because otherwise this is the site the key union is inferred
   * from, and one literal `'name'` here narrows `K` to that single column —
   * making every *other* column a type error at the sort control. The
   * comparators are the honest source: they list every column there is.
   */
  readonly initialSort: SortOrder<NoInfer<K>>;
  /** The text the filter box matches against, per row. */
  readonly haystack: (row: T) => string;
  /**
   * One ascending comparison per sortable column.
   *
   * Memoise it at the call site if it is built from anything — it is the one
   * input here that participates in the memo.
   */
  readonly comparators: Comparators<T, K>;
}

interface ListPage<T, K extends string> {
  readonly filter: string;
  readonly setFilter: (next: string) => void;
  readonly sort: SortOrder<K>;
  readonly setSort: (next: SortOrder<K>) => void;
  /** Everything the filter and sort produced, before paging. */
  readonly matched: readonly T[];
  readonly pagination: Pagination;
  /** The one page of `matched` that is on screen. */
  readonly rows: readonly T[];
}

export function useListPage<T, K extends string>({
  rows,
  initialSort,
  haystack,
  comparators,
  group,
  tiebreak,
}: ListPageOptions<T, K>): ListPage<T, K> {
  const [filter, setFilter] = useState('');
  const [sort, setSort] = useState<SortOrder<K>>(initialSort);

  const matched = useMemo(
    () =>
      sortRows(filterRows(rows, filter, haystack), sort, comparators, {
        // Spread rather than passed straight through: `exactOptionalPropertyTypes`
        // makes an explicit `group: undefined` a different thing from an absent
        // one, and `SortRowsOptions` asks for absent.
        ...(group === undefined ? {} : { group }),
        ...(tiebreak === undefined ? {} : { tiebreak }),
      }),
    // See the header: `haystack`, `group` and `tiebreak` are excluded on
    // purpose, and `comparators` is in on purpose.
    [rows, filter, sort, comparators],
  );

  const pagination = usePagination({
    resetOn: `${filter}|${sort.key}|${String(sort.descending)}`,
  }).withTotal(matched.length);

  return {
    filter,
    setFilter,
    sort,
    setSort,
    matched,
    pagination,
    rows: pageRows(matched, pagination),
  };
}
