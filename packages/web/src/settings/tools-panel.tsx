/**
 * The Tools panel: what running a tool costs, not who may run one.
 *
 * **There is no permission control here, deliberately.** Permission is per tool
 * and per agent — `agents.list.<id>.tools` — so it is edited in the agent
 * editor, beside the agent it governs. A global four-row matrix of risk band to
 * `allow`/`ask`/`deny` could not answer the question anyone actually has:
 * `exec: ask` says nothing about which tools it governs, and an agent's own
 * tool list could admit a tool the matrix then refused.
 *
 * What is left is the settings that are genuinely install-wide: how long a
 * prompt stays open, what `exec` may do when some agent is allowed to call it,
 * and how much output a result may carry.
 *
 * **There is no inventory of registered tools here either.** Every tool with
 * its risk badge would be a list you could read but not act on. The list that
 * matters is the one in the agent editor, where every row has a control on it.
 */

import { useState, type JSX } from 'react';
import { ContainersPanel } from './containers-panel.js';
import { useTranslation } from 'react-i18next';

import type { Config } from '@ghostwire/protocol';

import {
  FieldGrid,
  SaveBar,
  Section,
  SwitchRow,
  TextField,
} from '@/components/form/controls.js';
import { toToolsForm, toToolsPatch, type ToolsForm } from './tools-form.js';
import { useSaveSettings } from './use-settings.js';

export function ToolsPanel({
  config,
}: {
  readonly config: Config;
}): JSX.Element {
  const { t } = useTranslation();
  const [form, setForm] = useState<ToolsForm>(() => toToolsForm(config.tools));
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>({});
  const [dirty, setDirty] = useState(false);
  const { save, saving } = useSaveSettings();

  const update = <K extends keyof ToolsForm>(
    key: K,
    value: ToolsForm[K],
  ): void => {
    setForm((current) => ({ ...current, [key]: value }));
    setDirty(true);
  };

  const onSave = (): void => {
    const result = toToolsPatch(form, t);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    save(result.patch);
    setDirty(false);
  };

  return (
    <div className="stack settings-panel">
      <ContainersPanel />
      <Section
        title={t('settings.tools.approvalTitle')}
        description={t('settings.tools.approvalDesc')}
      >
        <TextField
          label={t('settings.tools.approvalTimeout')}
          inputMode="decimal"
          value={form.approvalTimeoutSeconds}
          error={errors.approvalTimeoutSeconds}
          onValueChange={(value) => {
            update('approvalTimeoutSeconds', value);
          }}
          hint={t('settings.tools.approvalTimeoutHint')}
        />
      </Section>

      <Section
        title={t('settings.tools.executionTitle')}
        description={t('settings.tools.executionDesc')}
      >
        {/* The one control here that overlaps with a per-agent permission, so
            the hint's job is to say how the two differ: this one is the whole
            install, and it wins. */}
        <SwitchRow
          label={t('settings.tools.enableExec')}
          hint={t('settings.tools.enableExecHint')}
          checked={form.execEnabled}
          onCheckedChange={(checked) => {
            update('execEnabled', checked);
          }}
        />
        <FieldGrid>
          <TextField
            label={t('settings.tools.commandTimeout')}
            inputMode="decimal"
            value={form.execTimeoutSeconds}
            error={errors.execTimeoutSeconds}
            disabled={!form.execEnabled}
            onValueChange={(value) => {
              update('execTimeoutSeconds', value);
            }}
            hint={t('settings.tools.execTimeoutHint')}
          />
          <TextField
            label={t('settings.tools.maxOutputBytes')}
            inputMode="numeric"
            value={form.execMaxOutputBytes}
            error={errors.execMaxOutputBytes}
            disabled={!form.execEnabled}
            onValueChange={(value) => {
              update('execMaxOutputBytes', value);
            }}
          />
        </FieldGrid>
      </Section>

      <Section
        title={t('settings.tools.resultsTitle')}
        description={t('settings.tools.resultsDesc')}
      >
        <TextField
          label={t('settings.tools.maxChars')}
          inputMode="numeric"
          value={form.maxOutputChars}
          error={errors.maxOutputChars}
          onValueChange={(value) => {
            update('maxOutputChars', value);
          }}
          hint={t('settings.tools.maxCharsHint')}
        />
      </Section>

      <SaveBar
        dirty={dirty}
        saving={saving}
        onSave={onSave}
        onRevert={() => {
          setForm(toToolsForm(config.tools));
          setErrors({});
          setDirty(false);
        }}
      />
    </div>
  );
}
