import { describe, expect, it } from 'vitest';

import {
  ExtensionManifestSchema,
  ExtensionStatusSchema,
  RunCommandResponseSchema,
} from '#src/index.js';

const MINIMAL = { schema: 'ghostai.extension/1', id: 'slack' };

describe('ExtensionManifestSchema', () => {
  it('needs only a schema tag and an id', () => {
    // The floor is deliberately low. Everything else is either cosmetic or has
    // one obvious answer, and a manifest that demanded six fields to describe
    // an extension contributing one tool would be answered by copy-paste.
    const manifest = ExtensionManifestSchema.parse(MINIMAL);

    expect(manifest.entry).toBe('dist/index.js');
    expect(manifest.version).toBe('0.0.0');
    expect(manifest.contributes).toEqual([]);
    expect(manifest.engines.ghostai).toBe('');
  });

  it('refuses a schema tag it does not recognise', () => {
    // A literal rather than a string, for the reason the toolbox manifest gives:
    // a breaking format change has to fail loudly on the old file rather than
    // parse it into something that means something else now.
    expect(() =>
      ExtensionManifestSchema.parse({ ...MINIMAL, schema: 'ghostai.plugin/1' }),
    ).toThrow();
  });

  it('refuses a contribution kind that names no registry', () => {
    // `contributes` is what the approval screen shows the operator, so a
    // spelling the host will never act on has to fail at parse rather than
    // read as a capability nobody granted.
    expect(() =>
      ExtensionManifestSchema.parse({ ...MINIMAL, contributes: ['routes'] }),
    ).toThrow();
  });

  it('accepts every kind the host can apply', () => {
    const manifest = ExtensionManifestSchema.parse({
      ...MINIMAL,
      contributes: ['tools', 'channels', 'providers', 'context', 'commands'],
    });

    expect(manifest.contributes).toHaveLength(5);
  });
});

describe('ExtensionStatusSchema', () => {
  it('distinguishes the four ways an extension is not running', () => {
    // Each has a different fix — approve it, re-approve it, enable it, repair
    // it — so collapsing them into one `failed` would put the operator back to
    // reading logs, which is the state this row exists to replace.
    for (const state of ['unapproved', 'drifted', 'disabled', 'failed']) {
      expect(ExtensionStatusSchema.parse({ id: 'slack', state }).state).toBe(
        state,
      );
    }
  });

  it('fills every list so a row never has to guard for undefined', () => {
    const status = ExtensionStatusSchema.parse({ id: 'slack', state: 'ready' });

    expect(status.tools).toEqual([]);
    expect(status.channels).toEqual([]);
    expect(status.providers).toEqual([]);
    expect(status.commands).toEqual([]);
    expect(status.warnings).toEqual([]);
    expect(status.lastError).toBeUndefined();
  });
});

describe('RunCommandResponseSchema', () => {
  it('answers with text rather than a resource key', () => {
    // An extension's copy ships with the extension and never reaches a locale
    // bundle, so the UI renders what it is given. The same rule a toolbox's
    // `notes` follows.
    const answer = RunCommandResponseSchema.parse({
      message: 'Posted to #ops',
    });

    expect(answer).toEqual({ message: 'Posted to #ops', ok: true });
  });
});
