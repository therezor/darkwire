/**
 * The settings tree.
 *
 * Conventions:
 *
 *  - **`0` means "no limit"** on every `*TimeoutMs` and `*PerMinute` field, so
 *    a limit can be disabled without a separate nullable flag.
 *  - **Durations are named with their unit.** A bare `toolTimeout: 40` is
 *    ambiguous between seconds and milliseconds at every call site;
 *    `toolTimeoutMs` is not.
 *  - **Every nested object uses `.prefault({})`**, so `ConfigSchema.parse({})`
 *    yields a fully-populated tree. The empty object is fed *through* the child
 *    schema so the child's own defaults apply, which `.default()` (output-typed
 *    in Zod 4) could not do without restating every leaf.
 *  - **No `.transform()` anywhere.** Normalisation (trimming an `apiBase` to
 *    `undefined`, expanding `~`) happens at load time in `darkwire-core`, which
 *    keeps input and output types identical and every schema here
 *    representable as JSON Schema for the OpenAPI document.
 */

import { z } from 'zod';

import { DEFAULT_AGENT_ID } from './ids.js';
import {
  ToolPermissionSchema,
  ToolPermissionsSchema,
  ToolPromptOverridesSchema,
  type ToolPermission,
  type ToolPermissions,
} from './tools.js';

/** A duration in milliseconds where `0` disables the limit. */
const OptionalDurationMs = z.number().int().nonnegative();

/**
 * How hard to ask the model to think, where `off` is a value and unset is not.
 *
 * The distinction is the whole point of having `off` at all. Unset means the
 * request carries no reasoning parameter and the provider applies its own —
 * which is the only thing that works against an endpoint that rejects the field
 * outright. `off` is a statement: this model thinks by default and I do not
 * want it to, so send whatever this wire spells that as. What that is per
 * endpoint lives in `ProviderSpec.reasoningOffBody`.
 *
 * `xhigh` is the other end and, unlike `off`, is not invented here: it is what
 * Qwen3.8 calls its top rung and the level that model runs at unless told
 * otherwise. It goes to the wire as it is written.
 */
export const ReasoningEffortSchema = z.enum([
  'off',
  'minimal',
  'low',
  'medium',
  'high',
  'xhigh',
]);
export type ReasoningEffort = z.infer<typeof ReasoningEffortSchema>;

/**
 * How an agent's system prompt is assembled.
 *
 * `template` is the two-half assembly: an identity template and a live-state
 * template, with the platform note and the tool-output policy filled in as
 * sections the operator may also replace. It is
 * the default and the one that keeps a provider's prompt cache working, because
 * everything that changes between requests sits in the tail.
 *
 * `raw` hands the whole system message to one template. Nothing is prepended or
 * appended — a `raw` prompt that wants the tool-output policy names
 * `{{toolPolicy}}`. It exists because "you own the prompt" and "you own the
 * prompt as long as you fill in our sections" are different claims, and only the
 * first one is worth making. The cost is stated in `RAW_PROMPT_PLACEHOLDERS`.
 */
export const PromptModeSchema = z.enum(['template', 'raw']);
export type PromptMode = z.infer<typeof PromptModeSchema>;

/**
 * Strips a `.default()` / `.prefault()` wrapper, leaving the schema underneath.
 *
 * Unconstrained on purpose: a `ZodRawShape`'s values are typed as the internal
 * `$ZodType`, so constraining to the public `z.ZodType` makes every mapped-type
 * application fail its own constraint check.
 */
type Unwrapped<T> =
  T extends z.ZodDefault<infer Inner>
    ? Inner
    : T extends z.ZodPrefault<infer Inner>
      ? Inner
      : T;

type PatchShape<S extends z.ZodRawShape> = {
  [K in keyof S]: Unwrapped<S[K]> extends z.ZodType
    ? z.ZodOptional<Unwrapped<S[K]>>
    : never;
};

/**
 * Turns a config schema into a true patch schema: every field optional **and
 * stripped of its default**.
 *
 * `.partial()` alone is not enough and is actively wrong here. It marks keys
 * optional but leaves the inner `ZodDefault` in place, so parsing `{}` returns
 * every default rather than an empty object — and since a patch is deep-merged
 * into the live config, saving one settings panel would silently rewrite every
 * field the client never mentioned back to its default.
 *
 * It is up here with the other helpers rather than beside `ConfigPatchSchema`
 * because a `ConfigPatch` is not the only place the shape is needed — the
 * settings panel builds one, and so does every caller that saves a section.
 */
function patchOf<S extends z.ZodRawShape>(
  schema: z.ZodObject<S>,
): z.ZodObject<PatchShape<S>> {
  const shape: Record<string, z.ZodType> = {};
  const fields = schema.shape as unknown as Record<string, z.ZodType>;
  for (const [key, field] of Object.entries(fields)) {
    const type = field.def.type;
    const base =
      type === 'default' || type === 'prefault'
        ? (field as z.ZodDefault<z.ZodType>).unwrap()
        : field;
    shape[key] = base.optional();
  }
  // The mapped type above states the result precisely; the loop cannot express it.
  return z.object(shape) as unknown as z.ZodObject<PatchShape<S>>;
}

// One agent's settings

/**
 * The working folder every agent shares.
 *
 * Root-level rather than on an agent, because an agent *works in* a workspace
 * and does not own one: the folder is a property of the session, and several
 * agents with separate identities opening the same one is the thing this is
 * built around. `AgentEntrySchema` therefore cannot name it, and a test says so.
 *
 * Empty means `~/DarkWire/workspaces`. Deliberately *not* defaulted to that
 * literal string: writing one machine's home directory into the file makes the
 * config non-portable and a container mount silently wrong. A relative path
 * here is resolved against the root, never against the process working
 * directory, and `DARKWIRE_WORKSPACES` wins over whatever this says.
 *
 * Merged per field like every other root key, which is what makes an omitted
 * key preserve it. It must never move under a `REPLACE_WHOLESALE` path: there an
 * omission *deletes*, so a settings save touching something else would silently
 * reset the configured folder.
 */
export const WorkspacesPathSchema = z.string().default('');

// The exec tool's own settings. Above `AgentSettingsSchema` because an agent
// carries them: two agents on one install can want different answers.

export const ExecToolConfigSchema = z.object({
  timeoutMs: OptionalDurationMs.default(0),
  pathAppend: z.string().default(''),
  /**
   * `argv[0]` allow-list. Empty means "anything not denied": the deny list and
   * the workspace jail still apply.
   *
   * Note what is *not* here: patterns for `$(...)`, backticks or `| sh`. The
   * exec tool takes `argv: string[]` and runs `execFile` with `shell: false`,
   * so there is no string for a shell metacharacter to live in. Scanning for
   * them would reject legitimate commands while blocking nothing.
   */
  allowedBinaries: z.array(z.string()).default([]),
  deniedBinaries: z.array(z.string()).default([]),
  /** Environment variables passed through to the child. */
  envAllowlist: z.array(z.string()).default(['PATH', 'HOME', 'LANG', 'TZ']),
  maxOutputBytes: z
    .number()
    .int()
    .positive()
    .default(1024 * 1024),
});
export type ExecToolConfig = z.infer<typeof ExecToolConfigSchema>;

