/**
 * One provider instance's editable settings.
 *
 * An *instance*, not a provider: the panel now lists endpoints an operator
 * created, so this module gained the two fields that make one endpoint distinct
 * from another of the same type — its label and whether it is on — and a `type`
 * that is written when the instance is created and never edited afterwards.
 * Changing an endpoint's protocol is not an edit, it is a different endpoint.
 *
 * Nothing here can fail validation, so these return patches rather than results
 * — the panel's only failure mode is the server refusing one.
 *
 * The API key is not in `ProviderForm` and never reaches `toProviderPatch`, and
 * that is the design. A key is not part of the settings tree: it goes to
 * `PUT /api/settings/credentials`, into the vault, and never comes back out.
 * Keeping it out of the form type is what makes "no credential is ever in a
 * settings patch" a property of the shape of the code rather than of
 * remembering. What *is* here is the rule for reading the key *field* — see
 * `toCredentialValue` — because that rule is pure, and the alternative was two
 * components each deciding for themselves what an empty box meant.
 *
 * `extraHeaders` is not here either, for a different reason: it is one of the
 * records `mergeConfigPatch` replaces wholesale, and a patch that omits it
 * leaves it untouched. Editing it needs a control that can express removal,
 * which is a panel of its own rather than a text field bolted to this one.
 */

import type {
  ConfigPatch,
  ProviderConfig,
  ProviderTestRequest,
} from '@darkwire/protocol';

import { formatList, parseList } from '@/components/form/fields.js';

export interface ProviderForm {
  /**
   * The registry id.
   *
   * In the form because one dialog serves both create and edit, and create has
   * to ask. It reaches the wire on a create and never on an edit — the control
   * that sets it is disabled once the instance exists, because changing an
   * endpoint's protocol is not an edit, it is a different endpoint.
   */
  readonly type: string;
  /** Blank falls back to the provider type's own display name. */
  readonly label: string;
  /** Empty means the provider's own default endpoint — see `resolveConnection`. */
  readonly apiBase: string;
  /** One model id per line, as typed. */
  readonly models: string;
  readonly enabled: boolean;
}

/** What the dialog opens on for a provider that does not exist yet. */
export const EMPTY_PROVIDER_FORM: ProviderForm = {
  type: '',
  label: '',
  apiBase: '',
  models: '',
  enabled: true,
};

export function toProviderForm(
  config: ProviderConfig | undefined,
): ProviderForm {
  return {
    type: config?.type ?? '',
    label: config?.label ?? '',
    apiBase: config?.apiBase ?? '',
    models: formatList(config?.models ?? []),
    enabled: config?.enabled ?? true,
  };
}

/**
 * The fields an edit may move — every one of them except the type.
 *
 * Shared with the create patch rather than restated there, which is what keeps
 * the two shapes from drifting now that one form produces both. A field added
 * to the dialog and only to one of the builders is the bug this shape removes.
 */
function editableFields(form: ProviderForm): {
  label: string;
  apiBase: string;
  models: string[];
  enabled: boolean;
} {
  return {
    label: form.label.trim(),
    // Sent even when empty, and that is what makes the field clearable: an
    // empty `apiBase` is read as "unset" and falls back to the registry's
    // default, where an omitted one would mean "leave whatever is there".
    apiBase: form.apiBase.trim(),
    models: parseList(form.models),
    enabled: form.enabled,
  };
}

export function toProviderPatch(
  instanceId: string,
  form: ProviderForm,
): ConfigPatch {
  return { providers: { [instanceId]: editableFields(form) } };
}

/**
 * A patch that creates an instance.
 *
 * `type` appears here and in no other patch: it is what the merged tree is
 * validated against, and an instance that could change its type would be a
 * different endpoint wearing the same credential.
 */
export function toCreateProviderPatch(
  instanceId: string,
  form: ProviderForm,
): ConfigPatch {
  return {
    providers: { [instanceId]: { type: form.type, ...editableFields(form) } },
  };
}

/**
 * A `-2`, `-3`, … suffix, added only when the bare type is taken.
 *
 * The same rule as `next_instance_id` in `darkwire-providers`, restated here
 * because the client needs an id *before* the round trip that would tell it one
 * — the server accepts whatever the patch names, so proposing a free one is the
 * client's job. A collision is not silent if it happens: the patch would merge
 * into the existing instance, which is why the taken set is the live config.
 */
export function proposeInstanceId(
  type: string,
  taken: readonly string[],
): string {
  const used = new Set(taken);
  if (!used.has(type)) return type;
  for (let n = 2; ; n += 1) {
    const candidate = `${type}-${String(n)}`;
    if (!used.has(candidate)) return candidate;
  }
}

