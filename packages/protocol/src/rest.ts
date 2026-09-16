/**
 * REST DTOs.
 *
 * These are the objects the server publishes under `components.schemas` in the
 * OpenAPI 3.1 document.
 * The API reference is generated from them and never hand-maintained, so it
 * cannot drift from the routes it documents.
 *
 * Cursor pagination for anything read sequentially: sessions and messages are
 * append-only, so an offset shifts under a reader whenever a turn lands. The
 * three listings that a numbered pager also reads — sessions, automation runs,
 * notifications — accept an `offset` as well, and carry a `total` a page of rows
 * cannot supply. The two modes are alternatives; see `PaginationQuerySchema`.
 */

import { z } from 'zod';

import {
  ConfigPatchSchema,
  ConfigSchema,
  EnvironmentNetworkSchema,
  McpTransportSchema,
  ReasoningEffortSchema,
} from './config.js';
import { EnvironmentDefinitionSchema } from './environment.js';
import {
  StopReasonSchema,
  StoredMessageSchema,
  UsageSchema,
} from './messages.js';
import { SubagentRunRefSchema } from './subagent.js';
import { ToolDefinitionSchema } from './tools.js';
import { AutomationJobSchema, AutomationRunSchema } from './automation.js';
import { ExtensionContributionSchema } from './extension.js';

// Envelopes

/**
 * The single error shape for every non-2xx response, so a client has one branch
 * to write. Mirrors the WS `error` event's code vocabulary.
 */
export const ErrorResponseSchema = z.object({
  error: z.object({
    code: z.string().min(1),
    message: z.string(),
    /** Field-level detail for a 422, keyed by JSON pointer. */
    details: z.record(z.string(), z.unknown()).optional(),
  }),
});
export type ErrorResponse = z.infer<typeof ErrorResponseSchema>;

/**
 * How a client asks for one page, in either of the two ways.
 *
 * **`cursor` and `offset` are alternatives, never a pair.** A cursor addresses a
 * position in the sort order and an offset counts rows from the top, so sending
 * both asks for a page relative to a page; the endpoints refuse the combination
 * with a 400 rather than letting one silently win.
 *
 * Which to send is a property of the reader, not of the endpoint. A sequential
 * one — the sidebar, an infinite scroll — wants `cursor`, because these tables
 * move under it: a turn landing between two requests bumps a session to the
 * front, which shifts every offset behind it and makes the reader see one row
 * twice and miss another. A numbered pager wants `offset`, because "page 7"
 * cannot be expressed as a position it has not visited, and it is jumping around
 * a list rather than reading through it.
 */
export const PaginationQuerySchema = z.object({
  limit: z.number().int().positive().max(200).default(50),
  /** Opaque; echo back `nextCursor` verbatim. */
  cursor: z.string().optional(),
  /** Rows to skip from the top. Mutually exclusive with `cursor`. */
  offset: z.number().int().nonnegative().default(0),
});
export type PaginationQuery = z.infer<typeof PaginationQuerySchema>;

// Status

export const StatusResponseSchema = z.object({
  version: z.string(),
  protocolVersion: z.number().int().positive(),
  uptimeMs: z.number().int().nonnegative(),
  /**
   * Resolved, not configured — reflects what a turn would actually use now.
   * Both are empty when nothing is configured yet; `configured` is the flag to
   * branch on, so a client never has to read meaning into an empty string.
   */
  model: z.string(),
  provider: z.string(),
  /**
   * Whether a turn can run at all.
   *
   * `false` on a fresh install: the server, the files, the settings and the
   * socket are all up, and only chat is unavailable until a provider and a
   * model exist.
   */
  configured: z.boolean(),
  /**
   * The default workspace's id, never its path.
   *
   * Carrying `jail.root` here — an absolute host path — would teach every
   * authenticated client the operator's username and directory layout, which is
   * the one string that turns a blind traversal attempt into a targeted one.
   * Absolute paths do not cross this boundary in either direction.
   */
  workspaceId: z.string().min(1),
  workspaceCount: z.number().int().positive(),
  authEnabled: z.boolean(),
  toolCount: z.number().int().nonnegative(),
  mcpServersConnected: z.number().int().nonnegative(),
  extensionsLoaded: z.number().int().nonnegative(),
});
export type StatusResponse = z.infer<typeof StatusResponseSchema>;

/** `ghost doctor` output. Defined early so the shape is stable before the CLI depends on it. */
export const HealthCheckSchema = z.object({
  name: z.string().min(1),
  status: z.enum(['ok', 'warn', 'fail', 'skipped']),
  detail: z.string().default(''),
});

export const HealthResponseSchema = z.object({
  status: z.enum(['ok', 'degraded', 'fail']),
  checks: z.array(HealthCheckSchema),
});
export type HealthResponse = z.infer<typeof HealthResponseSchema>;

// Settings

/**
 * Something the settings say that could not be honoured, but did not stop the
 * install from running.
 *
 * The counterpart to a `config` `GhostError`, which refuses the whole tree. An
 * agent id is user-authored and deletable, so a reference to one that has gone
 * has to be survivable — and the only alternative to a warning is discarding it
 * silently, which is how an operator ends up with a delegation that stopped
 * working and nothing that says when.
 */
export const ConfigWarningSchema = z.object({
  code: z.string().min(1),
  message: z.string(),
  /** The agent the warning is about, when it is about one. */
  agentId: z.string().optional(),
});
export type ConfigWarning = z.infer<typeof ConfigWarningSchema>;

/**
 * One channel, as the settings panel needs to see it.
 *
 * Four fields rather than one, because "is my bot working" has four distinct
 * answers and collapsing them loses the one the operator has to act on. A
 * channel that is `enabled` but not `configured` needs a token; one that is
 * both but not `running` either failed to start — `detail` says why — or has
 * not been restarted yet.
 *
 * `configured` exists because the vault is write-only over HTTP: the panel can
 * never read a token back, so a boolean is the only way it can say "a token is
 * saved" instead of showing an empty box over a working bot.
 */
export const ChannelStatusSchema = z.object({
  /** The channel id, which is also its `config.channels` key. */
  id: z.string().min(1),
  /** What the settings say. */
  enabled: z.boolean(),
  /** A credential is stored. Never the credential itself. */
  configured: z.boolean(),
  /** The channel is connected right now. */
  running: z.boolean(),
  /**
   * What to show beside the state: the bot's username when connected, or why
   * it is not. Absent when there is nothing to add.
   */
  detail: z.string().optional(),
});
export type ChannelStatus = z.infer<typeof ChannelStatusSchema>;

