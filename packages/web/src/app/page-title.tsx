/**
 * The tab title.
 *
 * A page states its own title from beside its heading, so the two cannot say
 * different things, and the shell writes it to `document.title` with the app
 * name after it. A page that has not claimed one yet (a loading state, a route
 * the shell does not know) falls back to the name of its section, which the
 * shell derives from the path.
 *
 * A store rather than an effect on the page alone, because React runs a
 * child's effects before its parent's: a page setting `document.title` directly
 * would be overwritten by the shell's fallback a moment later.
 */

import { useEffect } from 'react';
import { create } from 'zustand';

interface PageTitleState {
  readonly title: string | undefined;
  readonly claim: (title: string | undefined) => void;
}

export const usePageTitleStore = create<PageTitleState>((set) => ({
  title: undefined,
  claim: (title) => {
    set({ title });
  },
}));

/** Claims the tab title for as long as the caller is mounted. */
export function usePageTitle(title: string | undefined): void {
  const claim = usePageTitleStore((state) => state.claim);
  useEffect(() => {
    claim(title);
    return () => {
      claim(undefined);
    };
  }, [claim, title]);
}

/**
 * The same claim as an element, for a page whose early returns (loading, not
 * found) make a hook awkward. Place it beside the heading it repeats.
 */
export function PageTitle({ title }: { readonly title: string }): null {
  usePageTitle(title);
  return null;
}
