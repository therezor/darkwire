/**
 * The Tools panel's validation.
 *
 * The case that earns its place is the approval timeout's lower bound. Every
 * other duration in the config tree reads `0` as "no limit", and this one is
 * `.positive()` — an approval that never expires holds a turn, its tool call and
 * its provider connection open for a browser tab that was closed an hour ago.
 * The bound is easy to get wrong precisely because the convention everywhere
 * else says it should be allowed.
 */

import {
  ConfigPatchSchema,
  ToolsConfigSchema,
  type ToolsConfig,
} from '@ghostwire/protocol';
import { describe, expect, it } from 'vitest';
import { createWebI18n } from '@ghostwire/i18n/web';

/** English, resolved: these assertions compare the message a user would read. */
const t = createWebI18n('en').getFixedT(null, 'web');

import { toToolsForm, toToolsPatch } from '@/settings/tools-form.js';

const config = (overrides: Partial<ToolsConfig> = {}): ToolsConfig =>
  ToolsConfigSchema.parse(overrides);

describe('toToolsForm', () => {
  it('reads the shipped defaults', () => {
    const form = toToolsForm(config());

    expect(form.approvalTimeoutSeconds).toBe('300');
    expect(form.execEnabled).toBe(true);
  });
});

describe('toToolsPatch', () => {
  it('round-trips the config it was built from', () => {
    const result = toToolsPatch(toToolsForm(config()), t);
    expect(result.ok).toBe(true);
    if (!result.ok) return;

    expect(result.patch.tools?.approvalTimeoutMs).toBe(300_000);
    // No permission state reaches this patch: permission is per agent, and
    // `agents-form.ts` is the only thing that writes it.
    expect(result.patch.tools).not.toHaveProperty('approvals');
    expect(Object.keys(result.patch)).toEqual(['tools']);
  });

  it('refuses an approval timeout of zero, unlike every other duration here', () => {
    const result = toToolsPatch(
      { ...toToolsForm(config()), approvalTimeoutSeconds: '0' },
      t,
    );

    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.errors.approvalTimeoutSeconds).toBe('Must be at least 1');
  });

  it('allows an exec timeout of zero, where zero does mean no limit', () => {
    const result = toToolsPatch(
      { ...toToolsForm(config()), execTimeoutSeconds: '0' },
      t,
    );

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.patch.tools?.exec?.timeoutMs).toBe(0);
  });

  it('reports both bad byte counts at once', () => {
    const result = toToolsPatch(
      {
        ...toToolsForm(config()),
        execMaxOutputBytes: '0',
        maxOutputChars: '',
      },
      t,
    );

    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.errors).toEqual({
      execMaxOutputBytes: 'Must be at least 1',
      maxOutputChars: 'Required',
    });
  });

  it('produces a patch the protocol accepts', () => {
    const result = toToolsPatch(toToolsForm(config()), t);
    expect(result.ok).toBe(true);
    if (!result.ok) return;

    expect(() => ConfigPatchSchema.parse(result.patch)).not.toThrow();
  });
});