/**
 * Config as served to the UI. Credentials never appear — the vault is
 * write-only over HTTP — so the panel gets a per-provider boolean instead.
 */
export const SettingsResponseSchema = z.object({
  config: ConfigSchema,
  /** Provider *instance* id → whether a usable key exists in the vault. */
  credentialsPresent: z.record(z.string(), z.boolean()),
  /**
   * Channels this build ships, whether configured or not.
   *
   * A separate field rather than more keys in `credentialsPresent`, which is
   * documented as provider instances and is indexed by id — a `telegram` entry
   * there would collide with an endpoint an operator happened to name the same
   * thing, and would carry none of the other three answers a panel needs.
   */
  channels: z.array(ChannelStatusSchema).default([]),
  /** Set when the file on disk failed to parse and defaults are in use. */
  loadError: z.string().optional(),
  /**
   * Non-fatal problems found resolving the settings. Empty is healthy.
   *
   * A sibling of `loadError` rather than a widening of it: that field means the
   * file did not parse *at all* and defaults are standing in, which is one
   * string and one alert. These are individually addressable and render as a
   * list, and folding both into one field would leave the UI no way to tell
   * "nothing loaded" from "three delegations were dropped".
   */
  warnings: z.array(ConfigWarningSchema).default([]),
});
export type SettingsResponse = z.infer<typeof SettingsResponseSchema>;

/**
 * One agent moving to a new id, as part of a settings save.
 *
 * A rename travels *with* the patch rather than through a route of its own, and
 * the reason is that it is not separable from one. The editor's Save can change
 * an agent's id and its model in the same gesture, and as two requests that is
 * two writes with a window between them: the first can land and the second fail,
 * leaving the agent under its new name holding its old settings.
 *
 * What a patch alone cannot say is which of two things a key move *means* —
 * `{ "reviewer": null, "code-review": {…} }` describes "rename reviewer" and
 * "delete reviewer, create code-review" equally well, and the two are opposites:
 * a rename takes the conversations bound to the old id and its standing tool
 * approvals across, where a delete-and-recreate must strand the first and refuse
 * the second, because an id is user-authored and anyone can create one under a
 * name that was just freed. Naming the rename is how the caller says which.
 */
export const AgentRenameSchema = z.object({
  from: z.string().min(1),
  to: z.string().min(1),
});
export type AgentRename = z.infer<typeof AgentRenameSchema>;

/**
 * The body of `PATCH /api/settings`: a config patch, plus what it means.
 *
 * `ConfigPatchSchema` and nothing else was the shape until agents could be
 * renamed. It stays the whole of it in every other respect — the extra field is
 * not config and is never stored, it is read and discarded by the route, which
 * is why it is an extension here rather than a branch of `ConfigSchema`.
 */
export const SettingsPatchRequestSchema = ConfigPatchSchema.extend({
  /**
   * Applied *before* the patch, so the patch addresses the new ids.
   *
   * An array rather than one, because there is no reason for the route to be
   * the thing that stops an operator renaming two agents in one save — and
   * because a single field would have had to be widened the first time one did.
   */
  renameAgents: z.array(AgentRenameSchema).optional(),
});
export type SettingsPatchRequest = z.infer<typeof SettingsPatchRequestSchema>;

/** Write-only credential update. */
export const SetCredentialRequestSchema = z.object({
  namespace: z.enum([
    'providers',
    'tools',
    'audio',
    'mcp_servers',
    /** An extension's own credential, keyed by extension id. */
    'extensions',
    /** A channel's own credential — a bot token, keyed by channel id. */
    'channels',
  ]),
  key: z.string().min(1),
  /** `null` deletes the entry. */
  value: z.string().nullable(),
});
export type SetCredentialRequest = z.infer<typeof SetCredentialRequestSchema>;

// Providers and models

/**
 * A provider *type*, projected from the `PROVIDERS` table in
 * `ghostai-providers`. The catalogue an operator adds an endpoint from.
 *
 * It carries no credential flag. A credential belongs to a configured
 * instance — two Ollama entries can have different tokens — so the boolean
 * lives on `ProviderInstanceInfo` and nowhere else.
 */
export const ProviderInfoSchema = z.object({
  id: z.string().min(1),
  displayName: z.string(),
  /** Which wire adapter drives it. */
  wire: z.string(),
  isLocal: z.boolean(),
  isGateway: z.boolean(),
  isOAuth: z.boolean(),
  defaultApiBase: z.string().optional(),
  envKey: z.string().optional(),
  supportsModelListing: z.boolean(),
});
export type ProviderInfo = z.infer<typeof ProviderInfoSchema>;

/**
 * One configured endpoint.
 *
 * `type` names the `ProviderInfo` it was created from; `id` is the operator's
 * key for this particular endpoint, and is what an agent's `provider`
 * names and what the vault stores its credential under.
 */
export const ProviderInstanceInfoSchema = z.object({
  id: z.string().min(1),
  type: z.string().min(1),
  /** Resolved for display: the instance's label, or the type's name. */
  displayName: z.string(),
  /** Effective, not configured — the default is folded in. */
  apiBase: z.string(),
  isLocal: z.boolean(),
  isGateway: z.boolean(),
  isOAuth: z.boolean(),
  envKey: z.string().optional(),
  enabled: z.boolean(),
  supportsModelListing: z.boolean(),
  credentialsPresent: z.boolean(),
});
export type ProviderInstanceInfo = z.infer<typeof ProviderInstanceInfoSchema>;

/**
 * Both lists, because the panel needs both: `types` is what an "Add provider"
 * control offers, `instances` is what the list below it renders.
 */
export const ProvidersResponseSchema = z.object({
  types: z.array(ProviderInfoSchema),
  instances: z.array(ProviderInstanceInfoSchema),
});
export type ProvidersResponse = z.infer<typeof ProvidersResponseSchema>;

export const ModelInfoSchema = z.object({
  id: z.string().min(1),
  /** The provider *instance* this model was offered by. */
  providerId: z.string().min(1),
  /** The instance's type, for grouping and labelling. Absent on a bare list. */
  providerType: z.string().optional(),
  displayName: z.string().optional(),
  contextWindowTokens: z.number().int().positive().optional(),
  supportsTools: z.boolean().optional(),
  supportsVision: z.boolean().optional(),
  supportsReasoning: z.boolean().optional(),
});
export type ModelInfo = z.infer<typeof ModelInfoSchema>;