/**
 * What the key field holds when a key is already stored.
 *
 * A fixed literal — the same characters for every provider, not derived from
 * any credential, and carrying exactly the information `credentialsPresent`
 * already carries over the wire. The client still never receives a key.
 *
 * It exists because the key and the connection now save together, and "clear
 * the field and save to remove the key" is only a safe rule if the field can
 * *say* that a key is there. Left blank, opening a row to rename it and
 * pressing Save would delete the credential — which is precisely the accident
 * the old separate "Remove key" button was there to prevent.
 */
export const KEY_PLACEHOLDER = '••••••••••••';

export function initialKeyField(credentialsPresent: boolean): string {
  return credentialsPresent ? KEY_PLACEHOLDER : '';
}

/**
 * What a save should do to the vault, read off the key field.
 *
 * `undefined` — leave it alone. The field was never touched, so there is
 * nothing to write; an unchanged key is never rewritten.
 * `null` — delete it. The operator cleared a field that said a key was there.
 * A string — store it.
 */
export function toCredentialValue(
  field: string,
  credentialsPresent: boolean,
): string | null | undefined {
  if (field === KEY_PLACEHOLDER) return undefined;
  const trimmed = field.trim();
  if (trimmed !== '') return trimmed;
  // An empty field on an instance with no key is the ordinary resting state of
  // a row nobody has typed a key into. Sending `null` for it would make every
  // save of every keyless provider a pointless vault delete.
  return credentialsPresent ? null : undefined;
}

/**
 * Which key a *check* should use, from the same field.
 *
 * Three states, and collapsing any two of them tests a connection the operator
 * is not about to have:
 *
 *  - `undefined` — the field is untouched, so probe with whatever is stored.
 *    The client has never held that key and cannot send it.
 *  - `''` — the field was cleared, so probe with **no key**. Falling back to the
 *    stored one here is the wrong-connection bug in its clearest form: it would
 *    report a working endpoint using the very credential the save is about to
 *    delete.
 *  - the typed value — probe with what is about to be saved, not with what is
 *    still in the vault.
 */
function toProbeKey(
  field: string,
  credentialsPresent: boolean,
): string | undefined {
  const value = toCredentialValue(field, credentialsPresent);
  if (value === undefined) return undefined;
  return value ?? '';
}

/**
 * The connection to check — the one being edited, not the one on disk.
 *
 * Built here rather than at each press so there is one answer to "which
 * connection". The first version of this had two, and both were wrong in the
 * same way: they sent `extraHeaders: {}`, so a check on a gateway configured
 * with headers tested an endpoint without them and reported on a connection no
 * turn would ever make.
 *
 * `extraHeaders` comes from the stored config rather than the form because it
 * is not a field the form has — see the note at the top of this file — so the
 * one honest value for it is whatever the instance is already carrying.
 */
export function toProviderTestRequest({
  form,
  keyField,
  credentialsPresent,
  extraHeaders = {},
  instanceId,
}: {
  readonly form: ProviderForm;
  readonly keyField: string;
  readonly credentialsPresent: boolean;
  readonly extraHeaders?: Readonly<Record<string, string>> | undefined;
  /** Absent for a connection that has not been saved yet. */
  readonly instanceId?: string | undefined;
}): ProviderTestRequest {
  const apiKey = toProbeKey(keyField, credentialsPresent);
  return {
    type: form.type,
    apiBase: form.apiBase.trim(),
    extraHeaders,
    ...(apiKey === undefined ? {} : { apiKey }),
    ...(instanceId === undefined ? {} : { instanceId }),
  };
}

/**
 * Switching one off, which is the reversible half of deleting it.
 *
 * Its base URL, its models and its key all stay; it simply stops being offered
 * to an agent and stops being asked for a catalogue. A patch of its own rather
 * than a round trip through the whole form, so the list can do it without
 * opening the editor — the same shape `toAgentEnabledPatch` has.
 */
export function toProviderEnabledPatch(
  instanceId: string,
  enabled: boolean,
): ConfigPatch {
  return { providers: { [instanceId]: { enabled } } };
}

/**
 * A patch that removes one.
 *
 * `null` is the only token available: the merge treats an absent key as "not
 * mentioned", so without this there would be no way to take back a provider an
 * operator added. The server also drops the instance's vault entry, because
 * the config is only half of an endpoint.
 */
export function toDeleteProviderPatch(instanceId: string): ConfigPatch {
  return { providers: { [instanceId]: null } };
}
