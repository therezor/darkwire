/**
 * Select.
 *
 * Radix rather than a native `<select>` because the provider and model
 * pickers need grouping, descriptions and a searchable-length list, none of
 * which a native control can style — and because a native `<select>` on a dark
 * theme renders its popup in the OS's colours, which is the one place a
 * carefully built theme visibly ends.
 *
 * What that costs is keyboard behaviour, so it has to be borrowed rather than
 * written: type-ahead, `Home`/`End`, and the popup positioning that keeps the
 * selected item under the cursor.
 */

import * as SelectPrimitive from '@radix-ui/react-select';
import { Check, ChevronDown } from 'lucide-react';
import type { ComponentProps, JSX } from 'react';

import { cn } from '@/lib/cn.js';

export const Select = SelectPrimitive.Root;
export const SelectValue = SelectPrimitive.Value;

export function SelectTrigger({
  className,
  children,
  ...props
}: ComponentProps<typeof SelectPrimitive.Trigger>): JSX.Element {
  return (
    <SelectPrimitive.Trigger
      className={cn('select-trigger', className)}
      {...props}
    >
      {children}
      <SelectPrimitive.Icon asChild>
        <ChevronDown className="select-trigger__icon" />
      </SelectPrimitive.Icon>
    </SelectPrimitive.Trigger>
  );
}

export function SelectContent({
  className,
  children,
  position = 'popper',
  ...props
}: ComponentProps<typeof SelectPrimitive.Content>): JSX.Element {
  return (
    <SelectPrimitive.Portal>
      <SelectPrimitive.Content
        position={position}
        className={cn(
          'floating select-content',
          position === 'popper' && 'select-content--popper',
          className,
        )}
        {...props}
      >
        <SelectPrimitive.Viewport className="select-content__viewport">
          {children}
        </SelectPrimitive.Viewport>
      </SelectPrimitive.Content>
    </SelectPrimitive.Portal>
  );
}

export function SelectItem({
  className,
  children,
  ...props
}: ComponentProps<typeof SelectPrimitive.Item>): JSX.Element {
  return (
    <SelectPrimitive.Item
      className={cn('menu__item menu__item--checkable', className)}
      {...props}
    >
      {/* Label first, tick last — the same order as the dropdown's radio row,
          so a listbox and a menu do not mark their selection in two different
          places. See `menu.css`. */}
      <SelectPrimitive.ItemText>{children}</SelectPrimitive.ItemText>
      <span className="menu__indicator">
        <SelectPrimitive.ItemIndicator>
          <Check />
        </SelectPrimitive.ItemIndicator>
      </span>
    </SelectPrimitive.Item>
  );
}
