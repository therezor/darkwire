/**
 * One MCP server's editable settings.
 *
 * Shaped like `provider-form.ts`, because it is the same kind of thing: a record
 * whose entries an operator creates and removes, edited by one form that serves
 * both create and edit, producing patches rather than results.
 *
 * The transport is the field everything else hangs off, and it behaves like a
 * provider's `type`: it decides which half of the form is even a question.
 * Unlike a provider's, it *can* be changed — a server that moved from stdio to
 * a URL is the same server to the operator, and the tools it offers are the
 * same tools. What cannot change is a server's id, for the reason an agent's
 * cannot: the per-agent permission map is keyed on the name it produces.
 *
 * Two records — `env` and `headers` — are edited as text and sent whole. They
 * are in `mergeConfigPatch`'s `REPLACE_WHOLESALE` list for that reason: merging
 * key by key would make removing an entry impossible to express, since an
 * absent key means "not mentioned" everywhere else in a patch.
 */

import type {
  ConfigPatch,
  McpServerConfig,
  McpTransport,
} from '@ghostwire/protocol';

import {
  formatList,
  formatRecord,
  msToSeconds,
  parseList,
  parseNumber,
  parseRecord,
  secondsToMs,
  type PatchResult,
} from '@/components/form/fields.js';
import type { TFunction } from 'i18next';

/** `['*']` in the schema, and an empty box in the form. See `toEnabledTools`. */
const ALL_TOOLS = '*';

export interface McpForm {
  readonly transport: McpTransport;
  /** stdio: the program to run, and its arguments one per line. */
  readonly command: string;
  readonly args: string;
  /** `NAME=value` per line. */
  readonly env: string;
  /** HTTP: the endpoint, and headers as `Name: value` per line. */
  readonly url: string;
  readonly headers: string;
  /** Empty means every tool the server offers. */
  readonly enabledTools: string;
  readonly toolTimeoutSeconds: string;
  readonly enabled: boolean;
  /** Off sends `oauth: null`, which is what deletes it. */
  readonly usesOAuth: boolean;
  readonly authUrl: string;
  readonly tokenUrl: string;
  readonly clientId: string;
  readonly scopes: string;
}

/** What the editor opens on for a server that does not exist yet. */
export const EMPTY_MCP_FORM: McpForm = {
  // The transport almost every server in the wild speaks, and the one
  // `resolveSpec` infers from a bare URL. A new entry should start where most
  // of them end up.
  transport: 'stdio',
  command: '',
  args: '',
  env: '',
  url: '',
  headers: '',
  enabledTools: '',
  toolTimeoutSeconds: '0',
  enabled: true,
  usesOAuth: false,
  authUrl: '',
  tokenUrl: '',
  clientId: '',
  scopes: '',
};

/**
 * The transport an existing entry is on.
 *
 * `type` is optional in the schema — it is inferred from `command` versus `url`
 * — so the form has to run the same inference the client does, or opening an
 * entry that left it out would show the wrong half of the form. This mirrors
 * `resolve_spec` in `ghostai-mcp`, which is where the rule is enforced.
 */
export function transportOf(config: McpServerConfig | undefined): McpTransport {
  if (config?.type !== undefined) return config.type;
  if ((config?.url ?? '') !== '') return 'streamableHttp';
  return 'stdio';
}

export function toMcpForm(config: McpServerConfig | undefined): McpForm {
  if (config === undefined) return EMPTY_MCP_FORM;
  const oauth = config.oauth;
  return {
    transport: transportOf(config),
    command: config.command,
    args: formatList(config.args),
    env: formatRecord(config.env),
    url: config.url,
    headers: formatRecord(config.headers),
    // `['*']` is the schema's "everything", and an empty box says the same
    // thing without making an operator type a character they cannot explain.
    enabledTools: config.enabledTools.includes(ALL_TOOLS)
      ? ''
      : formatList(config.enabledTools),
    toolTimeoutSeconds: msToSeconds(config.toolTimeoutMs),
    enabled: config.enabled,
    usesOAuth: oauth !== undefined,
    authUrl: oauth?.authUrl ?? '',
    tokenUrl: oauth?.tokenUrl ?? '',
    clientId: oauth?.clientId ?? '',
    scopes: formatList(oauth?.scopes ?? []),
  };
}

function toEnabledTools(form: McpForm): string[] {
  const named = parseList(form.enabledTools);
  return named.length === 0 ? [ALL_TOOLS] : named;
}

/**
 * The fields of an entry, whatever transport it is on.
 *
 * The half that does not apply is sent **empty** rather than omitted, and that
 * is what makes the transport switchable: `resolve_spec` refuses an entry that
 * names both a command and a url, so moving a server to a URL has to clear the
 * command it used to have. An omitted field would leave the old one in place
 * and produce a config that refuses itself.
 */