export const ModelsResponseSchema = z.object({
  models: z.array(ModelInfoSchema),
  /** Instances whose model list could not be fetched, id → reason. */
  errors: z.record(z.string(), z.string()).default({}),
});
export type ModelsResponse = z.infer<typeof ModelsResponseSchema>;

/**
 * "Can this endpoint be talked to?", asked of a connection rather than of a
 * stored instance.
 *
 * A *connection*, because the panel needs the answer before there is anything
 * to store: the Add-provider dialog probes what the operator has typed, and a
 * request that could only name an existing instance would force a save first —
 * which is the thing the check exists to happen before.
 */
export const ProviderTestRequestSchema = z.object({
  /** A `ghostai-providers` registry id. */
  type: z.string().min(1),
  /** Empty means the type's own default endpoint. */
  apiBase: z.string().default(''),
  extraHeaders: z.record(z.string(), z.string()).default({}),
  /**
   * The key to probe *with*. Omitted means "whatever is already stored for
   * `instanceId`" — which is how a saved row re-tests without the client ever
   * having held the credential. It is never echoed back.
   */
  apiKey: z.string().optional(),
  /** The instance being tested, when one exists. Only used to find a key. */
  instanceId: z.string().optional(),
});
export type ProviderTestRequest = z.infer<typeof ProviderTestRequestSchema>;

/**
 * The result of one probe.
 *
 * `reason` is a `ProviderErrorReason`, and it is the field that matters: the
 * difference between `auth` (it answered and rejected the key) and `transport`
 * (nothing is listening) is the difference between two completely different
 * things for an operator to go and fix. A client that had only `message` would
 * be reduced to matching on prose.
 */
export const ProviderTestResponseSchema = z.object({
  ok: z.boolean(),
  /** Model ids the endpoint listed. Empty when `ok` is false. */
  models: z.array(z.string()).default([]),
  /** A `ProviderErrorReason`, or `unsupported` when nothing could be asked. */
  reason: z.string().optional(),
  message: z.string().optional(),
});
export type ProviderTestResponse = z.infer<typeof ProviderTestResponseSchema>;

// Sessions

export const SessionSummarySchema = z.object({
  key: z.string().min(1),
  title: z.string(),
  messageCount: z.number().int().nonnegative(),
  createdAtMs: z.number().int().nonnegative(),
  updatedAtMs: z.number().int().nonnegative(),
  /** Channel that owns it — `web`, `telegram`, `automation`, an extension id. */
  origin: z.string().default('web'),
  /**
   * The workspace this session's tools run in.
   *
   * Set at creation and moved only by `PATCH /api/sessions/:key`; a socket
   * frame naming one is still ignored for a session that already exists. See
   * `SessionRecord.workspaceId`.
   */
  workspaceId: z.string().min(1).default('default'),
  agentId: z.string().optional(),
  totalUsage: UsageSchema.optional(),
});
export type SessionSummary = z.infer<typeof SessionSummarySchema>;

export const SessionListResponseSchema = z.object({
  sessions: z.array(SessionSummarySchema),
  nextCursor: z.string().optional(),
  /**
   * Every session the filter matches, not the length of `sessions`.
   *
   * Required rather than optional, and present on the cursor path too. An
   * optional total is a field every client has to branch on before it can render
   * anything, to save one `COUNT(*)` over an indexed column — and a caller that
   * does not need it can ignore a number far more cheaply than it can handle its
   * absence.
   */
  total: z.number().int().nonnegative(),
});
export type SessionListResponse = z.infer<typeof SessionListResponseSchema>;

export const SessionMessagesResponseSchema = z.object({
  sessionKey: z.string().min(1),
  messages: z.array(StoredMessageSchema),
  nextCursor: z.string().optional(),
  /**
   * The delegations this history contains, by the call that made each.
   *
   * Here rather than on `SessionSummary` because this is the response a
   * transcript is *rebuilt* from, and a run is the one thing in a transcript
   * that these rows cannot describe: a subagent's steps live in the subagent's
   * own session. Carrying the pointer alongside the rows is what lets a
   * reloaded conversation offer the run rather than silently drop it, without a
   * second request to find out whether there is one.
   */
  subagentRuns: z.record(z.string(), SubagentRunRefSchema).default({}),
  /**
   * Why each failed turn failed, by turn id.
   *
   * Beside `subagentRuns` and for the same reason: a fact about the transcript
   * that the message rows cannot hold. A failed turn appends nothing — an error
   * in `messages` would be replayed into every later provider request — so a
   * rebuilt transcript would otherwise show the question, no answer, and no
   * indication that anything went wrong.
   */
  failures: z.record(z.string(), z.string()).default({}),
});
export type SessionMessagesResponse = z.infer<
  typeof SessionMessagesResponseSchema
>;

export const CreateSessionRequestSchema = z.object({
  key: z.string().min(1).optional(),
  title: z.string().optional(),
  /** Which workspace to open the conversation in. Defaults to `default`. */
  workspaceId: z.string().min(1).optional(),
  agentId: z.string().optional(),
});
export type CreateSessionRequest = z.infer<typeof CreateSessionRequestSchema>;

export const UpdateSessionRequestSchema = z.object({
  title: z.string().min(1).optional(),
  agentId: z.string().optional(),
  /**
   * Moves the conversation to another workspace.
   *
   * The only path that moves one. `session.new` carries a workspace too, but
   * it can only ever *create* — a frame naming one is ignored for a session
   * that already exists, so a crafted frame cannot point an open
   * conversation's tools at another workspace's files.
   *
   * The move takes effect from the next turn: a turn already running captured
   * its jail when it started and finishes in the workspace it began in.
   */
  workspaceId: z.string().min(1).optional(),
});
export type UpdateSessionRequest = z.infer<typeof UpdateSessionRequestSchema>;

/**
 * What the agent would actually send to the model, for the context inspector.
 * The panel that makes the token budget legible rather than a mystery.
 */
