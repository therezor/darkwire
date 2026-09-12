/**
 * The furniture every form on this app is built out of.
 *
 * A settings screen is where a design system either holds or visibly stops
 * holding: it is dozens of label/control/hint triples, and written one at a time
 * they drift in spacing, in where the hint sits, and in whether the label is
 * attached to the control at all. These are the shapes a form needs, so a new
 * setting is a line rather than a layout decision.
 *
 * It lived in `settings/` and was imported across by the agent editor, the job
 * editor, the workspace editor and the setup overlay — a design-system layer
 * named after one of its callers, so that deleting the Settings screen would
 * have been a compile error in four features that have nothing to do with it.
 * `fields.ts` next door already carried that argument in its `PatchResult` note;
 * this is the same argument applied to the module rather than to one symbol.
 *
 * `SaveBar` carries the one behavioural rule worth stating: **a settings panel
 * saves on a press, never on a keystroke.** A field that applies as it is typed
 * would rebuild the provider on every character of an API base, and half of
 * those characters are a URL that does not resolve yet.
 */

import type { ComponentProps, JSX, ReactNode } from 'react';
import { useId } from 'react';

import { cn } from '@/lib/cn.js';
import { Button } from '@/components/ui/button.js';
import { Input, Label, Textarea } from '@/components/ui/field.js';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select.js';
import { Switch } from '@/components/ui/switch.js';

export function Section({
  title,
  description,
  children,
  className,
}: {
  readonly title: string;
  readonly description?: ReactNode;
  readonly children: ReactNode;
  readonly className?: string;
}): JSX.Element {
  return (
    <section aria-label={title} className={cn('settings-section', className)}>
      <h3 className="settings-section__title">{title}</h3>
      {description !== undefined && (
        <p className="settings-section__description">{description}</p>
      )}
      <div className="stack settings-section__body">{children}</div>
    </section>
  );
}

/** Two columns above `sm`, one below — a settings form is the last place to force a scroll. */
export function FieldGrid({
  children,
}: {
  readonly children: ReactNode;
}): JSX.Element {
  return <div className="settings-grid">{children}</div>;
}

interface TextareaFieldProps extends Omit<
  ComponentProps<'textarea'>,
  'id' | 'onChange'
> {
  readonly label: ReactNode;
  readonly hint?: ReactNode;
  readonly error?: string | undefined;
  readonly onValueChange: (value: string) => void;
}

/**
 * `TextField`, for a setting that is a sentence rather than a value.
 *
 * The box grows with its content — `field-sizing: content` on `.textarea` — so
 * it is one line for a short answer and several for a long one, and it is set in
 * the sans face rather than the mono the list textareas use: this holds prose,
 * and mono is for the things read character by character.
 *
 * It exists because a one-line input hides a long *placeholder*. That is not a
 * cosmetic complaint — a placeholder holding the default an operator gets if
 * they type nothing is unreadable at forty characters, which leaves the default
 * exactly as invisible as having no placeholder at all.
 */
export function TextareaField({
  label,
  hint,
  error,
  onValueChange,
  className,
  ...props
}: TextareaFieldProps): JSX.Element {
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
      <Textarea
        id={id}
        aria-invalid={error !== undefined}
        {...(describedBy === '' ? {} : { 'aria-describedby': describedBy })}
        className={cn('textarea--prose', className)}
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

export interface SelectFieldOption {
  readonly value: string;
  readonly label: string;
}

/**
 * `placeholder` is not decoration here.
 *
 * An empty `value` means *no* value to a Radix select, so a control whose state
 * is "nothing chosen yet" — a required model on an install that has not picked
 * one — renders a blank trigger unless something is given to show instead. A
 * blank control reads as broken, which is worse than the setting it describes.
 *
 * `error` mirrors `TextField`'s rather than being passed as a `hint`: a hint is
 * not announced, does not mark the control invalid, and reads as advice at
 * exactly the moment it is a refusal.
 */
export function SelectField({
  label,
  hint,
  error,
  value,
  options,
  onValueChange,
  disabled,
  placeholder,
}: {
  readonly label: ReactNode;
  readonly hint?: ReactNode;
  readonly error?: string | undefined;
  readonly value: string;
  readonly options: readonly SelectFieldOption[];
  readonly onValueChange: (value: string) => void;
  readonly disabled?: boolean;
  readonly placeholder?: string;
}): JSX.Element {
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
      <Select
        value={value}
        onValueChange={onValueChange}
        {...(disabled === true ? { disabled } : {})}
      >
        <SelectTrigger
          id={id}
          aria-invalid={error !== undefined}
          {...(describedBy === '' ? {} : { 'aria-describedby': describedBy })}
        >
          <SelectValue
            {...(placeholder === undefined ? {} : { placeholder })}
          />
        </SelectTrigger>
        <SelectContent>
          {options.map((option) => (
            <SelectItem key={option.value} value={option.value}>
              {option.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
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

/**
 * A switch with its label to the left, which is the one arrangement that reads
 * as a toggle rather than as a field. The whole row is the label element, so the
 * hit target is the sentence and not the 2.25rem control at the end of it.
 */
export function SwitchRow({
  label,
  hint,
  checked,
  onCheckedChange,
}: {
  readonly label: ReactNode;
  readonly hint?: ReactNode;
  readonly checked: boolean;
  readonly onCheckedChange: (checked: boolean) => void;
}): JSX.Element {
  const id = useId();

  return (
    <div className="settings-switch-row">
      <div className="stack settings-switch-row__text">
        <Label htmlFor={id}>{label}</Label>
        {hint !== undefined && <p className="settings-field__hint">{hint}</p>}
      </div>
      <Switch id={id} checked={checked} onCheckedChange={onCheckedChange} />
    </div>
  );
}

/**
 * The save row.
 *
 * `Revert` appears only while there is something to revert, and both controls
 * disable when the form is clean — a Save button that is always pressable
 * invites a save of nothing, which on this screen means rebuilding the provider
 * and the jail to change nothing at all.
 */
export function SaveBar({
  dirty,
  saving,
  onSave,
  onRevert,
  children,
}: {
  readonly dirty: boolean;
  readonly saving: boolean;
  readonly onSave: () => void;
  readonly onRevert: () => void;
  readonly children?: ReactNode;
}): JSX.Element {
  return (
    <div className="cluster settings-save-bar">
      <Button variant="primary" disabled={!dirty || saving} onClick={onSave}>
        {saving ? 'Saving…' : 'Save changes'}
      </Button>
      {dirty && (
        <Button variant="ghost" disabled={saving} onClick={onRevert}>
          Revert
        </Button>
      )}
      {children}
      {/* Announced rather than only coloured: "unsaved changes" is the state a
          user is most likely to leave the page in the middle of. */}
      <span
        role="status"
        aria-live="polite"
        className="settings-save-bar__state"
      >
        {dirty ? 'Unsaved changes' : ''}
      </span>
    </div>
  );
}
