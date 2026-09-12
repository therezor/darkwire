/**
 * The Tools panel's form.
 *
 * Same shape as the Agent panel's — strings in, patch or errors out — with one
 * field that is not like the others. `tools.approvalTimeoutMs` is `.positive()`
 * in the schema rather than `.nonnegative()`, so unlike every other duration in
 * the tree, **zero does not mean "no limit" here.** An approval that never
 * expires is a turn that waits forever for a browser tab that was closed an hour
 * ago, holding its tool call and its provider connection open. The bound below
 * is what turns that into a message instead of a 400.
 *
 * There is no permission state here. Permission is per tool and per agent, and
 * `agents-form.ts` is where it is edited — see the header of `tools-panel.tsx`.
 */

import type { ToolsConfig } from '@ghostwire/protocol';
import type { TFunction } from 'i18next';

import {
  msToSeconds,
  parseNumber,
  secondsToMs,
} from '@/components/form/fields.js';
import type { PatchResult } from '@/components/form/fields.js';

export interface ToolsForm {
  readonly approvalTimeoutSeconds: string;
  readonly execEnabled: boolean;
  readonly execTimeoutSeconds: string;
  readonly execMaxOutputBytes: string;
  readonly maxOutputChars: string;
}

export function toToolsForm(tools: ToolsConfig): ToolsForm {
  return {
    approvalTimeoutSeconds: msToSeconds(tools.approvalTimeoutMs),
    execEnabled: tools.exec.enable,
    execTimeoutSeconds: msToSeconds(tools.exec.timeoutMs),
    execMaxOutputBytes: String(tools.exec.maxOutputBytes),
    maxOutputChars: String(tools.maxOutputChars),
  };
}

export function toToolsPatch(form: ToolsForm, t: TFunction): PatchResult {
  const errors: Record<string, string> = {};

  // `min: 1` second, not zero — see the note at the top of the file.
  const approvalTimeout = parseNumber(form.approvalTimeoutSeconds, t, {
    min: 1,
  });
  const execTimeout = parseNumber(form.execTimeoutSeconds, t, { min: 0 });
  const execMaxOutputBytes = parseNumber(form.execMaxOutputBytes, t, {
    integer: true,
    min: 1,
  });
  const maxOutputChars = parseNumber(form.maxOutputChars, t, {
    integer: true,
    min: 1,
  });

  if (!approvalTimeout.ok) {
    errors.approvalTimeoutSeconds = approvalTimeout.error;
  }
  if (!execTimeout.ok) errors.execTimeoutSeconds = execTimeout.error;
  if (!execMaxOutputBytes.ok) {
    errors.execMaxOutputBytes = execMaxOutputBytes.error;
  }
  if (!maxOutputChars.ok) errors.maxOutputChars = maxOutputChars.error;

  if (
    !approvalTimeout.ok ||
    !execTimeout.ok ||
    !execMaxOutputBytes.ok ||
    !maxOutputChars.ok
  ) {
    return { ok: false, errors };
  }

  return {
    ok: true,
    patch: {
      tools: {
        approvalTimeoutMs: secondsToMs(approvalTimeout.value),
        exec: {
          enable: form.execEnabled,
          timeoutMs: secondsToMs(execTimeout.value),
          maxOutputBytes: execMaxOutputBytes.value,
        },
        maxOutputChars: maxOutputChars.value,
      },
    },
  };
}