export const ContextResponseSchema = z.object({
  sessionKey: z.string().min(1),
  /** The cached prefix: the system message, without the per-iteration tail. */
  systemPrompt: z.string(),
  /**
   * The trailing turn the loop appends after the history — live state, the
   * turn's delimiter, a correction.
   *
   * Separate from `systemPrompt` because the two are billed differently: this is
   * the only section re-read at full price on every iteration, and the panel's
   * job is to make that legible. Defaulted so a client reading an older server's
   * response gets an empty section rather than a parse error.
   */
  runtimeBlock: z.string().default(''),
  /**
   * The definitions as the provider would receive them.
   *
   * Carried so the inspector's `tools` row can be opened. The breakdown reported
   * a token cost for a block the client had no copy of, which makes the one
   * follow-up question anyone has — *which* tools, and how big is each schema —
   * unanswerable from the panel that raised it.
   */
  tools: z.array(ToolDefinitionSchema).default([]),
  messages: z.array(StoredMessageSchema),
  estimatedTokens: z.number().int().nonnegative(),
  contextWindowTokens: z.number().int().positive(),
  /** Section name → token cost, so an oversized block is visible. */
  breakdown: z.record(z.string(), z.number()).default({}),
  /**
   * The agent these figures describe — the one a turn would actually run on.
   *
   * Not always the session's binding: an agent can be deleted out from under a
   * conversation, and the panel's whole job is "what would be sent", so it
   * measures what would run rather than what the row still names.
   */
  agentId: z.string().optional(),
  /**
   * Set only when the binding did not resolve, naming what it asked for.
   *
   * Absent is the healthy state, so a client can treat presence alone as "this
   * conversation is running on a fallback" without comparing two strings.
   */
  requestedAgentId: z.string().optional(),
});
export type ContextResponse = z.infer<typeof ContextResponseSchema>;

/**
 * What one turn cost, recorded when it ended.
 *
 * Fetched rather than streamed, because a conversation you did not watch happen
 * has no live events to have carried it — which was the whole reason the info
 * button showed nothing after a reload. The live path still rides on `turn.end`
 * rather than making the client ask for numbers it just watched being measured.
 */
export const TurnStatsSchema = z.object({
  turnId: z.string().min(1),
  sessionKey: z.string().min(1),
  agentId: z.string().default(''),
  /**
   * Which workspace this turn ran in — the files it could actually reach.
   *
   * Deliberately not the session's current workspace: a conversation can be
   * moved between workspaces, so a transcript can span several and only this
   * says which one a given turn saw. Defaulted for turns recorded before it was
   * captured.
   */
  workspaceId: z.string().min(1).default('default'),
  provider: z.string(),
  model: z.string(),
  startedAtMs: z.number().int().nonnegative(),
  endedAtMs: z.number().int().nonnegative(),
  iterations: z.number().int().nonnegative().default(0),
  stopReason: StopReasonSchema,
  usage: UsageSchema,
  /**
   * What the turn spent generating, and how long it waited to start.
   *
   * Both absent on any turn recorded before they were measured, which is what
   * `turnRate`'s fallback to the wall clock exists for. Named here as well as
   * on `turn.end` because this is the only copy a reloaded page can reach —
   * the live event is gone the moment it is delivered.
   */
  generationMs: z.number().int().nonnegative().optional(),
  /** The tokens produced inside `generationMs`, and only those. */
  generationTokens: z.number().int().nonnegative().optional(),
  firstTokenMs: z.number().int().nonnegative().optional(),
  /** Why it stopped, when `stopReason` is `error`. */
  error: z.string().optional(),
});

export const TurnStatsResponseSchema = z.object({
  sessionKey: z.string().min(1),
  turns: z.array(TurnStatsSchema),
});
export type TurnStatsResponse = z.infer<typeof TurnStatsResponseSchema>;

/**
 * Fork a conversation at a point.
 *
 * REST rather than a socket frame, unlike regenerate and edit: this creates a
 * resource and starts no turn, and the caller needs the new key back to
 * navigate to it. The protocol has no request/response correlation anywhere,
 * and should not grow one for a call that maps onto a POST exactly.
 */
export const BranchSessionRequestSchema = z.object({
  /** Copy everything at or below this `seq`. `0` forks an empty conversation. */
  seq: z.number().int().nonnegative(),
  key: z.string().min(1).optional(),
  title: z.string().optional(),
});
export type BranchSessionRequest = z.infer<typeof BranchSessionRequestSchema>;

// Agents

/**
 * One agent, as a picker needs it.
 *
 * Deliberately thin. The full settings tree — system prompts, tool selections,
 * approval overrides — already reaches the client through `GET /api/settings`,
 * and a second, subtly different copy of it here is how the two drift. What
 * this adds is the part settings cannot answer: the model *after* inheritance
 * and after any process-wide pin, which is what a turn would actually use.
 */
export const AgentSummarySchema = z.object({
  id: z.string().min(1),
  /** Never empty: falls back to the id. */
  label: z.string().min(1),
  model: z.string(),
  provider: z.string(),
  /**
   * The effort in force, absent when this agent states none.
   *
   * Absent means the agent sends no reasoning parameter at all, so the provider
   * applies its own. Reported beside `model` so a client has the answer without
   * reading the settings tree — and, like `model`, it is what a turn would send
   * rather than what the file says, which differ under a `--model` pin.
   */
  reasoningEffort: ReasoningEffortSchema.optional(),
});
export type AgentSummary = z.infer<typeof AgentSummarySchema>;

export const AgentListResponseSchema = z.object({
  /** The default agent first, then the operator's own order. */
  agents: z.array(AgentSummarySchema),
});
export type AgentListResponse = z.infer<typeof AgentListResponseSchema>;

// Tools

export const ToolListResponseSchema = z.object({
  tools: z.array(ToolDefinitionSchema),
});
export type ToolListResponse = z.infer<typeof ToolListResponseSchema>;

/**
 * One granted operation, as the agent editor's permission row needs it.
 *
 * Three fields rather than a bare name, because the editor renders a permission
 * row per grant: it needs something to label the row with and the manifest's own
 * ceiling to show beside what the agent chose. Fetching that separately would
 * mean a second request per environment to render one list.
 */
/**
 * One installed environment definition, as the API reports it.
 *
 * **The definition itself, not a projection of it.** This used to restate eight
 * of its fields by hand and drop the rest, which was survivable while the panel
 * only read them and is not now that it writes them too: an editor cannot
 * round-trip a definition it was handed two thirds of, and reassembling one in
 * the browser is how the two descriptions come apart.
 *
 * The three fields beside it are the ones that are *not* in the file. They are
 * derived, resolved server-side so the CLI's review and the browser cannot
 * describe one definition differently:
 *
 *  - `weakened` names the hardening this definition switched off.
 *  - `gatewayProblem` is the sentence a restricted egress request would fail
 *    with, resolved in advance so the editor can warn while a network is still
 *    being chosen rather than on save.
 *  - `problem` is why it cannot be used at all, which is also the case where
 *    `definition` is absent: a file that did not parse still has a name and
 *    still belongs on the list, because deleting it is the operator's way out.
 */
export const EnvironmentSummarySchema = z.object({
  /** Also its filename. */
  name: z.string(),
  definition: EnvironmentDefinitionSchema.optional(),
  weakened: z.array(z.string()),
  gatewayProblem: z.string().optional(),
  problem: z.string().optional(),
});
export type EnvironmentSummary = z.infer<typeof EnvironmentSummarySchema>;