function fieldsOf(form: McpForm): {
  type: McpTransport;
  command: string;
  args: string[];
  env: Record<string, string>;
  url: string;
  headers: Record<string, string>;
  enabledTools: string[];
  toolTimeoutMs: number;
  enabled: boolean;
  oauth: McpServerConfig['oauth'] | null;
} {
  const stdio = form.transport === 'stdio';
  return {
    type: form.transport,
    command: stdio ? form.command.trim() : '',
    args: stdio ? parseList(form.args) : [],
    env: stdio ? parseRecord(form.env) : {},
    url: stdio ? '' : form.url.trim(),
    headers: stdio ? {} : parseRecord(form.headers),
    enabledTools: toEnabledTools(form),
    toolTimeoutMs: secondsToMs(Number(form.toolTimeoutSeconds.trim())),
    enabled: form.enabled,
    // `null` rather than an omission, because `oauth` is genuinely optional in
    // the schema and an absent key means "not mentioned". See `DELETE_BY_NULL`.
    oauth:
      stdio || !form.usesOAuth
        ? null
        : {
            authUrl: form.authUrl.trim(),
            tokenUrl: form.tokenUrl.trim(),
            clientId: form.clientId.trim(),
            scopes: parseList(form.scopes),
            callbackTimeoutMs: 0,
          },
  };
}

/**
 * The same question `resolveSpec` asks, asked here first.
 *
 * `new URL` rather than a pattern, and not only because it is exact: a regex
 * for a scheme contains the two slashes that `self-contained.test.ts` sweeps
 * for, and that test is right to be blunt about it — the rule it protects is
 * far easier to hold than to restore.
 */
function isHttpUrl(value: string): boolean {
  try {
    const { protocol } = new URL(value);
    return protocol === 'http:' || protocol === 'https:';
  } catch {
    return false;
  }
}

/**
 * Validates what the schema would refuse, so the operator hears it here.
 *
 * The courtesy check, not the guard: `resolveSpec` is what decides whether a
 * server can be connected, and it runs on the server against the merged tree.
 * What is checked here is only what would come back as an opaque 422.
 */
export function toMcpPatch(
  serverId: string,
  form: McpForm,
  t: TFunction,
): PatchResult {
  const errors: Record<string, string> = {};

  if (form.transport === 'stdio') {
    if (form.command.trim() === '') {
      errors.command = t('settings.fields.required');
    }
  } else if (form.url.trim() === '') {
    errors.url = t('settings.fields.required');
  } else if (!isHttpUrl(form.url.trim())) {
    errors.url = t('settings.mcp.urlScheme');
  }

  const timeout = parseNumber(form.toolTimeoutSeconds, t, { min: 0 });
  if (!timeout.ok) errors.toolTimeoutSeconds = timeout.error;

  if (form.transport !== 'stdio' && form.usesOAuth) {
    // All three are `.min(1)` in `McpOAuthConfigSchema`, so an empty one is a
    // save the server refuses without saying which field.
    if (form.authUrl.trim() === '') {
      errors.authUrl = t('settings.fields.required');
    }
    if (form.tokenUrl.trim() === '') {
      errors.tokenUrl = t('settings.fields.required');
    }
    if (form.clientId.trim() === '') {
      errors.clientId = t('settings.fields.required');
    }
  }

  if (Object.keys(errors).length > 0) return { ok: false, errors };
  return {
    ok: true,
    patch: { tools: { mcpServers: { [serverId]: fieldsOf(form) } } },
  };
}

/**
 * Switching one off, which is the reversible half of deleting it.
 *
 * Its command, its headers and its tool list all stay; it simply stops being
 * connected. A patch of its own so the list can do it without opening the
 * editor — the same shape `toProviderEnabledPatch` has.
 */
export function toMcpEnabledPatch(
  serverId: string,
  enabled: boolean,
): ConfigPatch {
  return { tools: { mcpServers: { [serverId]: { enabled } } } };
}

/** `null` is the only token the merge reads as "remove this one". */
export function toDeleteMcpPatch(serverId: string): ConfigPatch {
  return { tools: { mcpServers: { [serverId]: null } } };
}

/**
 * A free id, proposed from what the operator typed.
 *
 * The id is part of every tool name this server contributes
 * (`mcp_<id>_<tool>`), so it is worth being a readable word rather than a
 * generated one — and it cannot be renamed afterwards without every agent's
 * permission map for it going stale. The same shape `proposeInstanceId` has.
 */
export function proposeServerId(
  suggestion: string,
  taken: readonly string[],
): string {
  const base =
    suggestion
      .toLowerCase()
      .replace(/[^a-z0-9-]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 32) || 'server';
  const used = new Set(taken);
  if (!used.has(base)) return base;
  for (let n = 2; ; n += 1) {
    const candidate = `${base}-${String(n)}`;
    if (!used.has(candidate)) return candidate;
  }
}
