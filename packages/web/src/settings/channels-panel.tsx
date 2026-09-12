/**
 * Settings → Channels: reaching the same agent from somewhere that is not here.
 *
 * One channel ships today, so this is a Telegram panel wearing a general name.
 * The name is the general one on purpose: a second channel is another `Section`
 * in this file rather than another tab, because what an operator is doing here
 * is the same job each time — point a chat app at this install, and say who may
 * use it.
 *
 * Two things make this panel unlike its siblings.
 *
 * **It writes to two places.** Everything except the bot token is config and
 * goes out as a settings patch; the token is a credential and goes to the vault
 * through its own endpoint, write-only, never read back. That is why the token
 * is `useState` here rather than a field on `ChannelsForm` — the form module
 * must not be able to put it in a patch by accident.
 *
 * **It reports back.** Every other panel saves a number and the number is the
 * whole outcome. Here the outcome is whether a bot on someone else's network
 * answered, so the response carries a `ChannelStatus` and the heading shows it.
 * The server restarts the channel on save, so the state an operator reads a
 * moment after saving is the real one — see `serve.ts`.
 */

import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { ChannelStatus, Config } from '@ghostwire/protocol';

import {
  SaveBar,
  Section,
  SwitchRow,
  TextField,
  TextareaField,
} from '@/components/form/controls.js';
import { Badge } from '@/components/ui/badge.js';

import {
  toChannelsForm,
  toChannelsPatch,
  type ChannelsForm,
} from './channels-form.js';
import { useSaveCredential, useSaveSettings } from './use-settings.js';

/** What the badge beside the heading says, and in which tone. */
function stateOf(status: ChannelStatus | undefined): {
  tone: 'success' | 'warning' | 'neutral';
  label: 'connected' | 'starting' | 'needsToken' | 'off';
} {
  if (status?.enabled !== true) return { tone: 'neutral', label: 'off' };
  if (!status.configured) return { tone: 'warning', label: 'needsToken' };
  return status.running
    ? { tone: 'success', label: 'connected' }
    : { tone: 'warning', label: 'starting' };
}

export function ChannelsPanel({
  config,
  channels,
}: {
  readonly config: Config;
  readonly channels: readonly ChannelStatus[];
}): JSX.Element {
  const { t } = useTranslation();
  const { save, saving } = useSaveSettings();
  const { save: saveCredential } = useSaveCredential();

  const status = channels.find((channel) => channel.id === 'telegram');
  const [form, setForm] = useState<ChannelsForm>(() => toChannelsForm(config));
  /**
   * The token as typed, and `undefined` for "leave the vault alone".
   *
   * Three states rather than two, and the empty one is the interesting case: an
   * operator who clears a box holding a saved key means to remove it, and an
   * operator who never touched it means nothing at all. `undefined` is the
   * second; `''` is the first.
   */
  const [token, setToken] = useState<string | undefined>(undefined);
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>({});
  const [dirty, setDirty] = useState(false);

  const update = <K extends keyof ChannelsForm>(
    key: K,
    value: ChannelsForm[K],
  ): void => {
    setForm((current) => ({ ...current, [key]: value }));
    setDirty(true);
  };

  const onSave = (): void => {
    const result = toChannelsPatch(form, t);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    save(result.patch, {
      onSuccess: () => {
        // After the settings, not beside them: a credential write restarts the
        // channel, and restarting it against settings that have not landed yet
        // would start the bot on the old allowlist.
        if (token !== undefined) {
          saveCredential({
            namespace: 'channels',
            key: 'telegram',
            value: token === '' ? null : token,
          });
          setToken(undefined);
        }
      },
    });
    setDirty(false);
  };

  const state = stateOf(status);

  return (
    <div className="stack settings-panel">
      <Section
        title={t('settings.channels.telegramTitle')}
        description={t('settings.channels.telegramDesc')}
      >
        <p className="cluster">
          <Badge tone={state.tone}>
            {t(`settings.channels.state.${state.label}` as const)}
          </Badge>
          {status?.detail === undefined ? null : (
            <span className="settings-field__hint">{status.detail}</span>
          )}
        </p>

        <SwitchRow
          label={t('settings.channels.enabled')}
          hint={t('settings.channels.enabledHint')}
          checked={form.enabled}
          onCheckedChange={(checked) => {
            update('enabled', checked);
          }}
        />

        <TextField
          label={t('settings.channels.token')}
          type="password"
          autoComplete="new-password"
          spellCheck={false}
          value={token ?? ''}
          placeholder={
            status?.configured === true
              ? t('settings.channels.tokenSaved')
              : t('settings.channels.tokenPlaceholder')
          }
          hint={
            status?.configured === true
              ? t('settings.channels.tokenReplaceHint')
              : t('settings.channels.tokenHint')
          }
          onValueChange={(value) => {
            setToken(value);
            setDirty(true);
          }}
        />
      </Section>

      <Section
        title={t('settings.channels.accessTitle')}
        description={t('settings.channels.accessDesc')}
      >
        <TextareaField
          label={t('settings.channels.allowlist')}
          value={form.allowlist}
          rows={4}
          spellCheck={false}
          error={errors.allowlist}
          hint={t('settings.channels.allowlistHint')}
          onValueChange={(value) => {
            update('allowlist', value);
          }}
        />
        <TextareaField
          label={t('settings.channels.admins')}
          value={form.admins}
          rows={3}
          spellCheck={false}
          error={errors.admins}
          hint={t('settings.channels.adminsHint')}
          onValueChange={(value) => {
            update('admins', value);
          }}
        />
      </Section>

      <SaveBar
        dirty={dirty}
        saving={saving}
        onSave={onSave}
        onRevert={() => {
          setForm(toChannelsForm(config));
          setToken(undefined);
          setErrors({});
          setDirty(false);
        }}
      />
    </div>
  );
}
