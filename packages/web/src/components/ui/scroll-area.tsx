/**
 * Scroll area.
 *
 * The native scrollbar is already themed in `base.css`, so this is not about
 * appearance — it is for the two places where the native one is wrong: a
 * scroll container inside a portal (a long menu, the notification list), where
 * an overlay scrollbar would sit above the content, and the chat transcript,
 * which needs a scroll position it can drive programmatically.
 *
 * Everything else should keep using `overflow-y: auto` and the native bar.
 *
 * The `display: table` trap Radix sets, and why the viewport rule undoes it,
 * is explained in `styles/components/scroll-area.css`.
 */

import * as ScrollAreaPrimitive from '@radix-ui/react-scroll-area';
import type { ComponentProps, JSX } from 'react';

import { cn } from '@/lib/cn.js';

export function ScrollArea({
  className,
  children,
  viewportRef,
  ...props
}: ComponentProps<typeof ScrollAreaPrimitive.Root> & {
  readonly viewportRef?: React.Ref<HTMLDivElement>;
}): JSX.Element {
  return (
    <ScrollAreaPrimitive.Root
      className={cn('scroll-area', className)}
      {...props}
    >
      <ScrollAreaPrimitive.Viewport
        ref={viewportRef}
        className="scroll-area__viewport"
      >
        {children}
      </ScrollAreaPrimitive.Viewport>
      <ScrollBar />
      <ScrollAreaPrimitive.Corner />
    </ScrollAreaPrimitive.Root>
  );
}

function ScrollBar({
  className,
  orientation = 'vertical',
  ...props
}: ComponentProps<
  typeof ScrollAreaPrimitive.ScrollAreaScrollbar
>): JSX.Element {
  return (
    <ScrollAreaPrimitive.ScrollAreaScrollbar
      orientation={orientation}
      className={cn(
        'scroll-area__bar',
        orientation === 'vertical' && 'scroll-area__bar--vertical',
        orientation === 'horizontal' && 'scroll-area__bar--horizontal',
        className,
      )}
      {...props}
    >
      <ScrollAreaPrimitive.ScrollAreaThumb className="scroll-area__thumb" />
    </ScrollAreaPrimitive.ScrollAreaScrollbar>
  );
}
