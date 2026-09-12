/**
 * The control under a list that is longer than one page.
 *
 * Numbered pages rather than a "Load more" button, and the difference is what
 * the reader is doing. Loading more suits someone scrolling *through* a list;
 * these screens are ones you arrive at knowing roughly what you want, filter,
 * and then act on a row — so the useful affordances are jumping to a page and
 * being told how much there is. A "Load more" answers neither, and the count it
 * hides is the one number that says whether the filter worked.
 *
 * **The count is not decoration.** `Showing 26–50 of 287` is the only thing on
 * screen that distinguishes "no matches" from "matches, but not on this page",
 * which is the state a stale page number leaves you in. `usePagination` makes
 * that state unreachable; this makes it legible if it ever happens anyway.
 *
 * It is `aria-live="polite"` because pressing a page number replaces the rows
 * silently — the focus stays on a button whose label has not changed, and
 * without an announcement the only feedback is visual.
 *
 * The current page is a raised surface and the full-strength text tier, which is
 * how this UI says selection everywhere else — a nav row, a menu item, a session
 * in the sidebar. Not the accent: a column of gold numbers would make the accent
 * mean "a number" rather than "the one thing asking to be looked at".
 */

import { ChevronLeft, ChevronRight } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '@/components/ui/button.js';
import { cn } from '@/lib/cn.js';
import type { Pagination as PaginationState } from './use-pagination.js';

/** Pages either side of the current one that are always spelled out. */
const SPAN = 1;

/**
 * The page numbers to render, with `gap` where a run was elided.
 *
 * The first and last pages are always present — they are the two destinations
 * anyone actually aims for — plus a window around wherever you are.
 *
 * A gap that would hide exactly one page renders that page instead. `1 … 3` is
 * absurd: the ellipsis is the same width as the number it replaced, and it
 * turns a destination into a mystery to save nothing.
 */
export function pageItems(
  page: number,
  pageCount: number,
): ReadonlyArray<number | 'gap'> {
  const wanted = new Set<number>([1, pageCount]);
  for (let candidate = page - SPAN; candidate <= page + SPAN; candidate += 1) {
    if (candidate >= 1 && candidate <= pageCount) wanted.add(candidate);
  }

  const items: Array<number | 'gap'> = [];
  let previous = 0;
  for (const current of [...wanted].sort((a, b) => a - b)) {
    if (previous !== 0) {
      if (current - previous === 2) items.push(previous + 1);
      else if (current - previous > 2) items.push('gap');
    }
    items.push(current);
    previous = current;
  }
  return items;
}

export function Pagination({
  pagination,
  total,
  label,
  numbered = true,
}: {
  readonly pagination: PaginationState;
  readonly total: number;
  /** What is being paged, so the landmark announces as more than "navigation". */
  readonly label: string;
  /**
   * Whether to spell out the page numbers, or offer only Previous and Next.
   *
   * A behaviour switch rather than a style one, and it turns on whether a page
   * number is a *destination*. On a list of records it is: "the agents starting
   * with M are on page 3" is a thing an operator learns and returns to. A
   * notification feed has no such structure — it is one stream ordered by when
   * things happened, and page 4 of it means nothing except "further back". So
   * that list gets the two controls that do mean something, and the count beside
   * them says where you are.
   */
  readonly numbered?: boolean;
}): JSX.Element | null {
  const { t } = useTranslation();
  const { page, pageCount, start, end, setPage } = pagination;

  // One page is not a pager. Rendering a disabled Previous and Next under every
  // short list is chrome that can never do anything.
  if (pageCount <= 1) return null;

  return (
    <nav className="row pagination" aria-label={label}>
      <p className="pagination__count" aria-live="polite">
        {t('common.pageRange', { start, end, total })}
      </p>

      <span className="spacer" />

      <Button
        variant="ghost"
        size="sm"
        className="pagination__step"
        disabled={page === 1}
        aria-label={t('common.previousPage')}
        onClick={() => {
          setPage(page - 1);
        }}
      >
        <ChevronLeft aria-hidden="true" />
      </Button>

      {/* A list, so a screen reader is told how many destinations there are
          before it starts reading them out. */}
      {numbered && (
        <ul className="row pagination__pages">
          {pageItems(page, pageCount).map((item, index) =>
            item === 'gap' ? (
              // Not a button, and not announced: it is the *absence* of pages, and
              // reading "ellipsis" between two numbers says nothing a reader can
              // act on.
              <li
                key={`gap-${String(index)}`}
                className="pagination__gap"
                aria-hidden="true"
              >
                …
              </li>
            ) : (
              <li key={item}>
                <Button
                  variant="ghost"
                  size="sm"
                  className={cn(
                    'pagination__page',
                    item === page && 'pagination__page--current',
                  )}
                  // The visible label is a bare number, which says nothing on its
                  // own; this is what makes it a destination.
                  aria-label={t('common.goToPage', { page: item })}
                  {...(item === page
                    ? { 'aria-current': 'page' as const }
                    : {})}
                  onClick={() => {
                    setPage(item);
                  }}
                >
                  {item}
                </Button>
              </li>
            ),
          )}
        </ul>
      )}

      <Button
        variant="ghost"
        size="sm"
        className="pagination__step"
        disabled={page === pageCount}
        aria-label={t('common.nextPage')}
        onClick={() => {
          setPage(page + 1);
        }}
      >
        <ChevronRight aria-hidden="true" />
      </Button>
    </nav>
  );
}
