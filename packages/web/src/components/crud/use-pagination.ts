/**
 * Which page of a list is showing, and the two ways that goes wrong.
 *
 * Both failures are the same shape — the page outlives the reason it was valid —
 * and both are invisible until someone uses the list for more than a moment,
 * which is why they are here once rather than in each screen:
 *
 *  - **A filter must send you back to page 1.** Typing a query while on page 4
 *    of 12 leaves you on page 4 of a result set that now has one page, looking
 *    at an empty list under a control insisting there are matches. The screens
 *    would each have to remember to reset, in the `onValueChange` of a search
 *    box and again in the `onChange` of a sort control.
 *  - **A shrinking total must pull you back.** Delete the only row on the last
 *    page and that page stops existing; without a clamp the list is empty and
 *    the only way out is a control that is now pointing at nothing.
 *
 * **Both are resolved during render, not in an effect.** An effect would paint
 * the wrong page once and correct it on the next frame, which is the flicker on
 * every keystroke of a search box. Comparing the stored key with the current one
 * while rendering is React's own sanctioned pattern for state derived from
 * props, and it produces no intermediate paint at all.
 *
 * **The total arrives after the page, which is why this is split in two.** A
 * server-paged list cannot hand the total in: the request needs a page number
 * before there is a response to count. So the hook owns the page and knows
 * nothing else, and `withTotal` is a pure computation applied once the number is
 * known. An in-memory list has the total up front and calls the two together on
 * one line; a server-paged one calls the hook, makes its request, and applies
 * `withTotal` to what came back. One shape, both readers.
 */

import { useState } from 'react';

/**
 * Rows per page, everywhere.
 *
 * One constant rather than a prop with a default: pages of different lengths on
 * two screens is a difference a reader notices and cannot act on, and a default
 * is an invitation for a third screen to pass something else.
 */
export const PAGE_SIZE = 25;

/** A page, once the size of the list it is a page of is known. */
export interface Pagination {
  /** 1-based, clamped into range. */
  readonly page: number;
  readonly pageCount: number;
  /** 1-based index of the first row on this page; 0 when there are none. */
  readonly start: number;
  /** 1-based index of the last row on this page; 0 when there are none. */
  readonly end: number;
  /** Rows to skip — what a server-paged list sends as `offset`. */
  readonly offset: number;
  readonly setPage: (next: number) => void;
}

/** A page number and the means to change it, before any total is known. */
interface PageState {
  /** What to ask the server for. Never clamped, because nothing here can clamp it yet. */
  readonly page: number;
  readonly setPage: (next: number) => void;
  /** The same page, resolved against the size of the list it turned out to be. */
  readonly withTotal: (total: number, pageSize?: number) => Pagination;
}

export function usePagination({
  resetOn,
}: {
  /**
   * Everything that changes *which rows* the list is showing, as one string —
   * typically `` `${filter}|${sort.key}|${sort.descending}` ``. When it changes,
   * the page goes back to 1.
   *
   * A string rather than a dependency array because it is compared, not
   * subscribed to: two renders with the same key are the same view of the list,
   * whatever objects the caller rebuilt on the way there.
   */
  readonly resetOn: string;
}): PageState {
  const [state, setState] = useState({ page: 1, resetOn });

  // The reset, during render. `state.page` is only trusted while the key it was
  // chosen under still holds.
  const page = state.resetOn === resetOn ? state.page : 1;
  if (state.resetOn !== resetOn) setState({ page: 1, resetOn });

  const setPage = (next: number): void => {
    setState({ page: Math.max(1, next), resetOn });
  };

  return {
    page,
    setPage,
    withTotal: (total, pageSize = PAGE_SIZE) => {
      // At least one page, so an empty list is "page 1 of 1" rather than
      // "1 of 0" — a count of zero would make every control here disabled *and*
      // out of range.
      const pageCount = Math.max(1, Math.ceil(total / pageSize));
      // The clamp is applied to what is *shown* rather than written back to
      // state, so a total that dips and recovers — a refetch mid-delete — does
      // not quietly lose the reader's place.
      const shown = Math.min(page, pageCount);
      const offset = (shown - 1) * pageSize;

      return {
        page: shown,
        pageCount,
        offset,
        start: total === 0 ? 0 : offset + 1,
        end: Math.min(offset + pageSize, total),
        setPage: (next) => {
          setPage(Math.min(next, pageCount));
        },
      };
    },
  };
}

/** The slice of an in-memory list one page shows. */
export function pageRows<T>(
  rows: readonly T[],
  { offset }: Pagination,
  pageSize = PAGE_SIZE,
): T[] {
  return rows.slice(offset, offset + pageSize);
}