export const EnvironmentListResponseSchema = z.object({
  environments: z.array(EnvironmentSummarySchema),
});
export type EnvironmentListResponse = z.infer<
  typeof EnvironmentListResponseSchema
>;

/** One container the environment service is holding open. */
export const SandboxInstanceSummarySchema = z
  .object({
    id: z.string(),
    workspace: z.string(),
    environment: z.string(),
    busy: z.number().int().nonnegative(),
    lastUsedMs: z.number().int().nonnegative(),
    agents: z.array(z.string()).default([]),
  })
  .strict();
export type SandboxInstanceSummary = z.infer<
  typeof SandboxInstanceSummarySchema
>;

export const SandboxListResponseSchema = z
  .object({
    instances: z.array(SandboxInstanceSummarySchema),
  })
  .strict();
export type SandboxListResponse = z.infer<typeof SandboxListResponseSchema>;

/**
 * Everything the app may ask the sandbox service for.
 *
 * One type for the whole boundary rather than one for the socket and another
 * for `POST /api/sandboxes`: the HTTP route parses this, refuses the variants an
 * operator may not send, and forwards the same value. Two enums meant a field
 * could be added to one and silently dropped re-serialising through the other.
 */
export const SandboxRequestSchema = z.discriminatedUnion('op', [
  z.object({ op: z.literal('health') }).strict(),
  z.object({ op: z.literal('list') }).strict(),
  z
    .object({
      op: z.literal('exec'),
      environment: z.string(),
      workspace: z.string(),
      agent: z.string(),
      session: z.string(),
      argv: z.array(z.string()),
      timeoutMs: z.number().int().nonnegative().default(0),
      maxOutputBytes: z.number().int().nonnegative().default(0),
      network: EnvironmentNetworkSchema.prefault({}),
    })
    .strict(),
  z
    .object({
      op: z.literal('start'),
      environment: z.string(),
      workspace: z.string(),
      agent: z.string(),
      session: z.string(),
      network: EnvironmentNetworkSchema.prefault({}),
    })
    .strict(),
  z
    .object({
      op: z.literal('stop'),
      instance: z.string(),
    })
    .strict(),
  z
    .object({
      op: z.literal('restart'),
      instance: z.string(),
    })
    .strict(),
]);
export type SandboxRequest = z.infer<typeof SandboxRequestSchema>;

// MCP servers

/**
 * Where one configured MCP server is right now.
 *
 * A *live* state, which is why it is here and not in the settings tree: an
 * operator's `tools.mcpServers.<id>` entry says what should be connected, and
 * this says what is. Folding the second into the first would mean writing
 * "unreachable" into `config.yaml`.
 *
 * Declared in `@ghostwire/protocol` rather than in `ghostai-mcp` so that the
 * server and the browser can name it without either of them depending on the
 * client package — the same reason `ToolDefinition` lives here rather than in
 * `ghostai-tools`.
 */
export const McpServerStateSchema = z.enum([
  'connecting',
  'ready',
  'needs_authorization',
  'failed',
  'disabled',
]);
export type McpServerState = z.infer<typeof McpServerStateSchema>;

export const McpServerStatusSchema = z.object({
  id: z.string().min(1),
  /** Resolved, so a config that left it to inference still reports one. */
  transport: McpTransportSchema.optional(),
  state: McpServerStateSchema,
  enabled: z.boolean(),
  /** The flattened names the model sees. Sorted. */
  tools: z.array(z.string()).default([]),
  /** What the server advertises and `enabledTools` filtered out. */
  filteredTools: z.array(z.string()).default([]),
  serverName: z.string().default(''),
  serverVersion: z.string().default(''),
  /**
   * Why it is not connected, phrased for the operator.
   *
   * A field on the row rather than a `ConfigWarning`, because those are
   * properties of the settings tree — true at every moment until someone edits
   * it — and a closed laptop is not. See `ConfigWarningSchema` above.
   */
  lastError: z.string().optional(),
  lastConnectedAtMs: z.number().int().nonnegative().optional(),
  /** Where the operator must go while `state` is `needs_authorization`. */
  authorizationUrl: z.string().optional(),
  /**
   * Problems that did not stop the server working: a tool whose schema could
   * not be advertised, an `enabledTools` entry matching nothing, a name
   * collision that skipped one tool.
   */
  warnings: z.array(z.string()).default([]),
});
export type McpServerStatus = z.infer<typeof McpServerStatusSchema>;

export const McpStatusResponseSchema = z.object({
  servers: z.array(McpServerStatusSchema),
});
export type McpStatusResponse = z.infer<typeof McpStatusResponseSchema>;

// Extensions

/**
 * What an extension is doing right now.
 *
 * Four of the five are reasons it is *not* running, and each is distinct
 * because each has a different fix: approve it, re-approve it, enable it, or
 * repair it. Collapsing them into one `failed` would put the operator back to
 * reading logs, which is the state this row exists to replace.
 */
export const ExtensionStateSchema = z.enum([
  /** Loaded and activated. */
  'ready',
  /** Discovered, never approved. */
  'unapproved',
  /** Approved once; the bytes on disk have changed since. */
  'drifted',
  /** Named in `extensions.disabled`. */
  'disabled',
  /** Approved and enabled, and it threw. */
  'failed',
]);
export type ExtensionState = z.infer<typeof ExtensionStateSchema>;

export const ExtensionStatusSchema = z.object({
  id: z.string().min(1),
  state: ExtensionStateSchema,
  version: z.string().default(''),
  label: z.string().default(''),
  description: z.string().default(''),
  /** What the manifest declares. Empty on an extension that failed to parse. */
  contributes: z.array(ExtensionContributionSchema).default([]),
  /** The tool names it registered, flattened and sorted. */
  tools: z.array(z.string()).default([]),
  channels: z.array(z.string()).default([]),
  providers: z.array(z.string()).default([]),
  commands: z.array(z.string()).default([]),
  /** Present once it has been approved, so the panel can show what it holds. */
  digest: z.string().default(''),
  approvedAtMs: z.number().int().nonnegative().optional(),
  /**
   * Why it is not running, phrased for the operator.
   *
   * A field on the row rather than a `ConfigWarning`, for the reason
   * `McpServerStatus.lastError` gives: a warning is a property of the settings
   * tree and true until someone edits it, and an extension that threw on
   * activation is not.
   */
  lastError: z.string().optional(),
  /**
   * Problems that did not stop it loading: a registration whose id broke the
   * namespace rule, or one whose kind `contributes` never declared.
   */
  warnings: z.array(z.string()).default([]),
});
export type ExtensionStatus = z.infer<typeof ExtensionStatusSchema>;

