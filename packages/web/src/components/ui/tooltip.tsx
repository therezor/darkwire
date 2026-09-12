/**
 * Tooltip.
 *
 * A tooltip is the one floating surface that must never receive focus and must
 * never carry information available nowhere else — a touch user has no hover,
 * and a screen reader announces it only because Radix wires `aria-describedby`.
 * So it labels; it does not explain.
 *
 * `TooltipProvider` is mounted once in `providers.tsx` rather than per tooltip:
 * the shared delay timer is what makes a row of icon buttons feel like one
 * control strip instead of eight independent 700ms waits.
 */

import * as TooltipPrimitive from '@radix-ui/react-tooltip';
import type { ComponentProps, JSX, ReactNode } from 'react';

import { cn } from '@/lib/cn.js';

export const TooltipProvider = TooltipPrimitive.Provider;
const TooltipTrigger = TooltipPrimitive.Trigger;

function TooltipContent({
  className,
  sideOffset = 6,
  ...props
}: ComponentProps<typeof TooltipPrimitive.Content>): JSX.Element {
  return (
    <TooltipPrimitive.Portal>
      <TooltipPrimitive.Content
        sideOffset={sideOffset}
        className={cn('tooltip', className)}
        {...props}
      />
    </TooltipPrimitive.Portal>
  );
}

/** The whole thing in one element, since a tooltip is always the same four nodes. */
export function Tooltip({
  label,
  children,
  side = 'top',
  ...props
}: {
  readonly label: ReactNode;
  readonly children: ReactNode;
  readonly side?: 'top' | 'right' | 'bottom' | 'left';
} & ComponentProps<typeof TooltipPrimitive.Root>): JSX.Element {
  return (
    <TooltipPrimitive.Root {...props}>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent side={side}>{label}</TooltipContent>
    </TooltipPrimitive.Root>
  );
}
