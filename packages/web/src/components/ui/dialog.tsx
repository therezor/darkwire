/**
 * Dialog.
 *
 * Radix contributes the behaviour that is genuinely hard and invisible when it
 * works: the focus trap, focus restoration to whatever opened it, `Escape`,
 * `aria-modal`, the outside-click, and marking the rest of the page inert for a
 * screen reader. Every class is ours; nothing about the styling is inherited
 * from a component library. That is the trade this package makes everywhere —
 * behaviour from Radix, appearance from the token layer.
 *
 * `DialogContent` renders a close button by default. A modal a keyboard user
 * can open and not leave is the single most common dialog bug, and `Escape`
 * alone does not help a pointer user on a touch device.
 */

import * as DialogPrimitive from '@radix-ui/react-dialog';
import { X } from 'lucide-react';
import type { ComponentProps, JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { cn } from '@/lib/cn.js';

export const Dialog = DialogPrimitive.Root;
export const DialogTrigger = DialogPrimitive.Trigger;
export const DialogClose = DialogPrimitive.Close;

export function DialogContent({
  className,
  children,
  showClose = true,
  ...props
}: ComponentProps<typeof DialogPrimitive.Content> & {
  readonly showClose?: boolean;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <DialogPrimitive.Portal>
      <DialogPrimitive.Overlay className="dialog-overlay" />
      <DialogPrimitive.Content className={cn('dialog', className)} {...props}>
        {children}
        {showClose && (
          <DialogPrimitive.Close
            aria-label={t('common.close')}
            className="dialog__close"
          >
            <X />
          </DialogPrimitive.Close>
        )}
      </DialogPrimitive.Content>
    </DialogPrimitive.Portal>
  );
}

export function DialogHeader({
  className,
  ...props
}: ComponentProps<'div'>): JSX.Element {
  return <div className={cn('stack dialog__header', className)} {...props} />;
}

export function DialogFooter({
  className,
  ...props
}: ComponentProps<'div'>): JSX.Element {
  return <div className={cn('dialog__footer', className)} {...props} />;
}

export function DialogHeading({
  className,
  ...props
}: ComponentProps<typeof DialogPrimitive.Title>): JSX.Element {
  return (
    <DialogPrimitive.Title
      className={cn('dialog__title', className)}
      {...props}
    />
  );
}

export function DialogSubheading({
  className,
  ...props
}: ComponentProps<typeof DialogPrimitive.Description>): JSX.Element {
  return (
    <DialogPrimitive.Description
      className={cn('dialog__description', className)}
      {...props}
    />
  );
}