/**
 * What one agent sends, and what it costs.
 *
 * Every agent states its own. There is no inheritance layer above this: a field
 * an entry does not name is filled by the schema's own default, not by another
 * agent's answer. That is the whole model, and it is why the two fields below
 * that are `.optional()` with no default mean "send nothing and let the provider
 * decide" rather than "look somewhere else".
 */
export const AgentSettingsSchema = z.object({
  /**
   * Empty means *unconfigured*, not "pick one for me".
   *
   * The distinction is worth stating because the neighbouring `provider` field
   * genuinely does resolve itself, and this one was documented as though it did
   * too — which put a "Resolved automatically" option in the agent editor that
   * saved an agent nothing could run. There is no model-picking code anywhere:
   * `Runtime#resolveProvider` turns an empty model into `noModelError` and
   * hands the loop a `null` provider, so `runtime.configured` goes false and
   * every turn is refused. It is the fresh-install state — the setup wizard's
   * model step is skippable — and the UI treats it as a question to answer.
   */
  model: z.string().default(''),
  /**
   * `auto` runs the resolution order; otherwise a provider *instance* id.
   *
   * A bare provider type is still accepted and means "any instance of that
   * type, or a default one if none is configured" — which is what keeps
   * `darkwire chat --provider ollama` working on a machine with no config file.
   */
  provider: z.string().min(1).default('auto'),
  maxTokens: z.number().int().positive().default(8192),
  contextWindowTokens: z.number().int().positive().default(65_536),
  /**
   * Optional, and unset is not the same as `0`.
   *
   * Unset means the request carries no `temperature` at all and the provider
   * applies its own — which is the only correct answer for the models that
   * reject the parameter outright, and the honest one for the rest, since a
   * default here is this project's guess at someone else's tuning. The range is
   * also not universal: most providers cap at 2, some at 1, and a few reasoning
   * models accept nothing but their own. So the setting is "say nothing unless
   * you mean it", exactly like `reasoningEffort` beside it.
   */
  temperature: z.number().min(0).max(2).optional(),
  maxToolIterations: z.number().int().positive().default(40),
  toolTimeoutMs: OptionalDurationMs.default(0),
  /** Wall-clock cap on one turn, checked at the top of each loop iteration. */
  loopWallTimeoutMs: OptionalDurationMs.default(0),
  subagentTimeoutMs: OptionalDurationMs.default(0),
  reasoningEffort: ReasoningEffortSchema.optional(),
  /**
   * Whether attached images are sent to the model as images.
   *
   * Off, an attachment still reaches the model — as the path line it always
   * carries, which `read` and the rest resolve — but never as an `image`
   * part. That is the difference between a text-only model answering "I cannot
   * see it, let me open it" and the request being rejected outright.
   *
   * The reactive half of this already existed: `stripImages` in
   * `darkwire-providers` removes images *after* an endpoint has refused them.
   * This is the same repair moved to before the round trip, for the case where
   * the operator already knows.
   */
  visionEnabled: z.boolean().default(true),
  /**
   * Whether the request advertises any tools at all.
   *
   * Off is not the same as denying every tool: the agent's permissions are left
   * exactly as configured and simply not offered to *this* model. Switch the
   * agent to a model that can call tools and its toolset is still there.
   *
   * It has no reactive counterpart, which is why it is here. The degradation
   * ladder deliberately never strips `tools` — a turn where the model cannot
   * act and answers from memory is a wrong answer rather than a failed request
   * — so an endpoint that cannot take a tool list has, until now, had no way to
   * be used at all.
   */
  toolsEnabled: z.boolean().default(true),
  /**
   * What the `exec` tool may run for this agent, and for how long.
   *
   * Whether the agent has `exec` at all is the permission map's answer, like
   * every other tool; there is no second switch here to disagree.
   */
  exec: ExecToolConfigSchema.prefault({}),
  /**
   * How long to wait for a decision before treating an `ask` call as denied.
   *
   * `.positive()`, unlike every other duration in this tree: an approval that
   * never expires holds the turn open for a browser tab that was closed an
   * hour ago.
   */
  approvalTimeoutMs: z
    .number()
    .int()
    .positive()
    .default(5 * 60 * 1000),
  /**
   * Head+tail truncation budget for a single tool result.
   *
   * **`.positive()`, and 0 does not mean "no limit" here.** This is also an
   * *allocation* bound: `read` sizes its read from it, so 0 would make it
   * read one byte of every file. An operator who wants effectively no cap sets
   * a large number, which is bounded and says what it means.
   */
  maxOutputChars: z.number().int().positive().default(8192),
  /**
   * Send the model `tool_search` plus the pinned tools, and nothing else.
   *
   * Off sends every tool the agent permits. On, the rest are reachable by name
   * through `tool_search` and stay in the list for the rest of the session
   * once the model activates one.
   */
  lazyDiscovery: z.boolean().default(false),
  /**
   * Tools that stay in the list while `lazyDiscovery` is on.
   *
   * Names, not permissions: a pin widens nothing, and a tool this agent denies
   * is still not sent. Replaced whole on a patch, so a pin can be removed.
   * `tool_search` itself is never pinned or hidden.
   */
  pinnedTools: z.array(z.string()).default([]),
});
export type AgentSettings = z.infer<typeof AgentSettingsSchema>;

// The named agents these belong to live further down, after the tool schemas
// they override — see "Agents".

// Providers

/**
 * One configured endpoint. API keys are deliberately absent: they live in the
 * encrypted `CredentialVault` under the `providers` namespace, keyed by the
 * *instance* id, so a `config.yaml` is safe to commit or paste into a bug
 * report.
 *
 * `type` is what makes an instance distinct from a provider. Two Ollama servers
 * — a laptop and a GPU box — are two entries with the same `type` and different
 * `apiBase`, which the previous shape (one entry per provider id) could not
 * express at all. It is validated against the registry table by
 * `darkwire-providers`, not here: this package sits upstream of that table and
 * cannot see it, which is the same reason `ProvidersConfig` is a record rather
 * than one named field per provider.
 */
export const ProviderConfigSchema = z.object({
  /** A `darkwire-providers` registry id — `ollama`, `openai`, `custom`. */
  type: z.string().min(1),
  /** Shown in the UI. Empty falls back to the type's display name. */
  label: z.string().default(''),
  apiBase: z.string().optional(),
  extraHeaders: z.record(z.string(), z.string()).default({}),
  /**
   * Models to offer for this instance.
   *
   * A fallback rather than the catalogue: an endpoint that answers `GET /models`
   * is enumerated live, and this is what an operator typed for one that does
   * not — or what is offered while a server is unreachable.
   */
  models: z.array(z.string()).default([]),
  /** A disabled instance is kept, and skipped by resolution and model listing. */
  enabled: z.boolean().default(true),
});
export type ProviderConfig = z.infer<typeof ProviderConfigSchema>;

