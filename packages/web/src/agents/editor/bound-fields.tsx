/**
 * The agent editor's fields that are bound to one key of the form.
 */

import type { JSX } from 'react';

import { SelectField, TextField } from '@/components/form/controls.js';
import { UNSET_VALUE } from '@/agents/agents-form.js';

/** One box on the form. */
export interface Bound {
  readonly value: string;
  readonly set: (value: string) => void;
}

/** The same, for a field that is on or off. */
export interface Toggle {
  readonly checked: boolean;
  readonly set: (checked: boolean) => void;
}

/**
 * A select whose blank is a real answer.
 *
 * `reasoningEffort` unset means the request carries no such parameter, not "no
 * value chosen", so it may not render as a blank trigger. `unsetLabel` is the
 * sentence that says what the blank does.
 */
export function OptionalSelect({
  label,
  bound,
  options,
  unsetLabel,
}: {
  readonly label: string;
  readonly bound: Bound;
  readonly options: readonly string[];
  readonly unsetLabel: string;
}): JSX.Element {
  return (
    <SelectField
      label={label}
      value={bound.value === '' ? UNSET_VALUE : bound.value}
      options={[
        { value: UNSET_VALUE, label: unsetLabel },
        ...options.map((option) => ({ value: option, label: option })),
      ]}
      onValueChange={(next) => {
        bound.set(next === UNSET_VALUE ? '' : next);
      }}
    />
  );
}

/** A text field over whichever subtree this agent keeps the setting in. */
export function BoundField({
  label,
  bound,
  error,
  hint,
  inputMode,
  placeholder,
}: {
  readonly label: string;
  readonly bound: Bound;
  readonly error?: string | undefined;
  readonly hint?: string;
  readonly inputMode?: 'numeric' | 'decimal';
  readonly placeholder?: string;
}): JSX.Element {
  return (
    <TextField
      label={label}
      value={bound.value}
      {...(error === undefined ? {} : { error })}
      {...(hint === undefined ? {} : { hint })}
      {...(inputMode === undefined ? {} : { inputMode })}
      {...(placeholder === undefined ? {} : { placeholder })}
      onValueChange={bound.set}
    />
  );
}
