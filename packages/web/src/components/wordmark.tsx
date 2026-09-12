/**
 * The name with the mark beside it.
 *
 * A component rather than two elements written out twice, because the two
 * places the brand appears — the header and the login card — set type
 * differently: the header is semibold at the base size, and the login card's
 * eyebrow is an uppercase micro-label. So this carries *no* type of its own. It
 * is structure only — the mark, the gap, the name — and every visual property
 * comes from the class the caller passes and from the text size it inherits.
 * That is what `.wordmark` in `components/wordmark.css` is written in `em` for:
 * the mark tracks whatever type it has been dropped into, so the lockup holds
 * together at 0.6875rem uppercase and at 0.875rem semibold without a size prop.
 *
 * The mark is `aria-hidden`, so a screen reader reads "GhostAI" once. A decorated
 * name is one thing, not two.
 */

import type { JSX } from 'react';

import { Skull } from 'lucide-react';

import { cn } from '@/lib/cn.js';

export function Wordmark({
  className,
}: {
  readonly className?: string;
}): JSX.Element {
  return (
    <span className={cn('wordmark', className)}>
      <Skull className="wordmark__mark" aria-hidden="true" />
      {/* Its own element so the name stays one text node — which is what keeps
          `getByText('GhostAI')` matching an exact string rather than having to
          fall back to a substring match around the mark. */}
      <span>GhostAI</span>
    </span>
  );
}