/**
 * Keyed by *instance* id, which is an operator's label rather than a provider id.
 *
 * Keying by provider id instead would cap the tree at one endpoint per
 * provider. The two spellings are deliberately compatible: a file whose keys
 * *are* provider ids needs only `type` = the key to move over, and every
 * credential already in the vault keeps resolving under the same string.
 * There is no automatic migration — a file without `type` is an error naming the
 * key, which is this project's rule for every old shape.
 */
export const ProvidersConfigSchema = z
  .record(z.string(), ProviderConfigSchema)
  .default({});
export type ProvidersConfig = z.infer<typeof ProvidersConfigSchema>;

// Server

export const AuthConfigSchema = z.object({
  /**
   * Disabling auth on a non-loopback bind is a startup *error*, not a warning —
   * see `isLoopbackHost`. A warning is not enough: it scrolls past, and the
   * result is an unauthenticated shell-capable agent on a LAN address.
   */
  enabled: z.boolean().default(true),
  sessionTtlMs: z
    .number()
    .int()
    .positive()
    .default(30 * 24 * 60 * 60 * 1000),
  rateLimitPerMinute: OptionalDurationMs.default(0),
  /** Lifetime of the HMAC-signed URLs that serve workspace media to `<img>`. */
  signedUrlTtlMs: z
    .number()
    .int()
    .positive()
    .default(10 * 60 * 1000),
});

export const ServerConfigSchema = z.object({
  host: z.string().min(1).default('127.0.0.1'),
  /**
   * One port for the API, the WebSocket and the static UI. DarkWire is
   * single-process by default; nothing in it is heavy enough to justify the
   * reconnect-and-HTTP-fallback client a split-process topology would need.
   */
  port: z.number().int().min(1).max(65_535).default(3000),
  auth: AuthConfigSchema.prefault({}),
  /**
   * How many server events to retain per session so a reconnecting tab can
   * replay an in-flight turn from its last `seq` instead of losing it.
   *
   * A count of *frames*, and a turn that streams a long answer spends one per
   * token — so this is not the knob that decides whether a reload comes back to
   * the whole turn. `turnLogMaxBytes` is. This one decides how far back a
   * *reconnect* can pick up across turns, which is a much smaller ask.
   */
  replayBufferSize: z.number().int().nonnegative().default(512),
  /**
   * The budget for retaining the turn that is running, whole, so a reload comes
   * back to all of it — including every nested subagent step.
   *
   * Bytes rather than frames because frames are not the cost: the log merges
   * adjacent deltas of the same part, so a hundred-thousand-token answer is one
   * entry, and what is actually retained is tool output. A turn that reads fifty
   * large files is the shape that reaches this; a turn that writes for ten
   * minutes is not.
   *
   * Held only while a turn is open, and only by the session running it, so the
   * ceiling is the number of concurrent turns rather than the number of
   * sessions. Past it the log stops retaining and says so, and a resume falls
   * back to the stored tail alone — the behaviour before the log existed. `0`
   * disables it outright.
   */
  turnLogMaxBytes: z
    .number()
    .int()
    .nonnegative()
    .default(16 * 1024 * 1024),
});

/**
 * Whether `host` binds only to the local machine.
 *
 * A pure predicate rather than a schema refinement: the caller needs to explain
 * *why* startup was refused, and cross-field validation would also make this
 * schema unrepresentable as JSON Schema. `darkwire-server` calls it during
 * boot; `0.0.0.0` and `::` are the wildcard binds that must count as remote.
 */
export function isLoopbackHost(host: string): boolean {
  const h = host
    .trim()
    .toLowerCase()
    .replace(/^\[|\]$/g, '');
  if (h === 'localhost' || h === '::1') return true;
  if (h === '0.0.0.0' || h === '::' || h === '') return false;
  // 127.0.0.0/8 — any of the 16 million loopback addresses, not just .0.1.
  return /^127\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.test(h);
}

// Tools

export const McpOAuthConfigSchema = z.object({
  authUrl: z.string().min(1),
  tokenUrl: z.string().min(1),
  clientId: z.string().min(1),
  scopes: z.array(z.string()).default([]),
  callbackTimeoutMs: OptionalDurationMs.default(0),
});
export type McpOAuthConfig = z.infer<typeof McpOAuthConfigSchema>;

export const McpTransportSchema = z.enum(['stdio', 'sse', 'streamableHttp']);
export type McpTransport = z.infer<typeof McpTransportSchema>;

export const McpServerConfigSchema = z.object({
  /** Inferred from `command` vs `url` when omitted. */
  type: McpTransportSchema.optional(),
  command: z.string().default(''),
  args: z.array(z.string()).default([]),
  env: z.record(z.string(), z.string()).default({}),
  url: z.string().default(''),
  headers: z.record(z.string(), z.string()).default({}),
  oauth: McpOAuthConfigSchema.optional(),
  toolTimeoutMs: OptionalDurationMs.default(0),
  /** `["*"]` exposes everything the server advertises. */
  enabledTools: z.array(z.string()).default(['*']),
  enabled: z.boolean().default(true),
});
export type McpServerConfig = z.infer<typeof McpServerConfigSchema>;

/**
 * The tool layer's install-wide half: the MCP servers and nothing else.
 *
 * Everything about how a tool runs for an agent, from `exec` to the result
 * budget, is on the agent (`AgentSettingsSchema`), because two agents on one
 * install can reasonably want different answers to all of it.
 */
/**
 * How this install reaches the web, for `web_fetch` and `web_search`.
 *
 * Install-wide rather than per agent, unlike `exec` and the result budget.
 * These describe the shape of an outbound connection and the machine making it:
 * which backend answers a search, how this install identifies itself, what
 * every request is bounded by, and one cache shared by everything. None of it
 * is something one agent should be able to answer differently from another.
 *
 * **No API keys, deliberately.** Both backends are keyless. What an agent may
 * *reach* is still the agent's, in its `EnvironmentNetwork` allow-list.
 */
export const WebSearchProviderSchema = z.enum(['auto', 'searxng']);
export type WebSearchProvider = z.infer<typeof WebSearchProviderSchema>;

