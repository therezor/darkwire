/**
 * The tab's icon, with a dot on it while a tool call waits for approval.
 *
 * Two static files rather than one drawn at runtime: a favicon is the one
 * place this package writes a raw colour, and `public/` is where that is
 * allowed. A pinned tab shows nothing but this, so it is the signal that
 * survives when the title cannot.
 */

import { useEffect } from 'react';

const PLAIN = '/favicon.svg';
const WAITING = '/favicon-alert.svg';

export function useFavicon(waiting: boolean): void {
  useEffect(() => {
    const link = document.querySelector('link[rel="icon"]');
    // `index.html` carries one. A page mounted without it, as a test is, has
    // nothing to swap.
    if (link === null) return;
    link.setAttribute('href', waiting ? WAITING : PLAIN);
  }, [waiting]);
}