export const ExtensionListResponseSchema = z.object({
  extensions: z.array(ExtensionStatusSchema),
});
export type ExtensionListResponse = z.infer<typeof ExtensionListResponseSchema>;

/**
 * A slash command an extension contributes.
 *
 * The first command table that is not written out by hand.
 * `packages/web/src/chat/commands.ts` explains why the three built-in ones do
 * not share a core: the surfaces agree on a vocabulary rather than on an
 * implementation. An extension's command is the case where they *have* to share
 * one, because there is exactly one definition of it and more than one place it
 * has to appear — so it is fetched rather than compiled in, and it answers with
 * text rather than a resource key, since its copy ships with the extension and
 * never reaches a locale bundle.
 *
 * **Two surfaces, not three.** The composer and the terminal reach these;
 * Telegram does not. Its commands are `bot_command` entities registered with
 * the Bot API, whose names are `[a-z0-9_]` — a namespaced `slack-post` cannot
 * be spelled there at all, and inventing a second spelling for one command is
 * how a command ends up meaning two things.
 */
export const ExtensionCommandSchema = z.object({
  /** `<extensionId>` or `<extensionId>-<suffix>`, so `/slack-status` is legal. */
  id: z.string().min(1),
  extensionId: z.string().min(1),
  /** The line the autocomplete shows. */
  description: z.string().default(''),
  /** What to write after the name, in prose. Empty means it takes none. */
  argsHint: z.string().default(''),
});
export type ExtensionCommand = z.infer<typeof ExtensionCommandSchema>;

export const CommandListResponseSchema = z.object({
  commands: z.array(ExtensionCommandSchema),
});
export type CommandListResponse = z.infer<typeof CommandListResponseSchema>;

export const RunCommandRequestSchema = z.object({
  /** Everything the operator typed after the command name. */
  args: z.string().default(''),
  /** The conversation it was typed in, when there is one. */
  sessionKey: z.string().optional(),
});
export type RunCommandRequest = z.infer<typeof RunCommandRequestSchema>;

export const RunCommandResponseSchema = z.object({
  /**
   * What to show the operator, verbatim.
   *
   * Not a resource key: an extension's copy ships with the extension, so the
   * translation layer has never seen it. The same rule an environment's `notes`
   * follows.
   */
  message: z.string().default(''),
  /** `false` renders the message as an error rather than a note. */
  ok: z.boolean().default(true),
});
export type RunCommandResponse = z.infer<typeof RunCommandResponseSchema>;

// Files

export const FileEntrySchema = z.object({
  /** Workspace-relative, always. Absolute paths never cross this boundary. */
  path: z.string().min(1),
  name: z.string().min(1),
  isDirectory: z.boolean(),
  sizeBytes: z.number().int().nonnegative(),
  modifiedAtMs: z.number().int().nonnegative(),
  mimeType: z.string().optional(),
});
export type FileEntry = z.infer<typeof FileEntrySchema>;

export const FileListResponseSchema = z.object({
  path: z.string(),
  entries: z.array(FileEntrySchema),
});
export type FileListResponse = z.infer<typeof FileListResponseSchema>;

/**
 * An HMAC-signed, expiring URL.
 *
 * `<img src>` cannot carry an Authorization header. The tempting fix is to make
 * the file endpoint public, which turns it into anonymous read access to
 * everything under the workspace. A short-lived signature satisfies the browser
 * instead, and the endpoint stays authenticated.
 */
export const SignedUrlSchema = z.object({
  url: z.string().min(1),
  expiresAtMs: z.number().int().nonnegative(),
});
export type SignedUrl = z.infer<typeof SignedUrlSchema>;

/**
 * Asking for one.
 *
 * A body rather than a query parameter, and the reason is the audit log: a
 * workspace path in a URL is written to every access log between the browser
 * and the server, and the whole point of the signature is that the *URL* is the
 * thing that travels.
 */
export const SignedUrlRequestSchema = z.object({
  /** Workspace-relative, like every other path that crosses this boundary. */
  path: z.string().min(1),
  /** Which workspace the path is relative to. Defaults to `default`. */
  workspaceId: z.string().min(1).optional(),
});
export type SignedUrlRequest = z.infer<typeof SignedUrlRequestSchema>;

export const UploadResponseSchema = z.object({
  path: z.string().min(1),
  sizeBytes: z.number().int().nonnegative(),
  mimeType: z.string(),
  signedUrl: SignedUrlSchema.optional(),
});
export type UploadResponse = z.infer<typeof UploadResponseSchema>;

/**
 * One text file, as an editor needs it.
 *
 * Distinct from the signed media URL, and not a duplicate of it. A signature
 * exists so a browser *element* — an `<img>` that cannot send a header — can
 * fetch bytes, and `/api/media/:token` therefore answers with the rules a
 * browser needs: `nosniff`, and `attachment` for anything it might execute. An
 * editor needs none of that. It needs the characters in a JSON string, which
 * render in a `<textarea>` and execute nowhere, and it needs `modifiedAtMs` —
 * which a media response does not carry and which is what makes a save
 * conflict detectable.
 */
export const FileTextResponseSchema = z.object({
  path: z.string().min(1),
  content: z.string(),
  /** The file's size on disk. Larger than `content` when `truncated`. */
  sizeBytes: z.number().int().nonnegative(),
  modifiedAtMs: z.number().int().nonnegative(),
  /**
   * The file was longer than the read limit and `content` is a prefix.
   *
   * An editor that saved a prefix would delete the rest of the file, so this is
   * the flag that makes the panel read-only rather than a detail for a footer.
   */
  truncated: z.boolean(),
});
export type FileTextResponse = z.infer<typeof FileTextResponseSchema>;

/**
 * Saving one.
 *
 * `expectedModifiedAtMs` is the reason this is not just a `PUT` of the body.
 * The workspace is a tree a language model writes to while a person is looking
 * at it, so "the agent rewrote the file under the open editor" is an ordinary
 * Tuesday rather than a race worth ignoring. Sending back the timestamp the
 * editor loaded turns that into a 409 the panel can explain, instead of a
 * silent overwrite of a turn's work.
 *
 * Absent means "write it regardless" — which is what creating a new file is.
 *
 * A modification time, not a hash, and it carries that mechanism's one
 * weakness: two writes inside the filesystem's timestamp resolution are
 * indistinguishable. This is the same trade `If-Unmodified-Since` has made for
 * thirty years, and it holds for the case that actually happens — a person
 * editing for seconds while a turn runs — rather than for two writes in the
 * same millisecond.
 */