export const WebToolsConfigSchema = z.object({
  /** Which backend answers `web_search`. */
  searchProvider: WebSearchProviderSchema.default('auto'),
  /** The SearXNG instance, for `searxng`. Ignored otherwise. */
  searchUrl: z.string().default(''),
  /**
   * Sent verbatim, with no client hints.
   *
   * Empty is the built-in browser profile, which is what gets past most bot
   * walls. A value here is the operator choosing to be identifiable, and the
   * client hints are dropped with it: a hint set naming Chrome beside a custom
   * agent is a contradiction that gives the whole thing away.
   */
  userAgent: z.string().default(''),
  /** Per fetch. */
  timeoutSeconds: z.number().int().positive().default(20),
  /** Per page read inside a search, so one slow origin cannot eat the batch. */
  readTimeoutSeconds: z.number().int().positive().default(15),
  /** The streaming body cap. */
  maxBytes: z
    .number()
    .int()
    .positive()
    .default(5 * 1024 * 1024),
  /** Pages and result sets held in memory. `0` disables the cache. */
  cacheEntries: z.number().int().min(0).default(64),
  /** How long a cached entry stays usable. `0` disables the cache. */
  cacheTtlSeconds: z.number().int().min(0).default(900),
});
export type WebToolsConfig = z.infer<typeof WebToolsConfigSchema>;

export const ToolsConfigSchema = z.object({
  mcpServers: z.record(z.string(), McpServerConfigSchema).default({}),
  /** How this install reaches the web. */
  web: WebToolsConfigSchema.prefault({}),
});
export type ToolsConfig = z.infer<typeof ToolsConfigSchema>;

// Agents
//
// Below the tool schemas rather than beside `AgentSettings`, because an agent
// overrides them: this is the one place in the tree where the dependency runs
// from an agent to the tools rather than the other way round.

/**
 * Which tools an agent may call, and what happens when it does.
 *
 * One map, not a selection plus a policy. A tool the map does not mention is
 * not enabled — it never reaches the definitions the model is sent — so
 * enabling a tool and choosing its permission are one act. That is the opposite
 * of the convention next door in `ExecToolConfig.allowedBinaries`, where empty
 * means "anything not denied", and deliberately so: an allow-list of *binaries*
 * is a narrowing of one tool an operator already turned on, while this is the
 * list of tools themselves, and a newly created agent quietly holding every
 * tool the registry happens to carry is the failure this shape prevents.
 *
 * Which is why a new agent is not born empty either — see `DEFAULT_AGENT_TOOLS`.
 *
 * The shape is `ToolPermissionsSchema` itself rather than an alias of it: the
 * schema registry asserts every entry is a distinct object, and a second name
 * for one schema would be a `$ref` in the OpenAPI document pointing at nothing
 * the wire distinguishes. The type alias below is for readers, not for zod.
 */
export type AgentTools = ToolPermissions;

/**
 * What a newly created agent starts with: the built-in tools, at the permission
 * their risk band implies.
 *
 * The one place a risk band still turns into a permission, and it happens once,
 * at creation, where an operator can see the result and change it. Nothing
 * reads a band at call time.
 *
 * Seeding rather than starting empty because an agent that can do nothing looks
 * broken to whoever just made it — and because the alternative reading of
 * "explicit" would be a setup chore five clicks long before the first turn.
 */
export const DEFAULT_AGENT_TOOLS: Readonly<Record<string, ToolPermission>> =
  Object.freeze({
    read: 'allow',
    ls: 'allow',
    // Searching is what an agent does before almost every edit, and both of
    // these are confined to the workspace, so they are seeded on.
    grep: 'allow',
    find: 'allow',
    write: 'allow',
    edit: 'allow',
    exec: 'ask',
    // These two are the switches for memory and skills — denying the tool also
    // removes the prompt section it feeds. Seeded on, because an agent that
    // silently fails to remember reads as broken rather than as unconfigured.
    // Note this is the seed for a *new* agent: an install that predates them has
    // neither until an operator grants it. See `docs/memory.md`.
    memory: 'allow',
    skill: 'allow',
    // The plan a long turn runs on, and the switch for the Tasks section of the
    // prompt. Seeded on: an agent that cannot say what it is doing is the thing
    // this exists to fix.
    todo: 'allow',
  });

/**
 * How much of the network an agent's environment reaches.
 *
 * The one place egress is configured. An environment definition decides whether a
 * restricted gateway *can* be built — a non-root numeric uid, no-new-privs, no
 * packet-forging capability — and this decides what that gateway permits.
 * Splitting the two across both files is what produced a "ceiling" nobody could
 * find the other half of.
 *
 * One list, whatever the destination looks like. An entry is a CIDR block, an
 * address, a name, or a name with a leading dot covering its subdomains. Blocks
 * and addresses are enforced by the gateway's packet filter; names are enforced
 * by the egress proxy, which sees the name rather than the address a name
 * resolved to, and so is the one thing DNS rebinding cannot defeat.
 *
 * Nothing inside an environment resolves a name. The proxy does it, on the
 * engine's side of the boundary, which is why there is no resolver to configure
 * and why a name is reachable only over HTTP and HTTPS.
 *
 * Strict, unlike most of this file. This field decides what an agent can reach,
 * and a key that was silently dropped would read as an allow-list that had been
 * applied.
 */
export const NetworkModeSchema = z.enum(['none', 'allowlist', 'open']);
export type NetworkMode = z.infer<typeof NetworkModeSchema>;

export const EnvironmentNetworkSchema = z.strictObject({
  mode: NetworkModeSchema.default('none'),
  /** What `allowlist` permits: blocks, addresses, names, `.suffix` names. */
  allow: z.array(z.string()).default([]),
});
export type EnvironmentNetwork = z.infer<typeof EnvironmentNetworkSchema>;

/**
 * Where this agent's command operations run, and what they can reach.
 *
 * An empty `name` is the behaviour that has always existed: a child process on
 * the machine running DarkWire, inside the workspace jail, where a network
 * request means nothing and is refused rather than ignored. A named environment
 * routes command operations through the sandbox service instead.
 *
 * The image, capabilities, hardening and sharing live in the installed
 * definition and have no representation here. The network *does* live here:
 * egress is the one thing an operator configures per agent rather than per
 * image, and a single place to configure it is worth more than a second ceiling
 * nobody could locate.
 */
export const AgentEnvironmentSchema = z.object({
  /** An installed environment name, or empty to run on the host. */
  name: z.string().default(''),
  /** What this agent's environment may reach. */
  network: EnvironmentNetworkSchema.prefault({}),
  /**
   * Whether this agent brings its own environment when something delegates to
   * it.
   *
   * Off, the default, means a delegated turn runs where its caller does. That
   * is what people expect of a subagent: work handed down stays inside the
   * boundary the operator chose rather than falling back to the host halfway
   * down a chain. At the top of a chain there is no caller, so the agent runs
   * in the environment named above either way.
   *
   * On pins it to that environment whoever called. A web-search agent with a
   * browser in its image is the case: it is useless anywhere else, and the
   * caller cannot be expected to know that.
   *
   * It is a property of the agent rather than of one delegation because an
   * agent that needs its own toolchain needs it from every caller. Asking per
   * relationship asks the same question once per parent and lets two of them
   * disagree.
   */
  alwaysUseOwn: z.boolean().default(false),
});
export type AgentEnvironment = z.infer<typeof AgentEnvironmentSchema>;

