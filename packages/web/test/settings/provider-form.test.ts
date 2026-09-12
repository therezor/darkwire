/**
 * One provider instance's connection settings.
 *
 * Three properties. An empty `apiBase` has to be *sent*, because that is how it
 * is cleared back to the registry's default; `null` has to reach the wire
 * intact, because it is the only syntax the merge has for a deletion; and no
 * field here may ever be a credential, because the vault is write-only and this
 * patch goes to a route that answers with the settings tree.
 */

import { ConfigPatchSchema, ProviderConfigSchema } from '@ghostwire/protocol';
import { describe, expect, it } from 'vitest';

import {
  EMPTY_PROVIDER_FORM,
  KEY_PLACEHOLDER,
  initialKeyField,
  proposeInstanceId,
  toCreateProviderPatch,
  toCredentialValue,
  toDeleteProviderPatch,
  toProviderForm,
  toProviderPatch,
  toProviderTestRequest,
  type ProviderForm,
} from '@/settings/provider-form.js';

describe('toProviderForm', () => {
  it('reads a configured instance', () => {
    const form = toProviderForm(
      ProviderConfigSchema.parse({
        type: 'ollama',
        label: 'GPU box',
        apiBase: 'http://127.0.0.1:11434/v1',
        models: ['a', 'b'],
      }),
    );

    expect(form.type).toBe('ollama');
    expect(form.label).toBe('GPU box');
    expect(form.apiBase).toBe('http://127.0.0.1:11434/v1');
    expect(form.models).toBe('a\nb');
    expect(form.enabled).toBe(true);
  });

  it('reads an instance the config has no block for', () => {
    // The panel renders from the providers response, which can list an instance
    // a moment before the settings query catches up.
    expect(toProviderForm(undefined)).toEqual(EMPTY_PROVIDER_FORM);
  });
});

const FORM: ProviderForm = {
  type: 'ollama',
  label: 'GPU box',
  apiBase: 'http://h/v1',
  models: 'a, b',
  enabled: true,
};

describe('toProviderPatch', () => {
  it('patches only the one instance it was given', () => {
    const patch = toProviderPatch('ollama-gpu', FORM);

    expect(patch).toEqual({
      providers: {
        'ollama-gpu': {
          label: 'GPU box',
          apiBase: 'http://h/v1',
          models: ['a', 'b'],
          enabled: true,
        },
      },
    });
    expect(() => ConfigPatchSchema.parse(patch)).not.toThrow();
  });

  it('never carries the type, so an endpoint cannot change protocol by being edited', () => {
    // The form holds one now — the dialog that creates and the dialog that
    // edits are the same dialog — so this is the assertion that keeps the type
    // control's `disabled` from being the only thing enforcing it.
    const patch = toProviderPatch('openai', { ...FORM, type: 'anthropic' });
    expect(patch.providers?.openai).not.toHaveProperty('type');
  });

  it('sends an empty base URL rather than omitting it, so the field can be cleared', () => {
    // Omitting it would mean "leave whatever is there", and the operator who
    // just emptied the field would find the old endpoint back on reload.
    const patch = toProviderPatch('openai', {
      ...FORM,
      apiBase: '   ',
      models: '',
    });
    expect(patch.providers?.openai).toMatchObject({ apiBase: '', models: [] });
  });

  it('never carries a credential', () => {
    expect(JSON.stringify(toProviderPatch('openai', FORM))).not.toMatch(
      /key|secret|token/i,
    );
  });
});

describe('toCreateProviderPatch', () => {
  it('names the type, which is the one patch that may', () => {
    const patch = toCreateProviderPatch('ollama-2', {
      ...FORM,
      label: '  GPU box  ',
    });

    expect(patch).toEqual({
      providers: {
        'ollama-2': {
          type: 'ollama',
          label: 'GPU box',
          apiBase: 'http://h/v1',
          models: ['a', 'b'],
          enabled: true,
        },
      },
    });
    expect(() => ConfigPatchSchema.parse(patch)).not.toThrow();
  });

  it('carries every field the edit patch does, plus the type', () => {
    // One form produces both, so a field added to the dialog cannot reach one
    // patch and miss the other — which is what a separate add form did.
    const created = toCreateProviderPatch('ollama-2', FORM).providers?.[
      'ollama-2'
    ];
    const edited = toProviderPatch('ollama-2', FORM).providers?.['ollama-2'];
    expect(created).toEqual({ type: 'ollama', ...edited });
  });
});

