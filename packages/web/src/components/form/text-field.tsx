/**
 * One labelled text input, with its hint and error. Part of `controls.tsx`,
 * which re-exports it; it lives here so importing it costs nothing else.
 */

import type { ComponentProps, JSX, ReactNode } from 'react';
import { useId } from 'react';

import { Input, Label } from '@/components/ui/field.js';

interface TextFieldProps extends Omit<
  ComponentProps<'input'>,
  'id' | 'onChange'
> {
  readonly label: ReactNode;
  readonly hint?: ReactNode;
  readonly error?: string | undefined;
  readonly onValueChange: (value: string) => void;
}

/**
 * `Field` from the primitives, minus its `onChange` event and plus a hint that
 * stays visible next to an error rather than being replaced by it.
 *
 * The replacement matters: the hint on a duration field says what `0` means, and
 * losing it exactly when the value is invalid removes the sentence that explains
 * the valid values.
 */
export function TextField({
  label,
  hint,
  error,
  onValueChange,
  className,
  ...props
}: TextFieldProps): JSX.Element {
  const id = useId();
  const hintId = `${id}-hint`;
  const errorId = `${id}-error`;
  const describedBy = [
    hint === undefined ? undefined : hintId,
    error === undefined ? undefined : errorId,
  ]
    .filter((value) => value !== undefined)
    .join(' ');

  return (
    <div className="stack settings-field">
      <Label htmlFor={id}>{label}</Label>
      <Input
        id={id}
        aria-invalid={error !== undefined}
        {...(describedBy === '' ? {} : { 'aria-describedby': describedBy })}
        className={className}
        onChange={(event) => {
          onValueChange(event.target.value);
        }}
        {...props}
      />
      {hint !== undefined && (
        <p id={hintId} className="settings-field__hint">
          {hint}
        </p>
      )}
      {error !== undefined && (
        <p id={errorId} role="alert" className="settings-field__error">
          {error}
        </p>
      )}
    </div>
  );
}
