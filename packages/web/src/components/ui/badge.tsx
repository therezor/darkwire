/**
 * The badge recipe.
 *
 * The variant is `tone`, not `role`, for one blunt reason: `role` is an HTML
 * attribute, and a prop of that name on a `<span>` would either shadow it or
 * silently pass `"danger"` through to the accessibility tree as an ARIA role.
 *
 * One pill, parameterised on semantic role — replacing what would otherwise
 * become a hand-written variant per usage: a risk badge on a tool card, a
 * `degraded` notice, a provider's "key present" indicator, a session's origin.
 * They are the same object with a different role, and writing them separately
 * is how twenty-five slightly different pills end up in one codebase.
 *
 * `soft` is the default because a badge is an annotation: a page of solid fills
 * competes with the content it annotates. `solid` exists for the one or two
 * places where a badge *is* the message.
 */

import { cva, type VariantProps } from 'class-variance-authority';
import type { HTMLAttributes, JSX } from 'react';

import { cn } from '@/lib/cn.js';

/**
 * Tone and variant are independent here, where the previous version needed
 * eighteen compound entries to pair them. The stylesheet is what changed: a
 * tone now sets custom properties and a variant reads them, so `accent` and
 * `outline` compose in CSS instead of having to be enumerated in TypeScript.
 */
export const badgeVariants = cva('badge', {
  variants: {
    tone: {
      neutral: 'badge--neutral',
      accent: 'badge--accent',
      success: 'badge--success',
      warning: 'badge--warning',
      danger: 'badge--danger',
      info: 'badge--info',
    },
    variant: {
      soft: 'badge--soft',
      solid: 'badge--solid',
      outline: 'badge--outline',
    },
  },
  defaultVariants: { tone: 'neutral', variant: 'soft' },
});

export interface BadgeProps
  extends HTMLAttributes<HTMLSpanElement>, VariantProps<typeof badgeVariants> {}

export function Badge({
  className,
  tone,
  variant,
  ...props
}: BadgeProps): JSX.Element {
  return (
    <span
      className={cn(badgeVariants({ tone, variant }), className)}
      {...props}
    />
  );
}