export const FileWriteRequestSchema = z.object({
  path: z.string().min(1),
  content: z.string(),
  /** Which workspace the path is relative to. Defaults to `default`. */
  workspaceId: z.string().min(1).optional(),
  expectedModifiedAtMs: z.number().int().nonnegative().optional(),
});
export type FileWriteRequest = z.infer<typeof FileWriteRequestSchema>;

export const CreateDirectoryRequestSchema = z.object({
  /** Workspace-relative, like every other path that crosses this boundary. */
  path: z.string().min(1),
  /** Which workspace the path is relative to. Defaults to `default`. */
  workspaceId: z.string().min(1).optional(),
});
export type CreateDirectoryRequest = z.infer<
  typeof CreateDirectoryRequestSchema
>;

/**
 * Moving a file or a directory within one workspace.
 *
 * **Two full paths, not a name.** A rename and a move are the same filesystem
 * operation, and a `{ path, newName }` shape would be a rename that has to grow
 * a second endpoint the first time anybody wants to drag a file into a folder.
 * The UI renames by sending the same parent with a different last segment,
 * which costs it one `joinPath` and keeps this route honest about what it does.
 *
 * Both ends are workspace-relative and both go through the jail, so a `to` that
 * climbs out is refused by the same code that refuses a `from` that does. There
 * is no `workspaceId` per side on purpose: moving *between* workspaces would
 * cross a boundary the jail exists to hold, and the honest way to do it is a
 * read and a write.
 */
export const MoveFileRequestSchema = z.object({
  from: z.string().min(1),
  to: z.string().min(1),
  /** Which workspace both paths are relative to. Defaults to `default`. */
  workspaceId: z.string().min(1).optional(),
});
export type MoveFileRequest = z.infer<typeof MoveFileRequestSchema>;

// Workspaces

/**
 * A workspace as the switcher and the manager see it.
 *
 * **No path field, anywhere in this section.** A workspace is
 * `<root>/workspace/<id>` and the id is the only thing that crosses the wire;
 * accepting a directory would turn "managed directories only" from a fact into
 * a convention, and the first client to send `/` would have handed an
 * authenticated caller the whole filesystem.
 */
export const WorkspaceSummarySchema = z.object({
  id: z.string().min(1),
  name: z.string(),
  /** True for exactly one, which cannot be deleted and contains all the others. */
  isDefault: z.boolean(),
  createdAtMs: z.number().int().nonnegative(),
  updatedAtMs: z.number().int().nonnegative(),
  /** What a delete would have to move first. */
  sessionCount: z.number().int().nonnegative(),
});
export type WorkspaceSummary = z.infer<typeof WorkspaceSummarySchema>;

export const WorkspaceListResponseSchema = z.object({
  workspaces: z.array(WorkspaceSummarySchema),
});
export type WorkspaceListResponse = z.infer<typeof WorkspaceListResponseSchema>;

export const CreateWorkspaceRequestSchema = z.object({
  name: z.string().min(1).max(60),
  /** Derived from the name when absent. Lowercase; also the folder name. */
  id: z.string().min(1).max(40).optional(),
});
export type CreateWorkspaceRequest = z.infer<
  typeof CreateWorkspaceRequestSchema
>;

/**
 * What an edit may change: the label, the folder, or both.
 *
 * `id` is the directory name, so sending it is a `rename(2)` under a tree
 * somebody may be working in — refused for the default workspace, whose folder
 * *is* the workspace root and the parent of every other one. Both fields are
 * optional and a body with neither is a no-op, which is what lets the editor
 * send one PATCH for whichever boxes were touched.
 */
export const UpdateWorkspaceRequestSchema = z.object({
  name: z.string().min(1).max(60).optional(),
  /** The folder to move it to. Lowercase; see `WORKSPACE_ID_PATTERN`. */
  id: z.string().min(1).max(40).optional(),
});
export type UpdateWorkspaceRequest = z.infer<
  typeof UpdateWorkspaceRequestSchema
>;

/** The way through a delete that was refused for having sessions. */
export const MoveSessionsRequestSchema = z.object({
  to: z.string().min(1),
});
export type MoveSessionsRequest = z.infer<typeof MoveSessionsRequestSchema>;

export const MoveSessionsResponseSchema = z.object({
  moved: z.number().int().nonnegative(),
});
export type MoveSessionsResponse = z.infer<typeof MoveSessionsResponseSchema>;

// Notifications

export const NotificationSchema = z.object({
  id: z.string().min(1),
  title: z.string(),
  body: z.string(),
  level: z.enum(['info', 'success', 'warning', 'error']).default('info'),
  createdAtMs: z.number().int().nonnegative(),
  readAtMs: z.number().int().nonnegative().optional(),
  sessionKey: z.string().optional(),
  jobId: z.string().optional(),
});
export type Notification = z.infer<typeof NotificationSchema>;

export const NotificationListResponseSchema = z.object({
  notifications: z.array(NotificationSchema),
  unreadCount: z.number().int().nonnegative(),
  nextCursor: z.string().optional(),
  /**
   * Every notification the filter matches — which is *not* `unreadCount` unless
   * `unread=true` was asked for. The bell wants the unread tally; the pager
   * under the list wants how many rows it is paging. Two numbers because they
   * are two questions, and conflating them is how a list of 200 read
   * notifications reported that it had none.
   */
  total: z.number().int().nonnegative(),
});
export type NotificationListResponse = z.infer<
  typeof NotificationListResponseSchema
>;

// Automation

export const AutomationJobListResponseSchema = z.object({
  jobs: z.array(AutomationJobSchema),
});
export type AutomationJobListResponse = z.infer<
  typeof AutomationJobListResponseSchema
>;

export const AutomationRunListResponseSchema = z.object({
  runs: z.array(AutomationRunSchema),
  nextCursor: z.string().optional(),
  /** Every run the job has kept, bounded by its retention knob. See `total` on `SessionListResponse`. */
  total: z.number().int().nonnegative(),
});
export type AutomationRunListResponse = z.infer<
  typeof AutomationRunListResponseSchema
>;

// Auth

/**
 * The login name an install starts with.
 *
 * A default rather than a required choice, because the first credential a fresh
 * install needs is a *password* — asking for a username in the same breath adds
 * a second thing to invent at the one moment the operator has least context. It
 * is exported so the sign-in form can prefill it and the CLI can name it in
 * help text; changing it is done from the same form that changes the password.
 */