/**
 * Another agent this one may hand a task to.
 *
 * A subagent is not a different kind of thing from an agent — it is an ordinary
 * entry in `agents.list` that some other entry points at. That is the whole
 * design: a researcher is configured, tested and used on its own, and being
 * someone's subagent is a relationship rather than a mode. It also means the
 * model and tool map a subagent runs under are already answered
 * by the entry it names, and nothing here restates them.
 *
 * Three fields, and the two that are not the id both exist because the operator
 * is the one who knows things the schema cannot:
 *
 *  - **`prompt` is the tool description the model reads.** Not a note beside it —
 *    the description *is* how a model decides whether to call something, so an
 *    operator writing "use this when you need facts you do not have; ask for a
 *    summary, not raw sources" is writing the only part of this feature that
 *    decides when it fires. Empty falls back to a sentence naming the agent.
 *  - **`permission` sits here rather than in `tools`.** The `tools` map is a list
 *    of installed tools, and a subagent is not one — putting `ask_researcher`
 *    there would render it in the editor's Tools section under a "not installed"
 *    badge, because it is absent from `/api/tools` and always will be.
 *
 * `allow` is the default because delegation is the feature: an agent given a
 * subagent is an agent whose operator wants it used. The tools the *subagent*
 * runs are gated by the subagent's own map, which is where the risk actually is.
 */
export const SubagentRefSchema = z.object({
  /** An id in `agents.list`. Checked against it by `assertBuildable`. */
  id: z.string(),
  /** The operator's guidance. Empty means the built-in sentence. */
  prompt: z.string().default(''),
  permission: ToolPermissionSchema.default('allow'),
});
export type SubagentRef = z.infer<typeof SubagentRefSchema>;

/**
 * One named agent, complete.
 *
 * Built on `AgentSettingsSchema` rather than on a patch of it, so an entry that
 * names three fields parses into an agent that states all of them: the rest come
 * from the *schema's* defaults. Nothing is inherited from anywhere, which is what
 * lets a reader answer "what does this agent run on" from the entry alone.
 *
 * The config stays as short as it ever was — `{ "label": "Coder", "model":
 * "qwen3" }` is still a whole agent — because brevity was never coming from the
 * inheritance, only the indirection was.
 *
 * `model` is the one field with no useful default: empty means *unconfigured*,
 * and an agent in that state is listed, editable and refused a turn rather than
 * quietly borrowing somebody else's. See `AgentSettingsSchema.model`.
 *
 * `workspaces` is deliberately absent. The working folder is root-level and
 * shared — see `WorkspacesPathSchema`.
 */
export const AgentEntrySchema = AgentSettingsSchema.extend({
  /** Shown in the UI. Empty falls back to the id. */
  label: z.string().default(''),
  /**
   * This agent's whole static system prompt, as a template.
   *
   * Not an addition to a built-in block — it replaces one. Empty means the
   * built-in `DEFAULT_SYSTEM_PROMPT_TEMPLATE`, which is what keeps an install
   * that never customised a prompt receiving improvements to it on upgrade.
   * See `prompt.ts` for the placeholder set and the substitution rules.
   */
  systemPrompt: z.string().default(''),
  /**
   * The per-iteration half's live-state section, as a template.
   *
   * Beside `systemPrompt` and for the same reason — an operator owns what their
   * agent is told — but with the opposite economics: this half is never cached,
   * so every line is re-sent on every request of every turn. Empty means the
   * built-in `DEFAULT_LIVE_STATE_TEMPLATE`. Its placeholder vocabulary is
   * `LIVE_PROMPT_PLACEHOLDERS`, which is *not* the identity half's: `{{time}}`
   * belongs only here, and `{{workspaceId}}` only there.
   *
   * Setting it to a single space is how an operator removes the section
   * entirely, since empty means "use the built-in".
   */
  livePrompt: z.string().default(''),
  /**
   * What is appended in the last few iterations of a turn.
   *
   * Separate from `livePrompt` because it is conditional and a placeholder
   * template cannot express a condition. Empty means the built-in
   * `DEFAULT_WRAP_UP_TEMPLATE`; a single space silences it.
   */
  wrapUpPrompt: z.string().default(''),
  /**
   * Whether `systemPrompt` is the static half or the entire system message.
   *
   * `template` — the default — leaves the four templates around it in force.
   * `raw` stops *placing* anything: no live-state block or tool-output policy
   * and the rest.
   *
   * The three section templates below still decide what those placeholders
   * render *to*, so raw controls the layout rather than discarding the wording.
   * `livePrompt` is the exception and the only field raw ignores outright: its
   * entire content is `{{time}}{{wrapUp}}`, both of which a raw template names
   * directly.
   */
  promptMode: PromptModeSchema.default('template'),
  /**
   * The `## Running commands` section: where commands run, and what is there.
   * Fills `{{platformPolicy}}`.
   *
   * Empty inherits a built-in that depends on placement, and which one applies
   * is decided per turn, because a subagent runs where its caller's reference
   * says it does. On the host that built-in is the rule `guardExec` enforces.
   * In a container it is the environment definition's own `prompt`, because
   * only the image knows what it holds; an image that says nothing places no
   * section at all. A single space removes the section.
   *
   * Editing it does not widen anything. Where a command may reach is decided by
   * `guardExec` and the workspace jail, neither of which reads the prompt; this
   * is the sentence that tells the model what those two will do.
   */
  platformPrompt: z.string().default(''),
  /**
   * @deprecated Read by nothing. `platformPrompt` above is the one section
   * about placement, and in a container its built-in is already the
   * definition's own words.
   *
   * Still parsed so an agent that set it can be told its wording is not placed,
   * rather than losing it in silence.
   */
  environmentPrompt: z.string().optional(),
  /**
   * The `## Tool output policy` section, as a template.
   *
   * Editable like the rest, and the one that deserves a sentence about what
   * that does and does not mean. The envelopes around tool results are emitted
   * by the runtime and the nonce is regenerated per turn whatever this says —
   * so this text is the *explanation* of a defence, not the defence. Deleting
   * it leaves the fences in place and the model with no reason to respect them,
   * which is why a template with no `{{nonce}}` and no `{{tag}}` saves with a
   * warning rather than silently.
   */
  toolPolicyPrompt: z.string().default(''),
  /**
   * The `## Memory` section, as a template.
   *
   * Only rendered while the agent may call `memory` and the workspace has at
   * least one — a permission of `deny` produces no section whatever this says,
   * and neither does an empty `memory/` folder. Empty means the built-in; a
   * single space removes the section, which is how an operator whose own
   * `systemPrompt` already explains the folder stops paying for it twice.
   *
   * Its vocabulary is `MEMORY_PROMPT_PLACEHOLDERS`, and `{{index}}` is the one
   * that matters: the generated lines are the section's content, and a
   * template omitting it advertises a memory folder while naming nothing in
   * it.
   */
  memoryPrompt: z.string().default(''),
  /**
   * The `## Skills` section, as a template.
   *
   * Only rendered while the agent may call `skill` and the workspace has at
   * least one — a permission of `deny` produces no section whatever this says,
   * and neither does an empty `skills/` folder. Empty means the built-in; a
   * single space removes the section.
   *
   * Its vocabulary is `SKILLS_PROMPT_PLACEHOLDERS`. Unlike the memory
   * template's, its `{{index}}` carries its own leading blank line, because it
   * can be empty and a section should leave no gap where it would have been.
   *
   * The sheets themselves are not here. The template renders the catalogue;
   * a body reaches the model only when the agent opens the file it names.
   */
  skillsPrompt: z.string().default(''),
  /**
   * Per-tool replacements for the description and the parameter descriptions
   * the model is sent.
   *
   * Keyed by advertised tool name, so it reaches built-ins, MCP and extension
   * tools and `ask_<id>` subagent tools alike. For a subagent
   * this wins over `subagents[].prompt`, being the more specific of the two.
   *
   * A key naming no advertised tool is a warning, not an error: a tool can
   * leave the list because an extension was uninstalled or `exec` was disabled,
   * and neither should stop an agent that was working a moment ago.
   */
  toolPrompts: ToolPromptOverridesSchema.default({}),
  enabled: z.boolean().default(true),
  /**
   * Replaces, never merges. An entry that names three tools has three tools —
   * the seed is what a *new* agent gets, not a floor every agent stands on,
   * or switching a tool off would be impossible to express.
   */
  tools: ToolPermissionsSchema.default({ ...DEFAULT_AGENT_TOOLS }),
  /** Command placement for the built-in `exec` tool. */
  environment: AgentEnvironmentSchema.prefault({}),
  /**
   * Agents this one may delegate to. Order is the order the model sees them.
   *
   * A list rather than a record keyed by id because the order is the
   * operator's and a record has none — and because "the same agent twice" is
   * a mistake `assertBuildable` should name, not a shape the schema silently
   * collapses.
   */
  subagents: z.array(SubagentRefSchema).default([]),
});
export type AgentEntry = z.infer<typeof AgentEntrySchema>;

