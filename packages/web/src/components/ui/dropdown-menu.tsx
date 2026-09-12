/**
 * Dropdown menu.
 *
 * The behaviour worth not writing twice: roving focus with type-ahead, arrow
 * keys, `Escape`, pointer-vs-keyboard open semantics, and collision-aware
 * placement in a portal. The portal is also why most of a z-index scale can
 * retire — layering is a DOM-order question once everything overlaying the page
 * is a sibling at the end of `<body>`.
 *
 */

import * as DropdownMenuPrimitive from '@radix-ui/react-dropdown-menu';
import { Check } from 'lucide-react';
import type { ComponentProps, JSX } from 'react';

import { cn } from '@/lib/cn.js';

export const DropdownMenu = DropdownMenuPrimitive.Root;
export const DropdownMenuTrigger = DropdownMenuPrimitive.Trigger;
export const DropdownMenuRadioGroup = DropdownMenuPrimitive.RadioGroup;

export function DropdownMenuContent({
  className,
  sideOffset = 6,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.Content>): JSX.Element {
  return (
    <DropdownMenuPrimitive.Portal>
      <DropdownMenuPrimitive.Content
        sideOffset={sideOffset}
        // `.floating` is the surface a menu, a popover and a select listbox all
        // share — see `styles/components/menu.css`.
        className={cn('floating', className)}
        {...props}
      />
    </DropdownMenuPrimitive.Portal>
  );
}

export function DropdownMenuItem({
  className,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.Item>): JSX.Element {
  return (
    <DropdownMenuPrimitive.Item
      className={cn('menu__item', className)}
      {...props}
    />
  );
}

/**
 * A row that reports whether it is the chosen one.
 *
 * The indicator is rendered *after* the children and sits at the trailing edge.
 * A tick in a left-hand gutter, in the accent colour, at icon size collides
 * with a row's own icon and makes the one green mark in the panel the least
 * important thing in it. Trailing and muted, the row reads label-first and the
 * tick is confirmation rather than decoration. `menu.css` carries the rest.
 */
export function DropdownMenuRadioItem({
  className,
  children,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.RadioItem>): JSX.Element {
  return (
    <DropdownMenuPrimitive.RadioItem
      className={cn('menu__item menu__item--checkable', className)}
      {...props}
    >
      {children}
      <span className="menu__indicator">
        <DropdownMenuPrimitive.ItemIndicator>
          <Check />
        </DropdownMenuPrimitive.ItemIndicator>
      </span>
    </DropdownMenuPrimitive.RadioItem>
  );
}

export function DropdownMenuLabel({
  className,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.Label>): JSX.Element {
  return (
    <DropdownMenuPrimitive.Label
      className={cn('menu__label', className)}
      {...props}
    />
  );
}

export function DropdownMenuSeparator({
  className,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.Separator>): JSX.Element {
  return (
    <DropdownMenuPrimitive.Separator
      className={cn('menu__separator', className)}
      {...props}
    />
  );
}
