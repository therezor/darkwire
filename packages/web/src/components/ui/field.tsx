/**
 * Text input, label and the error line that goes under them.
 *
 * Not Radix: an `<input>` needs no behaviour borrowed, and wrapping one would
 * add a layer whose only job is to forward props. What it does need is the
 * wiring that is forgotten by hand — `htmlFor`, `aria-invalid`,
 * `aria-describedby` pointing at the message — so `Field` generates the ids and
 * connects them, and a caller cannot render a label that labels nothing.
 */

import type { ComponentProps, JSX, ReactNode } from 'react';
import { useId } from 'react';

import { cn } from '@/lib/cn.js';

export function Input({
  className,
  ...props
}: ComponentProps<'input'>): JSX.Element {
  return <input className={cn('input', className)} {...props} />;
}

/** The same input, sized for a list — see `.textarea` for why it grows. */
export function Textarea({
  className,
  ...props
}: ComponentProps<'textarea'>): JSX.Element {
  return <textarea className={cn('textarea', className)} {...props} />;
}

export function Label({
  className,
  ...props
}: ComponentProps<'label'>): JSX.Element {
  return <label className={cn('label', className)} {...props} />;
}

interface FieldProps extends Omit<ComponentProps<'input'>, 'id'> {
  readonly label: ReactNode;
  /** Present means invalid: it sets `aria-invalid` and is announced. */
  readonly error?: string | undefined;
  readonly hint?: ReactNode;
}

export function Field({
  label,
  error,
  hint,
  className,
  ...props
}: FieldProps): JSX.Element {
  const id = useId();
  const messageId = `${id}-message`;
  const message = error ?? hint;

  return (
    <div className="stack field">
      <Label htmlFor={id}>{label}</Label>
      <Input
        id={id}
        aria-invalid={error !== undefined}
        {...(message === undefined ? {} : { 'aria-describedby': messageId })}
        className={className}
        {...props}
      />
      {message !== undefined && (
        <p
          id={messageId}
          // `role="alert"` only when it *is* one: a hint announced as an alert
          // interrupts a screen reader mid-sentence for no reason.
          {...(error === undefined ? {} : { role: 'alert' })}
          className={cn(
            'field__message',
            error !== undefined && 'field__message--error',
          )}
        >
          {message}
        </p>
      )}
    </div>
  );
}