describe('proposeInstanceId', () => {
  it('uses the bare type when it is free', () => {
    expect(proposeInstanceId('ollama', [])).toBe('ollama');
    expect(proposeInstanceId('ollama', ['openai'])).toBe('ollama');
  });

  it('suffixes past every id already taken', () => {
    expect(proposeInstanceId('ollama', ['ollama'])).toBe('ollama-2');
    expect(proposeInstanceId('ollama', ['ollama', 'ollama-2'])).toBe(
      'ollama-3',
    );
    // A gap is reused rather than skipped past — the rule is "first free", not
    // "one more than the highest".
    expect(proposeInstanceId('ollama', ['ollama', 'ollama-3'])).toBe(
      'ollama-2',
    );
  });
});

describe('toCredentialValue', () => {
  it('writes nothing when the field still holds the placeholder', () => {
    // The case the whole sentinel exists for: opening a row to rename it and
    // pressing Save must not touch a key nobody typed at.
    expect(toCredentialValue(KEY_PLACEHOLDER, true)).toBeUndefined();
  });

  it('deletes when a field that said a key was there is cleared', () => {
    expect(toCredentialValue('', true)).toBeNull();
    expect(toCredentialValue('   ', true)).toBeNull();
  });

  it('writes nothing for an empty field on an instance with no key', () => {
    // The resting state of every keyless row. Sending `null` here would make
    // each save a pointless vault delete.
    expect(toCredentialValue('', false)).toBeUndefined();
  });

  it('stores what was typed, trimmed', () => {
    expect(toCredentialValue('  sk-abc  ', true)).toBe('sk-abc');
    expect(toCredentialValue('sk-abc', false)).toBe('sk-abc');
  });
});

describe('toProviderTestRequest', () => {
  it('checks the connection being edited, not the one on disk', () => {
    expect(
      toProviderTestRequest({
        form: { ...FORM, apiBase: '  http://typed-just-now/v1  ' },
        keyField: KEY_PLACEHOLDER,
        credentialsPresent: true,
        extraHeaders: { 'X-Title': 'GhostAI' },
        instanceId: 'ollama',
      }),
    ).toEqual({
      type: 'ollama',
      apiBase: 'http://typed-just-now/v1',
      // The bug this function exists to hold shut: sending `{}` here checked a
      // gateway without the headers a turn sends it, which is a different
      // endpoint wearing the same URL.
      extraHeaders: { 'X-Title': 'GhostAI' },
      instanceId: 'ollama',
    });
  });

  it('omits the key when the field was not touched, so the stored one is used', () => {
    const request = toProviderTestRequest({
      form: FORM,
      keyField: KEY_PLACEHOLDER,
      credentialsPresent: true,
      instanceId: 'ollama',
    });
    expect(request).not.toHaveProperty('apiKey');
  });

  it('checks with no key at all once the field has been cleared', () => {
    // The wrong-connection bug in its clearest form: falling back to the stored
    // key here reports a working endpoint using the very credential the save is
    // about to delete.
    expect(
      toProviderTestRequest({
        form: FORM,
        keyField: '',
        credentialsPresent: true,
        instanceId: 'ollama',
      }),
    ).toMatchObject({ apiKey: '' });
  });

  it('checks with the key that is about to be saved, not the one still stored', () => {
    expect(
      toProviderTestRequest({
        form: FORM,
        keyField: '  sk-new  ',
        credentialsPresent: true,
        instanceId: 'ollama',
      }),
    ).toMatchObject({ apiKey: 'sk-new' });
  });

  it('names no instance for a connection that has not been saved yet', () => {
    const request = toProviderTestRequest({
      form: FORM,
      keyField: '',
      credentialsPresent: false,
    });
    expect(request).not.toHaveProperty('instanceId');
    // Nothing stored to fall back to, and nothing typed: the server probes
    // with whatever the environment holds, which is the honest answer.
    expect(request).not.toHaveProperty('apiKey');
    expect(request.extraHeaders).toEqual({});
  });
});

describe('initialKeyField', () => {
  it('says a key is there without being one', () => {
    expect(initialKeyField(true)).toBe(KEY_PLACEHOLDER);
    expect(initialKeyField(false)).toBe('');
    // It is a constant, not a credential: the same characters for every
    // provider, carrying only what `credentialsPresent` already carries.
    expect(KEY_PLACEHOLDER).not.toMatch(/[a-z0-9]/i);
  });
});

describe('toDeleteProviderPatch', () => {
  it('sends null, which is what the merge reads as a removal', () => {
    const patch = toDeleteProviderPatch('ollama-2');

    expect(patch).toEqual({ providers: { 'ollama-2': null } });
    // It has to survive validation to reach the merge that honours it.
    expect(() => ConfigPatchSchema.parse(patch)).not.toThrow();
    expect(ConfigPatchSchema.parse(patch).providers?.['ollama-2']).toBeNull();
  });
});
