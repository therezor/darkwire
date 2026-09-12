/**
 * Naming one thing: a new file, a new folder, a new agent, a workspace.
 *
 * Five call sites wrote this out by hand, and the copies had already drifted:
 * some carried a subheading and some did not, and they disagreed about the type
 * scale.
 *
 * Three behaviours belong to the component because they are the ones a copy
 * forgets:
 *
 *  - **It is a real `<form>`,** so `Enter` submits. That is the only
 *    interaction anybody wants from a dialog with one box in it.
 *  - **It resets when it reopens.** Otherwise the second "New folder" of a
 *    session opens holding the first one's name, which is how a directory ends
 *    up called `drafts` twice.
 *  - **It never validates a name itself.** For a file that is the server's
 *    jail's call, and the error comes back as the toast any other refusal
 *    would. `validate` exists for the one thing the browser genuinely knows
 *    ahead of the server — that an agent id is already taken — and it returns a
 *    hint to show rather than a message to throw.
 */

import { useEffect, useId, useState, type JSX, type ReactNode } from 'react';

import { Button } from '@/components/ui/button.js';
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogHeading,
  DialogSubheading,
} from '@/components/ui/dialog.js';
import { Input, Label } from '@/components/ui/field.js';

interface NameValidation {
  readonly ok: boolean;
  /** Shown under the box in both states — what will happen, or why it will not. */
  readonly hint?: ReactNode;
}

export function NameDialog({
  open,
  onOpenChange,
  title,
  description,
  fieldLabel,
  placeholder,
  initialValue = '',
  submitLabel = 'Create',
  pending = false,
  validate,
  onSubmit,
}: {
  readonly open: boolean;
  readonly onOpenChange: (open: boolean) => void;
  readonly title: ReactNode;
  readonly description?: ReactNode;
  readonly fieldLabel: string;
  readonly placeholder?: string;
  /** Prefilled for a rename; empty for a create. */
  readonly initialValue?: string;
  readonly submitLabel?: string;
  readonly pending?: boolean;
  readonly validate?: (value: string) => NameValidation;
  readonly onSubmit: (value: string) => void;
}): JSX.Element {
  const id = useId();
  const hintId = `${id}-hint`;
  const [value, setValue] = useState(initialValue);

  // Keyed on `open` rather than reset in `onOpenChange`, so it holds whether
  // the dialog was closed by Cancel, by `Escape` or by an outside click — three
  // paths that a caller-side reset has to remember to cover and usually does
  // not.
  useEffect(() => {
    if (open) setValue(initialValue);
  }, [open, initialValue]);

  const trimmed = value.trim();
  const validation = validate?.(value);
  const submittable = trimmed !== '' && validation?.ok !== false && !pending;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form
          className="stack"
          onSubmit={(event) => {
            event.preventDefault();
            if (submittable) onSubmit(trimmed);
          }}
        >
          <DialogHeader>
            <DialogHeading>{title}</DialogHeading>
            {description !== undefined && (
              <DialogSubheading>{description}</DialogSubheading>
            )}
          </DialogHeader>

          <div className="stack settings-field">
            <Label htmlFor={id}>{fieldLabel}</Label>
            <Input
              id={id}
              autoFocus
              value={value}
              aria-invalid={validation?.ok === false}
              {...(validation?.hint === undefined
                ? {}
                : { 'aria-describedby': hintId })}
              {...(placeholder === undefined ? {} : { placeholder })}
              onChange={(event) => {
                setValue(event.target.value);
              }}
            />
            {validation?.hint !== undefined && (
              <p id={hintId} className="settings-field__hint">
                {validation.hint}
              </p>
            )}
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="ghost"
              onClick={() => {
                onOpenChange(false);
              }}
            >
              Cancel
            </Button>
            <Button type="submit" variant="primary" disabled={!submittable}>
              {submitLabel}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