/** Every agent this install has. There is nothing above them. */
export const AgentsConfigSchema = z.object({
  /**
   * Keyed by an id the operator chooses, which also names the agent's directory
   * on disk — so it follows the workspace id rules.
   *
   * `default` is prefaulted into existence because it is the agent every unbound
   * conversation runs on, and with no settings layer above it there is nothing
   * else for it to resolve from. A fresh install therefore has exactly one agent,
   * complete and unconfigured, rather than none.
   *
   * `prefault` rather than `default`, so the literal is parsed *through*
   * `AgentEntrySchema` on every call and two parses cannot share one object.
   */
  list: z
    .record(z.string(), AgentEntrySchema)
    .prefault({ [DEFAULT_AGENT_ID]: {} }),
});

// Scheduler, channels, extensions

/**
 * The engine, and nothing about any one job.
 *
 * Every key here is true of the *scheduler*; none of them describes a task.
 * That line is the whole shape of this block, and it is worth stating because a
 * `heartbeat` sub-block would break it: an `intervalMin`, a `file`, a `model`,
 * an `agentId`, a `sessionKey` and its own `enabled` is a second way to
 * describe one scheduled job.
 *
 * A heartbeat **is** a job. Its interval is the job's schedule, its file and
 * model are the job's payload, and its on/off is the job's own flag. Two
 * vocabularies for one concept is how an operator configures the half that does
 * not run, so there is one: `AutomationJob`.
 */
export const SchedulerConfigSchema = z.object({
  enabled: z.boolean().default(true),
  /** Concurrent automation runs. Two keeps a slow job from blocking the queue. */
  concurrency: z.number().int().positive().default(2),
  /** Run `at` jobs whose time passed while the process was down. */
  catchUpOnBoot: z.boolean().default(true),
  /**
   * Runs kept per job, trimmed on write.
   *
   * Per job rather than a global cap: a nightly job's year of history must not
   * be evicted by a five-minute job's afternoon, which is exactly what one
   * shared ceiling would do. Unbounded is not an option — a job on a
   * five-minute interval writes about 105,000 rows a year.
   */
  runRetention: z.number().int().positive().default(200),
});

/**
 * Channel settings. Loose by design: each channel — built-in or from an
 * extension — parses its own block, so installing a channel does not require a
 * schema change here. Telegram ships in the box but consumes the same
 * `ChannelFactory` contract an extension would, so the contract cannot rot.
 */
export const ChannelsConfigSchema = z.looseObject({
  sendProgress: z.boolean().default(true),
  sendToolHints: z.boolean().default(false),
});
export type ChannelsConfig = z.infer<typeof ChannelsConfigSchema>;

/**
 * Extension settings.
 *
 * Per-extension configuration is a `settings` sub-object rather than the loose
 * top level `channels` uses, and the difference is not taste: this block
 * already has keys of its own, so an extension whose id happened to be `load`
 * or `disabled` would silently overwrite one. Inside `settings` each block is
 * loose for the same reason a channel's is — installing an extension must not
 * require a schema change here, and the extension parses its own block.
 */
export const ExtensionsConfigSchema = z.object({
  /**
   * Extra directories to load from, beside `~/.darkwire/extensions`.
   *
   * A path, never a package spec. Nothing here fetches: an extension is a
   * directory an operator put on the box, which is what keeps an air-gapped
   * install air-gapped.
   */
  load: z.array(z.string()).default([]),
  disabled: z.array(z.string()).default([]),
  /** Lets a later-discovered extension shadow an earlier id instead of erroring. */
  allowOverride: z.boolean().default(false),
  settings: z.record(z.string(), z.looseObject({})).default({}),
});
export type ExtensionsConfig = z.infer<typeof ExtensionsConfigSchema>;

/**
 * What the install looks and reads like, for both surfaces.
 *
 * Its own section rather than a field on `server`, because nothing here is
 * transport: `server` is ports, hosts and auth, and a locale is neither. Theme
 * is the natural next occupant.
 *
 * `locale` is a bare `z.string()` on purpose. An enum would have to enumerate
 * the shipped languages, which would give `protocol` a dependency on
 * `@darkwire/i18n` for a value that changes every time a translation lands — and
 * would turn a config naming a language this build does not carry into a parse
 * failure that takes the whole file down. `resolveLocale` narrows an unknown tag
 * to the nearest match and ultimately to English, so an unrecognised value costs
 * a fallback rather than a broken install.
 *
 * `assertBootPolicy` reads only the `server` subtree, so nothing here is boot
 * policy — and `settings.reload` already exists, which is what makes a language
 * change take effect without a restart.
 */
