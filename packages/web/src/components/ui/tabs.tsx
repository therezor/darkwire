/**
 * Tabs.
 *
 * Radix gives the roving tabindex and the `aria-controls` wiring; the reason to
 * use it rather than a row of buttons is that arrow-key navigation between tabs
 * is what a screen-reader user expects and nobody remembers to implement.
 *
 * The active tab is marked with a surface change and a text tier, never with
 * colour alone — colour alone fails for the same users the roving tabindex is
 * for.
 */

import * as TabsPrimitive from '@radix-ui/react-tabs';
import type { ComponentProps, JSX } from 'react';

import { cn } from '@/lib/cn.js';

export const Tabs = TabsPrimitive.Root;

export function TabsList({
  className,
  ...props
}: ComponentProps<typeof TabsPrimitive.List>): JSX.Element {
  return (
    <TabsPrimitive.List className={cn('tabs__list', className)} {...props} />
  );
}

export function TabsTrigger({
  className,
  ...props
}: ComponentProps<typeof TabsPrimitive.Trigger>): JSX.Element {
  return (
    <TabsPrimitive.Trigger
      className={cn('tabs__trigger', className)}
      {...props}
    />
  );
}

export function TabsContent({
  className,
  ...props
}: ComponentProps<typeof TabsPrimitive.Content>): JSX.Element {
  return (
    <TabsPrimitive.Content
      className={cn('tabs__content', className)}
      {...props}
    />
  );
}
