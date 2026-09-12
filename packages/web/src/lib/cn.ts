/**
 * Class composition.
 *
 * `clsx` resolves the conditionals and nothing resolves the conflicts, because
 * with hand-written CSS there are none to resolve. That is worth stating,
 * because the previous version of this file did the opposite: under a utility
 * framework, a caller passing `className="px-6"` to a button whose recipe said
 * `px-3` got both, and which won depended on the order the framework happened
 * to emit them in — so a merge step had to re-implement the framework's own
 * conflict groups to make `className` a usable escape hatch.
 *
 * Here a component's rules and a caller's rules are different selectors in
 * different cascade layers, and `@layer` in `app.css` already says which wins.
 * Joining strings is the whole job.
 */

import { clsx, type ClassValue } from 'clsx';

export function cn(...inputs: ClassValue[]): string {
  return clsx(inputs);
}