export const DEFAULT_USERNAME = 'ghost';

/** Bounds on the login name. */
const USERNAME_MIN_LENGTH = 1;
const USERNAME_MAX_LENGTH = 64;

/**
 * Bounds on the password.
 *
 * Twelve rather than the eight a login form usually settles for, because what
 * sits behind this one is not an account on a website: it is an agent that can
 * read files and run commands on the host. The upper bound is not a strength
 * ceiling but a work ceiling — argon2id will happily chew through a megabyte of
 * input, and an unauthenticated caller must not be able to ask it to.
 */
export const PASSWORD_MIN_LENGTH = 12;
export const PASSWORD_MAX_LENGTH = 256;

/**
 * A login name, as it is compared.
 *
 * Trimmed and lower-cased by the schema rather than by each caller, so the
 * value that reaches storage is the value that reaches a comparison. A name
 * that matched on the way in and failed on the way back — because one path
 * folded case and the other did not — is a lockout with no error message.
 *
 * The character class is narrow on purpose. This is a single local account, not
 * a directory, and every character it does not accept is one that cannot turn
 * up in a log line, a shell completion or a URL as something other than itself.
 */
export const UsernameSchema = z
  .string()
  .trim()
  .toLowerCase()
  .min(USERNAME_MIN_LENGTH)
  .max(USERNAME_MAX_LENGTH)
  .regex(
    /^[a-z0-9][a-z0-9._-]*$/,
    'Use letters, digits, dots, dashes and underscores, starting with a letter or digit.',
  );

/**
 * A new password, as it is accepted.
 *
 * Deliberately not trimmed. A leading or trailing space is a character the
 * person chose, and silently removing it here would mean storing a digest of
 * something they never typed — after which the password manager that replays it
 * verbatim can never sign in.
 */
export const NewPasswordSchema = z
  .string()
  .min(PASSWORD_MIN_LENGTH)
  .max(PASSWORD_MAX_LENGTH);

/**
 * A password being *presented*, which is a different schema from one being set.
 *
 * The bounds a new password must clear are a policy, and applying a policy to an
 * attempt would turn the login into an oracle: a 422 for "too short" and a 401
 * for "wrong" tell an attacker which guesses are not worth making. Only the
 * upper bound survives, and only because it caps the work an anonymous caller
 * can ask argon2id to do.
 */
export const PresentedPasswordSchema = z
  .string()
  .min(1)
  .max(PASSWORD_MAX_LENGTH);

export const LoginRequestSchema = z.object({
  username: UsernameSchema,
  password: PresentedPasswordSchema,
});
export type LoginRequest = z.infer<typeof LoginRequestSchema>;

/**
 * No token in the body. Browsers get an `httpOnly; Secure; SameSite=Strict`
 * cookie set by the response: a token readable from JavaScript is
 * XSS-exfiltratable, and this app's whole job is rendering model-authored
 * markdown. CLI and CI use a `Bearer` token minted out of band.
 */
export const LoginResponseSchema = z.object({
  ok: z.literal(true),
  expiresAtMs: z.number().int().nonnegative(),
});
export type LoginResponse = z.infer<typeof LoginResponseSchema>;

/**
 * Who the caller is, as far as the server is concerned.
 *
 * `authEnabled` is here so the UI has one request to make before deciding
 * whether to render the login overlay. With auth off every caller is
 * authenticated and there is no session behind it, which is why `expiresAtMs`
 * is optional rather than a sentinel.
 */
export const AuthSessionResponseSchema = z.object({
  authenticated: z.boolean(),
  authEnabled: z.boolean(),
  expiresAtMs: z.number().int().nonnegative().optional(),
  /**
   * Who the caller is signed in as.
   *
   * Only on an authenticated response, and that is the whole reason it is not
   * on `SetupStatusResponse` instead: the sign-in form would like to prefill it,
   * but a public route that answered "the account here is called `admin`" would
   * be handing out half of the credential to anyone who asked. The form prefills
   * `DEFAULT_USERNAME` and is wrong only on installs that changed it.
   */
  username: z.string().optional(),
});
export type AuthSessionResponse = z.infer<typeof AuthSessionResponseSchema>;

// First-run setup

/**
 * Whether this install still has to be claimed.
 *
 * Public, and deliberately says nothing else. An unauthenticated caller learns
 * one bit — that no password has been set — which they would learn anyway by
 * watching every login fail. Anything more (the workspace, the provider list,
 * whether a code is outstanding) would be describing an unclaimed agent to
 * whoever asked first.
 */
export const SetupStatusResponseSchema = z.object({
  required: z.boolean(),
});
export type SetupStatusResponse = z.infer<typeof SetupStatusResponseSchema>;

/**
 * The one-time code printed to the console on first launch.
 *
 * It exists because both alternatives are worse: refusing to start without a
 * password leaves the UI that would set one unreachable, and starting
 * unauthenticated is a shell-capable agent answering to whoever reaches the
 * port first. A code that only the operator's own terminal can see closes that
 * gap without either.
 */
export const SetupClaimRequestSchema = z.object({
  code: z.string().min(1),
});
export type SetupClaimRequest = z.infer<typeof SetupClaimRequestSchema>;

/**
 * Setting the password, and — the same request, later in an install's life —
 * changing it.
 *
 * One route for both because they are one operation with one precondition that
 * differs: a claim has no current password to prove, and a rotation does.
 * Splitting them would mean two handlers, two rate limits and two chances for
 * the one that skips the proof to be reachable when it should not be.
 *
 * `currentPassword` is optional *in the schema* and mandatory *in the handler*
 * whenever a password already exists. Encoding that here would need a
 * cross-field refinement, which cannot be represented in the generated OpenAPI
 * document — and a document that describes the field as always-optional is
 * closer to the truth than one that describes it as always-required.
 */
export const SetupPasswordRequestSchema = z.object({
  password: NewPasswordSchema,
  /**
   * Proof that the caller knows the password they are replacing.
   *
   * A session alone is not enough for a rotation. The session cookie is
   * `httpOnly`, but this application renders markdown a language model wrote,
   * and the failure mode being closed here is an injection that changes the
   * password and locks the operator out of their own agent. Knowing the old one
   * is the thing a stolen session does not confer.
   */
  currentPassword: PresentedPasswordSchema.optional(),
  /** Absent leaves the login name alone. */
  username: UsernameSchema.optional(),
});
export type SetupPasswordRequest = z.infer<typeof SetupPasswordRequestSchema>;
