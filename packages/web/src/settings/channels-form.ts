/**
 * The Channels panel's form.
 *
 * Same shape as the others — strings in, patch or errors out — with one thing
 * none of them have: **the block this edits is not in the config type.**
 * `channels` is a `looseObject` precisely so a channel owns its own settings,
 * so `config.channels.telegram` is `unknown` here and has to be narrowed on the
 * way in. That is the cost of the looseness, and it is paid once, here.
 *
 * The bot token is deliberately absent from both the form and the patch. It is
 * a credential: it goes to the vault through `PUT /api/settings/credentials`,
 * never into `config.json`, so the panel holds it in its own state and this
 * module never sees it.
 */

import type { Config } from '@ghostwire/protocol';
import type { TFunction } from 'i18next';

import { formatList, parseList } from '@/components/form/fields.js';
import type { PatchResult } from '@/components/form/fields.js';

export interface ChannelsForm {
  readonly enabled: boolean;
  /** One entry per line: `<telegram id>` or `<telegram id>|<label>`. */
  readonly allowlist: string;
  readonly admins: string;
}

/** `config.channels.telegram`, narrowed. Unknown to the type, loose by design. */
function telegramBlock(config: Config): Readonly<Record<string, unknown>> {
  const block = config.channels.telegram;
  return typeof block === 'object' && block !== null && !Array.isArray(block)
    ? (block as Readonly<Record<string, unknown>>)
    : {};
}

function stringsAt(
  block: Readonly<Record<string, unknown>>,
  key: string,
): readonly string[] {
  const value = block[key];
  return Array.isArray(value)
    ? value.filter((entry): entry is string => typeof entry === 'string')
    : [];
}

export function toChannelsForm(config: Config): ChannelsForm {
  const block = telegramBlock(config);
  return {
    // Absent means enabled, which is what the channel manager does: it skips
    // only a block that says `false`. Reading it any other way would show a
    // switch that disagrees with the running bot.
    enabled: block.enabled !== false,
    allowlist: formatList(stringsAt(block, 'allowlist')),
    admins: formatList(stringsAt(block, 'admins')),
  };
}

/** `<id>` or `<id>|<label>`, where the id is what is matched on. */
function idOf(entry: string): string {
  return (entry.split('|')[0] ?? '').trim();
}

/**
 * The patch, or what is wrong with the form.
 *
 * The allowlist is validated here rather than left to the channel, because the
 * channel reports a bad entry by refusing to start — which from a settings
 * panel is a save that appears to work and a bot that quietly stops answering.
 * Saying it in the field is the difference.
 */
export function toChannelsPatch(form: ChannelsForm, t: TFunction): PatchResult {
  const errors: Record<string, string> = {};
  const allowlist = parseList(form.allowlist);
  const admins = parseList(form.admins);

  const badEntry = [...allowlist, ...admins].find((entry) => {
    const id = idOf(entry);
    return id === '' || !Number.isSafeInteger(Number(id)) || Number(id) === 0;
  });
  if (badEntry !== undefined) {
    const key = allowlist.includes(badEntry) ? 'allowlist' : 'admins';
    errors[key] = t('settings.channels.badId', { entry: badEntry });
  }

  // Enabling a bot that will answer nobody is the one combination the channel
  // refuses to start on, so it is refused here where there is room to say why.
  if (form.enabled && allowlist.length === 0) {
    errors.allowlist = t('settings.channels.allowlistRequired');
  }

  // An admin who is not on the allowlist cannot talk to the bot at all, so an
  // admin list that names one is a typo every time.
  const stranger = admins.find(
    (entry) => !allowlist.some((allowed) => idOf(allowed) === idOf(entry)),
  );
  if (stranger !== undefined && errors.admins === undefined) {
    errors.admins = t('settings.channels.adminNotAllowed', {
      entry: idOf(stranger),
    });
  }

  if (Object.keys(errors).length > 0) return { ok: false, errors };

  return {
    ok: true,
    // The whole block this panel owns, not only the edited field: arrays
    // replace on merge, and a partial patch would leave the half it did not
    // mention as whatever it was before the operator emptied it.
    patch: {
      channels: { telegram: { enabled: form.enabled, allowlist, admins } },
    },
  };
}