export const ReasoningDisplaySchema = z.enum([
  'hidden',
  'collapsed',
  'expanded',
]);
export type ReasoningDisplay = z.infer<typeof ReasoningDisplaySchema>;

export const UiConfigSchema = z.object({
  /** A BCP-47 tag. Unknown values fall back rather than failing to parse. */
  locale: z.string().default('en'),
  /**
   * How much of the model's reasoning a reader is shown by default.
   *
   * Three states rather than a switch, and the middle one is why. `hidden`
   * stops the reasoning reaching a surface at all, which is what somebody who
   * never wants to see it means. `collapsed` is the default: the run is there,
   * labelled, one row, and opening it is a click or a keystroke. A switch would
   * have had to pick one of those two to be "off", and both readings are
   * reasonable.
   *
   * It governs both surfaces because it is a property of the install rather
   * than of the terminal. This UI collapses reasoning already, so only `hidden`
   * changes anything here.
   */
  reasoning: ReasoningDisplaySchema.default('collapsed'),
  /**
   * Whether a tool's output arrives open.
   *
   * A switch and not three states, unlike reasoning above: the text is written
   * either way, so there is no third thing for "off" to mean.
   */
  expandToolOutput: z.boolean().default(false),
  /**
   * Whether the terminal shows what a turn cost as it finishes.
   *
   * `· 2 steps · 3.9k in / 134 out · 2.5s · 68.9 tok/s`, which is worth having
   * and is not worth a row under every answer. A switch for the same reason as
   * `expandToolOutput`: the line is written either way and `ctrl-y` reveals it,
   * so there is no third thing for "off" to mean.
   *
   * The terminal only. The browser puts the same figures in a turn-info
   * popover, which is already out of the way.
   */
  expandTurnStats: z.boolean().default(false),
  /**
   * The one zone this install reads and writes clock times in.
   *
   * Everything is *stored* in UTC — every persisted instant is epoch
   * milliseconds — so this is not a storage format. It is the answer to "whose
   * clock", and it is deliberately a single install-wide answer rather than one
   * per job: three timezone controls (a per-job zone, a scheduler default, and
   * whatever the viewer's browser happens to be set to) meant an operator had to
   * hold all three in their head to predict when a job fires.
   *
   * It governs both halves, and that is the point. A timestamp is *rendered* in
   * this zone, and a wall-clock time is *read* in it — so a cron written
   * `0 9 * * *` fires at 9am on the same clock the next-run line is printed
   * against, and nobody converts anything by hand.
   *
   * **A concrete IANA name, never a rule.** `system` is offered by the settings
   * select and resolved to a real zone before it is saved, exactly as the
   * language select resolves its own `system`. Storing the rule instead would
   * mean the server resolved it to the host zone while a browser resolved it to
   * the viewer's, which is the disagreement this field exists to end.
   *
   * **UTC rather than the host zone as the default**, and that is the point of
   * the default. A server's zone is a property of where it happens to be
   * running — it moves when the box moves, it is whatever the image was built
   * with, and on a laptop it follows the traveller. A schedule written
   * `0 9 * * *` would then fire at a different real instant after a migration
   * nobody connected to it. UTC is the one zone that does not drift, and an
   * operator who wants local time says so once, here.
   *
   * A bare `z.string()` for the reason `locale` is: an enum would have to
   * enumerate the IANA database, which changes without this schema. It is
   * validated where it is used — `parseCron` refuses a zone `Intl` does not
   * know, which surfaces as a 422 on the job that names it.
   */
  timezone: z.string().min(1).default('UTC'),
});

// Root

export const ConfigSchema = z.object({
  /** The folder the workspaces live in. See `WorkspacesPathSchema`. */
  workspaces: WorkspacesPathSchema,
  agents: AgentsConfigSchema.prefault({}),
  providers: ProvidersConfigSchema,
  server: ServerConfigSchema.prefault({}),
  tools: ToolsConfigSchema.prefault({}),
  channels: ChannelsConfigSchema.prefault({}),
  scheduler: SchedulerConfigSchema.prefault({}),
  extensions: ExtensionsConfigSchema.prefault({}),
  ui: UiConfigSchema.prefault({}),
});
export type Config = z.infer<typeof ConfigSchema>;

/**
 * A settings patch from the UI or CLI.
 *
 * Deep-partial rather than `ConfigSchema.partial()`: the settings panel saves
 * one section at a time, so `{ agents: { model: 'x' } }` must validate without
 * restating the sibling fields — and must not invent them.
 *
 * **Strict, and that is the point.** A plain `z.object` *strips* a key it does
 * not know, so a client writing a section this schema no longer has gets a 200
 * and a save that changed nothing. That is not a hypothetical: removing
 * `agents.defaults` left every already-loaded browser tab sending
 * `{agents: {defaults: {provider, model}}}` for `/model`, and the answer was
 * "the agent now runs it. Saved." over a config the request never touched.
 * Refusing names the key instead, which is the only signal that reaches an old
 * client — it does not read a new warning field, but its existing failure path
 * does show a 4xx.
 *
 * Strict here and on `agents` alone, which is where the two shapes a client can
 * be wrong about live: a whole section, and a block inside it. The nested
 * `patchOf` blocks stay loose deliberately — `extensions.settings` holds shapes
 * this layer cannot know, and a strict `agents.list.*` would refuse the entry
 * the settings panel reads back and sends whole.
 */
