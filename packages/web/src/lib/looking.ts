/**
 * Whether the person is looking at this page: its tab shown and its window
 * focused. A background tab, or a window behind another app, is not.
 */

import { useEffect, useState } from 'react';

export function lookingAtPage(): boolean {
  return document.visibilityState === 'visible' && document.hasFocus();
}

/** {@link lookingAtPage}, kept current. */
export function useLookingAtPage(): boolean {
  const [looking, setLooking] = useState(lookingAtPage);
  useEffect(() => {
    const update = (): void => {
      setLooking(lookingAtPage());
    };
    // Not `update`: some browsers still answer `hasFocus()` with true while
    // the window's own blur is being handled.
    const left = (): void => {
      setLooking(false);
    };
    document.addEventListener('visibilitychange', update);
    window.addEventListener('focus', update);
    window.addEventListener('blur', left);
    return () => {
      document.removeEventListener('visibilitychange', update);
      window.removeEventListener('focus', update);
      window.removeEventListener('blur', left);
    };
  }, []);
  return looking;
}
