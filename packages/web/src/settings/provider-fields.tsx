/**
 * The parts of a provider form the dialog and the row both have.
 *
 * Not a whole shared form: the two are genuinely different — the dialog chooses
 * a type and the row cannot, the row edits a model list and the dialog has
 * nothing to edit it against. What they share is the key field's rules and how
 * a check's verdict is worded, and those are the two pieces where two copies
 * would quietly disagree.
 */

import { CircleAlert, CircleCheck } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { ProviderTestResponse } from '@ghostwire/protocol';

import { TextField } from '@/components/form/controls.js';
import { describeProbe } from './use-provider.js';

/**
 * The API key.
 *
 * `new-password` rather than `off`, because password managers ignore `off` and
 * offer to fill the field with the site's login password — a different secret
 * entirely.
 */
export function KeyField({
  value,
  isLocal,
  envKey,
  credentialsPresent,
  onValueChange,
}: {
  readonly value: string;
  readonly isLocal: boolean;
  readonly envKey: string | undefined;
  /** Changes the hint from "paste one" to "clear this to remove it". */
  readonly credentialsPresent: boolean;
  readonly onValueChange: (value: string) => void;
}): JSX.Element {
  const stored = credentialsPresent
    ? 'A key is saved. Type to replace it, or clear this field and save to remove it.'
    : isLocal
      ? 'Most local servers need none. Fill this in if yours sits behind an authenticating proxy.'
      : 'Stored in the encrypted vault, and never sent back to this page.';

  return (
    <TextField
      label={isLocal ? 'API token (optional)' : 'API key'}
      type="password"
      autoComplete="new-password"
      spellCheck={false}
      value={value}
      placeholder={credentialsPresent ? '' : 'Paste a key'}
      onValueChange={onValueChange}
      hint={
        <>
          {stored}
          {envKey !== undefined &&
            ` ${envKey} in the environment is used when none is saved.`}
        </>
      }
    />
  );
}

/**
 * What the check found.
 *
 * Ambient, per the design system: a line of text with an icon, not a filled
 * pill. A connection that cannot be reached is a normal state on this screen —
 * it is what the operator opened it to fix — and the loudest thing in the panel
 * should not be a status that will be stale in a minute.
 *
 * `role="alert"` only for a failure. A success announced the same way would
 * interrupt a screen reader on every save to say nothing happened.
 */
export function ProbeLine({
  result,
  probing,
  saved,
}: {
  readonly result: ProviderTestResponse | null;
  readonly probing: boolean;
  /** Prefixes the failure with "Saved — but", so the write is not in doubt. */
  readonly saved: boolean;
}): JSX.Element | null {
  const { t } = useTranslation();

  if (probing) {
    return (
      <p className="provider-row__probe" role="status">
        <span className="spinner" />
        {t('providers.checking')}
      </p>
    );
  }

  if (result === null) return null;

  return (
    <p
      className={`provider-row__probe provider-row__probe--${result.ok ? 'ok' : 'bad'}`}
      role={result.ok ? 'status' : 'alert'}
    >
      {result.ok ? <CircleCheck /> : <CircleAlert />}
      {describeProbe(result, saved, t)}
    </p>
  );
}