export const ConfigPatchSchema = z.strictObject({
  agents: z
    .strictObject({
      /**
       * `null` deletes the agent; an object creates or updates one. Same
       * reasoning as `providers` below — an absent key means "not mentioned",
       * so removing an agent needs a syntax the merge can tell apart.
       *
       * The nested blocks are restated as patches for the same reason `tools`
       * is below: `patchOf` is not recursive, so without this an operator
       * toggling `sandbox.network` would have to resend the image, the workdir
       * and the kind alongside it.
       */
      list: z
        .record(
          z.string(),
          patchOf(AgentEntrySchema)
            .extend({
              // Not a patch: the map replaces wholesale, because a patch that
              // merged key by key could add a tool and change a permission but
              // never remove one. `agents.list.*` is in the merge's
              // `REPLACE_WHOLESALE` list, so this is what already happens — it
              // is restated here so the type says it.
              tools: ToolPermissionsSchema.optional(),
              // Restated as a patch for the reason `environment` is below: an
              // editor that changes the exec switch should not have to resend
              // the allow-list beside it.
              exec: patchOf(ExecToolConfigSchema).optional(),
              // `network` is restated because `patchOf` is not recursive, and
              // without it a save that only changes the mode would have to
              // resend `allow` — which is how a settings panel silently clears
              // the allow-list it never rendered.
              environment: patchOf(AgentEnvironmentSchema)
                .extend({
                  network: patchOf(EnvironmentNetworkSchema).optional(),
                })
                .optional(),
              // `subagents` is deliberately *not* restated beside these. It is
              // an array, so it already replaces wholesale — there is no
              // per-field merge for `patchOf` to be non-recursive about, and
              // `patchOf` leaves the element schema's own defaults intact, so
              // `[{ id: 'researcher' }]` still arrives with a permission.
            })
            .nullable(),
        )
        .optional(),
    })
    .optional(),
  /**
   * `null` deletes the instance; an object creates or updates one.
   *
   * Deletion needs a syntax of its own because the merge treats an absent key
   * as "not mentioned" — there is otherwise no way to remove a provider the
   * operator added. `mergeConfigPatch` honours the null only under the paths in
   * its `DELETE_BY_NULL` list, so it cannot be used to punch a hole in a struct.
   *
   * `patchOf` makes `type` optional, which is right for editing an instance
   * that already has one. Creating an instance without naming a type fails the
   * merged tree's re-parse, which is a 400 saying exactly that.
   */
  providers: z
    .record(z.string(), patchOf(ProviderConfigSchema).nullable())
    .optional(),
  server: patchOf(ServerConfigSchema)
    .extend({ auth: patchOf(AuthConfigSchema).optional() })
    .optional(),
  tools: patchOf(ToolsConfigSchema)
    .extend({
      /**
       * `null` deletes the server, exactly as it does for a provider instance.
       *
       * Restated here for the same two reasons `providers` is: `patchOf` is not
       * recursive, so without this an entry would have to be resent whole to
       * change one field — and a record whose entries an operator adds and
       * removes needs a syntax for "remove this one". `mergeConfigPatch` has
       * listed `tools.mcpServers.*` in `DELETE_BY_NULL` since before there was
       * a client; until this line the null was rejected here, one layer above,
       * so that entry could never fire.
       */
      mcpServers: z
        .record(
          z.string(),
          patchOf(McpServerConfigSchema)
            .extend({
              /**
               * `null` says this server does not use OAuth.
               *
               * Needed because `oauth` is genuinely optional rather than
               * defaulted: "unset" is a real state, and an absent key already
               * means "not mentioned". The same reason
               * an agent's entry replaces wholesale.
               */
              oauth: McpOAuthConfigSchema.nullable().optional(),
            })
            .nullable(),
        )
        .optional(),
      // Restated because `patchOf` is not recursive: a save that only
      // changes a timeout must not resend the cache settings beside it.
      web: patchOf(WebToolsConfigSchema).optional(),
    })
    .optional(),
  /**
   * Loose, unlike the rest: an extension channel's config block is an unknown
   * key here, and a stripping patch schema would drop it on every save.
   */
  channels: z
    .looseObject({
      sendProgress: z.boolean().optional(),
      sendToolHints: z.boolean().optional(),
    })
    .optional(),
  scheduler: patchOf(SchedulerConfigSchema).optional(),
  /**
   * Written out rather than `patchOf`, for the reason `tools.mcpServers` is:
   * an extension's settings block has to be deletable, and `patchOf` cannot
   * make a record's *value* nullable.
   */
  extensions: z
    .object({
      load: z.array(z.string()).optional(),
      disabled: z.array(z.string()).optional(),
      allowOverride: z.boolean().optional(),
      /** `null` deletes one extension's block. */
      settings: z.record(z.string(), z.looseObject({}).nullable()).optional(),
    })
    .optional(),
  ui: patchOf(UiConfigSchema).optional(),
  /**
   * Stripped of its default, not `WorkspacesPathSchema.optional()`.
   *
   * `.optional()` leaves the `ZodDefault` in place, so a patch parsed from `{}`
   * would carry `workspaces: ''` and every settings save would reset a
   * configured folder to "unset". That is the exact hazard `patchOf` exists to
   * prevent, and this is the one root field that needs it written out by hand.
   */
  workspaces: z.string().optional(),
});
export type ConfigPatch = z.infer<typeof ConfigPatchSchema>;

// Editing one agent's model and sampling settings

/**
 * The fields a chat surface can move without opening the agent editor.
 *
 * `model` and `provider` are one setting and travel together: a model sent
 * without the instance that offers it leaves `provider` naming an endpoint that
 * has never heard of the model. Every caller has a whole `ModelInfo` in hand, so
 * neither is optional-in-practice — they are optional here only because
 * `/temperature` changes neither.
 *
 * `null` means *clear this*, and only the two genuinely optional fields accept
 * it. Cleared means the request carries no such parameter and the provider
 * applies its own — there is nothing above an agent for it to fall back to.
 */
export interface AgentSettingsChange {
  readonly model?: string;
  readonly provider?: string;
  readonly temperature?: number | null;
  readonly reasoningEffort?: ReasoningEffort | null;
}

/**
 * A patch that moves one agent's model or sampling settings and nothing else.
 *
 * Written once because of the rule it encodes: **`agents.list.*` is in the
 * merge's `REPLACE_WHOLESALE` list, so the patch *is* the agent.** A patch naming
 * `model` alone does not set one field — it replaces the entry and takes the
 * label, the system prompt, the tools, the environment and the subagent roster with
 * it. So the stored entry is read, spread, and sent back whole, and clearing a
 * field means deleting the key rather than nulling it.
 *
 * An id with no entry yields a patch that creates one. That is the honest answer
 * for an agent deleted underneath a conversation: the turn will fall back to the
 * default agent anyway, and writing a half-agent under a dead id would be worse
 * than writing a whole one.
 *
 * Pure, and deliberately in `@darkwire/protocol` rather than beside the merge
 * it encodes. Both callers need it and only this package reaches both: the web
 * bundle cannot import `darkwire-runtime` or `darkwire-core` at all.
 */
export function agentSettingsPatch(
  config: Config,
  agentId: string,
  changes: AgentSettingsChange,
): ConfigPatch {
  // Spread first, then apply — so every field this function does not know about
  // survives the replacement it is about to be part of.
  const next = {
    ...(config.agents.list[agentId] ?? AgentEntrySchema.parse({})),
  };
  if (changes.model !== undefined) next.model = changes.model;
  if (changes.provider !== undefined) next.provider = changes.provider;
  // Deleted rather than nulled: the entry replaces wholesale, so an absent key
  // is how "send no such parameter" is spelled. A `null` would reach
  // `AgentEntrySchema` as a value and be rejected.
  if (changes.temperature === null) delete next.temperature;
  else if (changes.temperature !== undefined) {
    next.temperature = changes.temperature;
  }
  if (changes.reasoningEffort === null) delete next.reasoningEffort;
  else if (changes.reasoningEffort !== undefined) {
    next.reasoningEffort = changes.reasoningEffort;
  }

  return { agents: { list: { [agentId]: next } } };
}
