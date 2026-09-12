/**
 * The button recipe.
 *
 * One `cva` call replaces what would otherwise be a dozen near-identical
 * components. It names classes rather than describing appearance: what a
 * primary button *looks* like is `styles/components/button.css`, and this file
 * only decides which of those names apply. The split matters because a variant
 * is a design decision — an accent fill with `--on-fill` text, asserted at AA
 * in both themes by the contrast suite — and a decision expressed as a string
 * of utilities at the call site is a decision nothing can review.
 *
 * Nothing here suppresses the focus outline, and nothing anywhere else does
 * either. The base layer's `:focus-visible` ring applies to every focusable
 * element, so a component that removed it would be opting out of the one thing
 * that makes the app keyboard usable — which is why `a11y.test.tsx` sweeps the
 * source for the declarations that would.
 */

import { Slot } from '@radix-ui/react-slot';
import { cva, type VariantProps } from 'class-variance-authority';
import type { ButtonHTMLAttributes, JSX, Ref } from 'react';

import { cn } from '@/lib/cn.js';

export const buttonVariants = cva('btn', {
  variants: {
    variant: {
      primary: 'btn--primary',
      secondary: 'btn--secondary',
      ghost: 'btn--ghost',
      danger: 'btn--danger',
      link: 'btn--link',
    },
    size: {
      sm: 'btn--sm',
      md: 'btn--md',
      icon: 'btn--icon',
    },
  },
  defaultVariants: { variant: 'secondary', size: 'md' },
});

interface ButtonProps
  extends
    ButtonHTMLAttributes<HTMLButtonElement>,
    VariantProps<typeof buttonVariants> {
  /**
   * Render the child element instead of a `<button>`, keeping the classes.
   * The escape hatch for a link that looks like a button — which must stay an
   * `<a>`, because a `<button>` that navigates is not reachable the way a link
   * is and does not open in a new tab.
   */
  readonly asChild?: boolean;
  readonly ref?: Ref<HTMLButtonElement>;
}

export function Button({
  className,
  variant,
  size,
  asChild = false,
  type,
  ...props
}: ButtonProps): JSX.Element {
  const Component = asChild ? Slot : 'button';

  return (
    <Component
      // A `<button>` inside a form defaults to `submit`. That default has
      // submitted more forms by accident than it has on purpose.
      {...(asChild ? {} : { type: type ?? 'button' })}
      className={cn(buttonVariants({ variant, size }), className)}
      {...props}
    />
  );
}
