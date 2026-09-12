/**
 * Copy, with the confirmation on the control rather than in a toast.
 *
 * A toast saying "copied" is a notification about something the user just did
 * and is already looking at. The check mark goes where their eye already is,
 * and reverts on its own — so the hook owns the timer and the caller owns the
 * appearance.
 *
 * A copy that failed does not flip `copied`. A button that claims success it
 * did not have is worse than one that appears not to have registered the click,
 * because only one of those leaves the user pasting something stale.
 */

import { useEffect, useState } from 'react';

import { copyToClipboard } from '@/lib/clipboard.js';

/** How long the confirmation holds. Long enough to read, short enough to forget. */
const COPY_CONFIRM_MS = 1500;

interface CopyState {
  readonly copied: boolean;
  readonly copy: () => void;
}

export function useCopy(text: string): CopyState {
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return undefined;
    const timer = setTimeout(() => {
      setCopied(false);
    }, COPY_CONFIRM_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [copied]);

  return {
    copied,
    copy: () => {
      void copyToClipboard(text).then((ok) => {
        if (ok) setCopied(true);
      });
    },
  };
}
