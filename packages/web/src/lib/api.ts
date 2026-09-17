/**
 * The REST client.
 *
 * One `request` function, one error type, and a Zod parse on the way out. The
 * parse is not ceremony: `@darkwire/protocol` is the same module the server
 * builds its responses from, so a field the server stopped sending is a failing
 * request here rather than `undefined` reaching a component three renders
 * later. It costs one schema lookup per response and it is the only reason the
 * types below can be trusted.
 *
 * Two things are deliberate:
 *
 *  - **`credentials: 'same-origin'` and no CSRF header.** The session cookie is
 *    `SameSite=Strict`, which is what stands in for a token — see
 *    `packages/server/src/auth.ts`. A header would be a second mechanism
 *    protecting nothing.
 *  - **401 is a value, not a crash.** It is the normal state of a browser that
 *    has not logged in yet, and `ApiError.status` is what the login overlay
 *    reads to decide whether to show itself.
 */

import {
  ResolveImageResponseSchema,
  SandboxListResponseSchema,
  type ResolveImageResponse,
  type SandboxListResponse,
  type SandboxRequest,
  AutomationJobListResponseSchema,
  AutomationJobSchema,
  AutomationRunListResponseSchema,
  AutomationRunSchema,
  AuthSessionResponseSchema,
  CommandListResponseSchema,
  ContextResponseSchema,
  ErrorResponseSchema,
  ExtensionListResponseSchema,
  FileEntrySchema,
  FileListResponseSchema,
  FileTextResponseSchema,
  LoginResponseSchema,
  McpStatusResponseSchema,
  ModelsResponseSchema,
  NotificationListResponseSchema,
  NotificationSchema,
  ProviderTestResponseSchema,
  ProvidersResponseSchema,
  RunCommandResponseSchema,
  SessionListResponseSchema,
  SessionMessagesResponseSchema,
  SessionSummarySchema,
  TurnStatsResponseSchema,
  SettingsResponseSchema,
  SetupStatusResponseSchema,
  MoveSessionsResponseSchema,
  SignedUrlSchema,
  WorkspaceListResponseSchema,
  WorkspaceSummarySchema,
  StatusResponseSchema,
  AgentListResponseSchema,
  EnvironmentListResponseSchema,
  ToolListResponseSchema,
  UploadResponseSchema,
  type AuthSessionResponse,
  type AutomationJob,
  type CommandListResponse,
  type ExtensionListResponse,
  type RunCommandRequest,
  type RunCommandResponse,
  type AutomationJobListResponse,
  type AutomationRun,
  type AutomationRunListResponse,
  type CreateAutomationJob,
  type UpdateAutomationJob,
  type SettingsPatchRequest,
  type ContextResponse,
  type FileEntry,
  type FileListResponse,
  type FileTextResponse,
  type LoginResponse,
  type McpStatusResponse,
  type ModelsResponse,
  type Notification,
  type NotificationListResponse,
  type ProviderTestRequest,
  type ProviderTestResponse,
  type ProvidersResponse,
  type SessionListResponse,
  type SessionMessagesResponse,
  type SessionSummary,
  type TurnStatsResponse,
  type SetCredentialRequest,
  type SettingsResponse,
  type SetupStatusResponse,
  type MoveSessionsResponse,
  type SignedUrl,
  type WorkspaceListResponse,
  type WorkspaceSummary,
  type StatusResponse,
  type AgentListResponse,
  type EnvironmentDefinition,
  type EnvironmentListResponse,
  type ToolListResponse,
  type UploadResponse,
} from '@darkwire/protocol';
import { z } from 'zod';

/** A non-2xx response, or a body that did not match its schema. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly details: Readonly<Record<string, unknown>> | undefined;

  constructor(
    status: number,
    code: string,
    message: string,
    details?: Record<string, unknown>,
  ) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.details = details;
  }

  /** The one branch every caller writes: is this "log in again" or "it broke"? */
  get isUnauthenticated(): boolean {
    return this.status === 401;
  }
}

export interface RequestOptions {
  readonly method?: 'GET' | 'POST' | 'PATCH' | 'PUT' | 'DELETE';
  readonly body?: unknown;
  readonly signal?: AbortSignal;
  readonly query?: Readonly<
    Record<string, string | number | boolean | undefined>
  >;
}

/**
 * One request, parsed against `schema`.
 *
 * Pass `undefined` for a 204 or a response whose body nothing reads — the
 * result is `undefined` rather than a parse of an empty string.
 */
export async function request<T>(
  path: string,
  schema: z.ZodType<T>,
  options: RequestOptions = {},
): Promise<T> {
  const response = await send(path, options);
  const body: unknown = await readJson(response);

  if (!response.ok) throw toApiError(response.status, body);

  const parsed = schema.safeParse(body);
  if (!parsed.success) {
    // A shape mismatch is a server the client cannot trust, so it is an error
    // rather than a cast. The issues go in `details` for the console.
    throw new ApiError(
      response.status,
      'invalid_response',
      `Unexpected response from ${path}`,
      {
        issues: parsed.error.issues,
      },
    );
  }

  return parsed.data;
}

/** A request whose response body is not read — a 204, or a fire-and-forget POST. */
export async function requestVoid(
  path: string,
  options: RequestOptions = {},
): Promise<void> {
  const response = await send(path, options);
  if (response.ok) return;

  throw toApiError(response.status, await readJson(response));
}

/**
 * The endpoints something in this package actually calls.
 *
 * Still not a client for every route: a wrapper written before its caller is a
 * wrapper written to the wrong shape, and an untested one, since nothing
 * exercises it. Everything here has a caller.
 *
 * One rule holds across the whole object and is the reason `setCredential`
 * returns `void`: **no response body ever carries a credential.** The vault is
 * write-only over HTTP, so a key goes in through `PUT /api/settings/credentials`
 * and the only thing that comes back out anywhere is the per-provider boolean
 * in `SettingsResponse.credentialsPresent`. A client method that returned what
 * it stored would be a read path for a store that has none.
 */
export const api = {
  me: (signal?: AbortSignal): Promise<AuthSessionResponse> =>
    request('/api/auth/me', AuthSessionResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  login: (username: string, password: string): Promise<LoginResponse> =>
    request('/api/auth/login', LoginResponseSchema, {
      method: 'POST',
      body: { username, password },
    }),

  /** Public, and the one request the app makes before it knows anything else. */
  setupStatus: (signal?: AbortSignal): Promise<SetupStatusResponse> =>
    request('/api/setup', SetupStatusResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  /** Spends the one-time code printed to the console on a first launch. */
  claimSetup: (code: string): Promise<LoginResponse> =>
    request('/api/setup/claim', LoginResponseSchema, {
      method: 'POST',
      body: { code },
    }),

  /**
   * Sets the password and re-issues the session the server just revoked.
   *
   * One method for the wizard's password step and the Account panel's change
   * form, because it is one route: `currentPassword` is what the server demands
   * once a password exists, and the wizard has none to send.
   */
  setSetupPassword: (body: {
    readonly password: string;
    readonly currentPassword?: string;
    readonly username?: string;
  }): Promise<LoginResponse> =>
    request('/api/setup/password', LoginResponseSchema, {
      method: 'POST',
      body,
    }),

  status: (signal?: AbortSignal): Promise<StatusResponse> =>
    request('/api/status', StatusResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  /**
   * A page of conversations.
   *
   * Two readers, and they ask differently. The sidebar wants the most recent
   * few and passes little more than a workspace; the management screen searches
   * and jumps to a page, so it sends `q`, `sort` and `offset`. Never a `cursor`
   * *and* an `offset` — the route answers 400, deliberately, rather than
   * silently honouring one. See `cursor.ts` on the server.
   */
  sessions: (
    options: {
      readonly workspaceId?: string;
      readonly limit?: number;
      readonly offset?: number;
      /**
       * One origin to leave out — the sidebar's, and it sends `subagent`.
       *
       * Asked of the server rather than filtered out of the answer, so a
       * `limit` of thirty is thirty conversations however many delegations ran
       * between them.
       */
      readonly excludeOrigin?: string;
      /** A title substring. Blank is the same as absent. */
      readonly q?: string;
      readonly sort?: 'updated' | 'created' | 'title';
      readonly desc?: boolean;
      readonly signal?: AbortSignal;
    } = {},
  ): Promise<SessionListResponse> =>
    request('/api/sessions', SessionListResponseSchema, {
      // `undefined` entries are dropped by `request`, so these are named plainly
      // rather than spread-guarded one at a time.
      query: {
        workspace: options.workspaceId,
        excludeOrigin: options.excludeOrigin,
        limit: options.limit,
        offset: options.offset,
        q: options.q,
        sort: options.sort,
        // The route takes the two words rather than a bare boolean, so the
        // generated document lists what a client may send.
        desc: options.desc === undefined ? undefined : String(options.desc),
      },
      ...(options.signal ? { signal: options.signal } : {}),
    }),

  messages: (
    key: string,
    signal?: AbortSignal,
  ): Promise<SessionMessagesResponse> =>
    request(
      `/api/sessions/${encodeURIComponent(key)}/messages`,
      SessionMessagesResponseSchema,
      {
        ...(signal ? { signal } : {}),
      },
    ),

  /**
   * Drops a conversation's transcript, keeping the conversation.
   *
   * Nothing has to be done with the answer: the server announces the clear as a
   * `session.reset` frame, so every attached tab — including the one that asked
   * — empties from the socket rather than from this promise.
   */
  clearMessages: (key: string): Promise<void> =>
    requestVoid(`/api/sessions/${encodeURIComponent(key)}/messages`, {
      method: 'DELETE',
    }),

  renameSession: (key: string, title: string): Promise<SessionSummary> =>
    request(`/api/sessions/${encodeURIComponent(key)}`, SessionSummarySchema, {
      method: 'PATCH',
      body: { title },
    }),

  /** One conversation, or a 404 for a key nothing has been said in yet. */
  session: (key: string, signal?: AbortSignal): Promise<SessionSummary> =>
    request(`/api/sessions/${encodeURIComponent(key)}`, SessionSummarySchema, {
      ...(signal ? { signal } : {}),
    }),

  /**
   * Moves a conversation to another agent.
   *
   * The binding lives on the session row, so this is the only way to change it
   * — a frame naming an agent is ignored for a session that already exists,
   * deliberately, so a history cannot drift onto another agent's prompt and
   * tools by accident.
   */
  moveSessionToAgent: (key: string, agentId: string): Promise<SessionSummary> =>
    request(`/api/sessions/${encodeURIComponent(key)}`, SessionSummarySchema, {
      method: 'PATCH',
      body: { agentId },
    }),

  /**
   * Moves a conversation to another workspace.
   *
   * The same shape as the agent move above, and the only way to change it for
   * the same reason: a frame naming a workspace is ignored for a session that
   * already exists. The move lands on the next turn — one already running
   * captured its jail when it started and finishes where it began.
   */
  moveSessionToWorkspace: (
    key: string,
    workspaceId: string,
  ): Promise<SessionSummary> =>
    request(`/api/sessions/${encodeURIComponent(key)}`, SessionSummarySchema, {
      method: 'PATCH',
      body: { workspaceId },
    }),

  deleteSession: (key: string): Promise<void> =>
    requestVoid(`/api/sessions/${encodeURIComponent(key)}`, {
      method: 'DELETE',
    }),

  /** Forks the conversation at `seq` into a new one, which the caller opens. */
  branchSession: (
    key: string,
    seq: number,
    title?: string,
  ): Promise<SessionSummary> =>
    request(
      `/api/sessions/${encodeURIComponent(key)}/branch`,
      SessionSummarySchema,
      {
        method: 'POST',
        body: { seq, ...(title === undefined ? {} : { title }) },
      },
    ),

  /**
   * What each turn in a conversation cost.
   *
   * For turns nobody watched happen: a live turn carries its own numbers on
   * `turn.end`, and asking the server for figures the client just measured
   * would be a round trip to learn what it already knows.
   */
  turns: (key: string, signal?: AbortSignal): Promise<TurnStatsResponse> =>
    request(
      `/api/sessions/${encodeURIComponent(key)}/turns`,
      TurnStatsResponseSchema,
      {
        ...(signal ? { signal } : {}),
      },
    ),

  notifications: (
    options: {
      readonly limit?: number;
      readonly offset?: number;
      readonly signal?: AbortSignal;
    } = {},
  ): Promise<NotificationListResponse> =>
    request('/api/notifications', NotificationListResponseSchema, {
      // `undefined` entries are dropped by `request`. The bell asks for neither
      // and takes the server's default page; the full list pages.
      query: { limit: options.limit, offset: options.offset },
      ...(options.signal ? { signal: options.signal } : {}),
    }),

  /** The updated row rather than a 204, so one item reconciles without a refetch. */
  readNotification: (id: string): Promise<Notification> =>
    request(
      `/api/notifications/${encodeURIComponent(id)}/read`,
      NotificationSchema,
      {
        method: 'POST',
      },
    ),

  readAllNotifications: (): Promise<void> =>
    requestVoid('/api/notifications/read', { method: 'POST' }),

  deleteNotification: (id: string): Promise<void> =>
    requestVoid(`/api/notifications/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    }),

  /** Read and unread alike — the confirmation in front of it is the UI's job. */
  deleteAllNotifications: (): Promise<void> =>
    requestVoid('/api/notifications', { method: 'DELETE' }),

  automationJobs: (signal?: AbortSignal): Promise<AutomationJobListResponse> =>
    request('/api/automation/jobs', AutomationJobListResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  automationJob: (id: string, signal?: AbortSignal): Promise<AutomationJob> =>
    request(
      `/api/automation/jobs/${encodeURIComponent(id)}`,
      AutomationJobSchema,
      {
        ...(signal ? { signal } : {}),
      },
    ),

  createAutomationJob: (body: CreateAutomationJob): Promise<AutomationJob> =>
    request('/api/automation/jobs', AutomationJobSchema, {
      method: 'POST',
      body,
    }),

  updateAutomationJob: (
    id: string,
    body: UpdateAutomationJob,
  ): Promise<AutomationJob> =>
    request(
      `/api/automation/jobs/${encodeURIComponent(id)}`,
      AutomationJobSchema,
      {
        method: 'PATCH',
        body,
      },
    ),

  deleteAutomationJob: (id: string): Promise<void> =>
    requestVoid(`/api/automation/jobs/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    }),

  /**
   * Starts a run and returns the `pending` row.
   *
   * 202, not 200: the answer is minutes away. The caller refreshes the run list
   * rather than holding this open — which is also what the `notification` frame
   * arriving on the socket tells it to do.
   */
  runAutomationJob: (id: string): Promise<AutomationRun> =>
    // `AutomationRunSchema`, not `AutomationJobSchema`. The route answers with
    // the run it just started — a different shape entirely, sharing only `id` —
    // so every press failed `safeParse` and surfaced as "Could not start the
    // run" while the run itself started and finished perfectly well. A response
    // schema that names the wrong type is worse than none: it turns a working
    // endpoint into an error the operator has no way to act on.
    request(
      `/api/automation/jobs/${encodeURIComponent(id)}/run`,
      AutomationRunSchema,
      {
        method: 'POST',
      },
    ),

  automationRuns: (
    id: string,
    options: {
      readonly limit?: number;
      /** A numbered pager's position. Never sent alongside `cursor` — the route answers 400. */
      readonly offset?: number;
      readonly cursor?: string;
      readonly signal?: AbortSignal;
    } = {},
  ): Promise<AutomationRunListResponse> =>
    request(
      `/api/automation/jobs/${encodeURIComponent(id)}/runs`,
      AutomationRunListResponseSchema,
      {
        // `undefined` entries are dropped by `request`, so the three are named
        // plainly rather than spread-guarded one at a time.
        query: {
          limit: options.limit,
          offset: options.offset,
          cursor: options.cursor,
        },
        ...(options.signal ? { signal: options.signal } : {}),
      },
    ),

  tools: (signal?: AbortSignal): Promise<ToolListResponse> =>
    request('/api/tools', ToolListResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  sandboxes: (signal?: AbortSignal): Promise<SandboxListResponse> =>
    request('/api/sandboxes', SandboxListResponseSchema, {
      ...(signal ? { signal } : {}),
    }),
  /**
   * Start, stop or restart one instance.
   *
   * Guarded `exec` belongs to the agent loop and is refused by this route.
   */
  manageSandbox: (
    operation: Exclude<SandboxRequest, { op: 'exec' } | { op: 'list' }>,
  ): Promise<Record<string, unknown>> =>
    request('/api/sandboxes', z.record(z.string(), z.unknown()), {
      method: 'POST',
      body: operation,
    }),

  /**
   * Turn an image reference into the digest a definition may pin.
   *
   * Its own method rather than a `manageSandbox` call, because it is the one
   * operation here that may take minutes: the engine pulls when it does not
   * already hold the image. No timeout is set on purpose: an operator pressed a
   * button and is watching, and giving up on a slow link would send them back to
   * a terminal, which is the thing this removes.
   */
  resolveImage: (reference: string): Promise<ResolveImageResponse> =>
    request('/api/sandboxes', ResolveImageResponseSchema, {
      method: 'POST',
      body: { op: 'resolveImage', reference },
    }),

  /** The environment definitions an operator installed. */
  environments: (signal?: AbortSignal): Promise<EnvironmentListResponse> =>
    request('/api/environments', EnvironmentListResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  /**
   * Installs or replaces one definition, and answers with the whole list.
   *
   * The list rather than the one row, so a save never leaves the panel deciding
   * whether to refetch: the derived facts beside a definition — what hardening
   * it weakens, whether a restricted allow-list could be enforced — are resolved
   * on the server and move when the definition does.
   */
  saveEnvironment: (
    definition: EnvironmentDefinition,
  ): Promise<EnvironmentListResponse> =>
    request(
      `/api/environments/${encodeURIComponent(definition.name)}`,
      EnvironmentListResponseSchema,
      { method: 'PUT', body: definition },
    ),

  removeEnvironment: (name: string): Promise<EnvironmentListResponse> =>
    request(
      `/api/environments/${encodeURIComponent(name)}`,
      EnvironmentListResponseSchema,
      { method: 'DELETE' },
    ),

  /**
   * Where each configured MCP server actually is.
   *
   * Separate from `settings()` for the reason `agents()` is: the settings tree
   * says what an operator asked for, and this says what came of it. A server
   * that is unreachable is a live state that changes without the config
   * changing, and `config.yaml` is not where that belongs.
   */
  mcpServers: (signal?: AbortSignal): Promise<McpStatusResponse> =>
    request('/api/mcp', McpStatusResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  /**
   * Every installed extension and the state it is in.
   *
   * Separate from `settings()` for the reason `mcpServers()` is: the settings
   * tree says which extensions an operator disabled, and this says what
   * happened when the install tried to load them.
   */
  extensions: (signal?: AbortSignal): Promise<ExtensionListResponse> =>
    request('/api/extensions', ExtensionListResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  /**
   * Approving records the digest of the files on disk *now*.
   *
   * A `POST` rather than a settings patch, and not idempotent across an edit to
   * those files — approving twice either side of one approves two different
   * things. See `packages/server/src/routes/extensions.ts`.
   */
  approveExtension: (id: string): Promise<ExtensionListResponse> =>
    request(
      `/api/extensions/${encodeURIComponent(id)}/approve`,
      ExtensionListResponseSchema,
      { method: 'POST' },
    ),

  revokeExtension: (id: string): Promise<ExtensionListResponse> =>
    request(
      `/api/extensions/${encodeURIComponent(id)}/revoke`,
      ExtensionListResponseSchema,
      { method: 'POST' },
    ),

  /** The slash commands extensions contribute, for the composer's `/` list. */
  commands: (signal?: AbortSignal): Promise<CommandListResponse> =>
    request('/api/commands', CommandListResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  /**
   * Runs one, and answers with the extension's own words.
   *
   * Not a resource key: an extension's copy ships with the extension and the
   * translation layer has never seen it. The same rule an environment's `notes`
   * follows.
   */
  runCommand: (
    id: string,
    body: RunCommandRequest,
  ): Promise<RunCommandResponse> =>
    request(
      `/api/commands/${encodeURIComponent(id)}`,
      RunCommandResponseSchema,
      { method: 'POST', body },
    ),

  /**
   * The agents a turn can run on, resolved.
   *
   * Separate from `settings()` even though the settings tree already carries
   * `agents.list`: this reports the model each agent would *actually* use,
   * after inheritance and after any process-wide pin, which the raw config
   * cannot answer — most agents inherit their model and store it as empty.
   */
  agents: (signal?: AbortSignal): Promise<AgentListResponse> =>
    request('/api/agents', AgentListResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  settings: (signal?: AbortSignal): Promise<SettingsResponse> =>
    request('/api/settings', SettingsResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  /**
   * A deep-partial patch, never the whole tree.
   *
   * `ConfigPatch` is what makes saving one panel leave the others alone: it is
   * built by `patchOf()` with every default stripped, so a field this request
   * does not mention is a field the server does not touch.
   */
  patchSettings: (patch: SettingsPatchRequest): Promise<SettingsResponse> =>
    request('/api/settings', SettingsResponseSchema, {
      method: 'PATCH',
      body: patch,
    }),

  /**
   * Makes the server re-read `config.yaml` and rebuild what depends on it.
   *
   * Not a restart: the process, the socket and any turn already running all
   * survive. It is for the changes a running server cannot see — a config
   * edited by hand, an extension dropped in beside it — and it answers with the
   * settings it is now serving, so a caller can tell a reload that changed
   * something from one that changed nothing.
   */
  reloadSettings: (): Promise<SettingsResponse> =>
    request('/api/settings/reload', SettingsResponseSchema, { method: 'POST' }),

  /** Write-only. `value: null` clears the entry; nothing reads one back. */
  setCredential: (body: SetCredentialRequest): Promise<void> =>
    requestVoid('/api/settings/credentials', { method: 'PUT', body }),

  providers: (signal?: AbortSignal): Promise<ProvidersResponse> =>
    request('/api/providers', ProvidersResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  /**
   * Asks one connection whether it answers. Writes nothing.
   *
   * The body carries a connection rather than an instance id so the
   * Add-provider dialog can check what has been typed before it is saved —
   * which is the moment the answer is most use. A key sent here goes the same
   * way a key sent to the vault does: out, and never back.
   */
  testProvider: (body: ProviderTestRequest): Promise<ProviderTestResponse> =>
    request('/api/providers/test', ProviderTestResponseSchema, {
      method: 'POST',
      body,
    }),

  models: (signal?: AbortSignal): Promise<ModelsResponse> =>
    request('/api/models', ModelsResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  /**
   * Re-asks every configured endpoint, past both caches.
   *
   * A POST because it has an effect on the server, and separate from `models`
   * so a render cannot trigger it: a GET that always reached out would turn a
   * re-render loop into a flood of requests at somebody's local model server.
   */
  refreshModels: (): Promise<ModelsResponse> =>
    request('/api/models/refresh', ModelsResponseSchema, { method: 'POST' }),

  /** What a turn on this session would actually send, for the context inspector. */
  context: (key: string, signal?: AbortSignal): Promise<ContextResponse> =>
    request(
      `/api/sessions/${encodeURIComponent(key)}/context`,
      ContextResponseSchema,
      {
        ...(signal ? { signal } : {}),
      },
    ),

  files: (
    workspace: string,
    path: string,
    signal?: AbortSignal,
  ): Promise<FileListResponse> =>
    request('/api/files', FileListResponseSchema, {
      query: { path, workspace },
      ...(signal ? { signal } : {}),
    }),

  /**
   * Deletes a file, or a directory.
   *
   * `recursive` is only sent when it is true, and it is what lets a directory
   * take its contents with it. Without it the server removes an empty directory
   * and refuses a full one — so emptying a tree is never something a request
   * happens to do.
   */
  deleteFile: (
    workspace: string,
    path: string,
    recursive = false,
  ): Promise<void> =>
    requestVoid('/api/files', {
      method: 'DELETE',
      query: { path, workspace, ...(recursive ? { recursive: 'true' } : {}) },
    }),

  /**
   * Writes a file into the workspace and returns a URL an `<img>` can load.
   *
   * The body is the raw bytes rather than a `multipart/form-data` envelope,
   * because that is what the route reads: a browser already has the `File`, and
   * a base64 or multipart wrapper would inflate every upload to describe what
   * `Content-Type` already says.
   */
  upload: (
    workspace: string,
    path: string,
    file: Blob,
    signal?: AbortSignal,
  ): Promise<UploadResponse> =>
    request('/api/files/upload', UploadResponseSchema, {
      method: 'POST',
      query: { path, workspace },
      body: file,
      ...(signal ? { signal } : {}),
    }),

  /**
   * One file as text, for the editor.
   *
   * Not the signed URL, and not a duplicate of it. A signature exists so an
   * `<img>` can fetch bytes without a header; this returns characters, plus the
   * `modifiedAtMs` that `writeText` sends back to prove nothing moved. It also
   * answers for the source files the MIME table does not know — `.py`, `.ts`,
   * `.css` — which the media route serves as attachments.
   */
  readText: (
    workspace: string,
    path: string,
    signal?: AbortSignal,
  ): Promise<FileTextResponse> =>
    request('/api/files/text', FileTextResponseSchema, {
      query: { path, workspace },
      ...(signal ? { signal } : {}),
    }),

  /**
   * Saves text, refusing when the file moved under the editor.
   *
   * `expectedModifiedAtMs` is the timestamp the editor loaded. Omitting it is
   * how a new file is created — there is nothing yet to conflict with.
   */
  writeText: (
    workspaceId: string,
    path: string,
    content: string,
    expectedModifiedAtMs?: number,
  ): Promise<FileEntry> =>
    request('/api/files/text', FileEntrySchema, {
      method: 'PUT',
      body: {
        path,
        content,
        workspaceId,
        ...(expectedModifiedAtMs === undefined ? {} : { expectedModifiedAtMs }),
      },
    }),

  createDirectory: (workspaceId: string, path: string): Promise<FileEntry> =>
    request('/api/files/directory', FileEntrySchema, {
      method: 'POST',
      body: { path, workspaceId },
    }),

  /**
   * Renames or moves an entry within one workspace.
   *
   * Two full paths rather than a new name: a rename and a move are the same
   * operation, and the caller that only wants to rename joins the old parent to
   * the new last segment. Directories go the same way files do — the server does
   * not recurse, it asks the filesystem to move the tree.
   */
  moveFile: (
    workspaceId: string,
    from: string,
    to: string,
  ): Promise<FileEntry> =>
    request('/api/files/move', FileEntrySchema, {
      method: 'POST',
      body: { from, to, workspaceId },
    }),

  /** A short-lived signed URL for a workspace path an `<img>` will load. */
  signUrl: (
    workspaceId: string,
    path: string,
    signal?: AbortSignal,
  ): Promise<SignedUrl> =>
    request('/api/files/signed-url', SignedUrlSchema, {
      method: 'POST',
      body: { path, workspaceId },
      ...(signal ? { signal } : {}),
    }),

  // Workspaces

  workspaces: (signal?: AbortSignal): Promise<WorkspaceListResponse> =>
    request('/api/workspaces', WorkspaceListResponseSchema, {
      ...(signal ? { signal } : {}),
    }),

  /** The id is derived from the name unless one is given. Never a path. */
  createWorkspace: (name: string, id?: string): Promise<WorkspaceSummary> =>
    request('/api/workspaces', WorkspaceSummarySchema, {
      method: 'POST',
      body: { name, ...(id === undefined ? {} : { id }) },
    }),

  /**
   * The label, the folder, or both — whichever the editor's boxes changed.
   *
   * `folder` is the directory name, so sending it moves the tree on disk and
   * repoints every session at it. Omitted fields are left alone, which is what
   * lets one press send one request.
   */
  updateWorkspace: (
    id: string,
    changes: { readonly name?: string; readonly folder?: string },
  ): Promise<WorkspaceSummary> =>
    request(
      `/api/workspaces/${encodeURIComponent(id)}`,
      WorkspaceSummarySchema,
      {
        method: 'PATCH',
        body: {
          ...(changes.name === undefined ? {} : { name: changes.name }),
          ...(changes.folder === undefined ? {} : { id: changes.folder }),
        },
      },
    ),

  /** Detaches it. The folder and everything in it stays on disk. */
  deleteWorkspace: (id: string): Promise<void> =>
    requestVoid(`/api/workspaces/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    }),

  moveWorkspaceSessions: (
    from: string,
    to: string,
  ): Promise<MoveSessionsResponse> =>
    request(
      `/api/workspaces/${encodeURIComponent(from)}/sessions/move`,
      MoveSessionsResponseSchema,
      { method: 'POST', body: { to } },
    ),
};

async function send(path: string, options: RequestOptions): Promise<Response> {
  const { method = 'GET', body, signal, query } = options;

  const url = query === undefined ? path : `${path}?${searchParams(query)}`;
  const hasBody = body !== undefined;
  // A `Blob` is an upload: it goes as its own bytes, and the browser sets the
  // `Content-Type` from the file. Anything else is JSON.
  const raw = body instanceof Blob;

  return await fetch(url, {
    method,
    // The cookie is the credential, and it is `SameSite=Strict`.
    credentials: 'same-origin',
    ...(hasBody && !raw
      ? { headers: { 'content-type': 'application/json' } }
      : {}),
    ...(hasBody ? { body: raw ? body : JSON.stringify(body) } : {}),
    ...(signal ? { signal } : {}),
  });
}

function searchParams(
  query: Readonly<Record<string, string | number | boolean | undefined>>,
): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value !== undefined) params.set(key, String(value));
  }
  return params.toString();
}

/** A body that is not JSON is not a protocol error worth a stack trace. */
async function readJson(response: Response): Promise<unknown> {
  if (response.status === 204) return undefined;
  try {
    return await response.json();
  } catch {
    return undefined;
  }
}

function toApiError(status: number, body: unknown): ApiError {
  const parsed = ErrorResponseSchema.safeParse(body);
  if (parsed.success) {
    const { code, message, details } = parsed.data.error;
    return new ApiError(status, code, message, details);
  }

  // A proxy, a gateway or a crash — anything that answered without going
  // through the error serialiser.
  return new ApiError(
    status,
    'http_error',
    `Request failed with ${status.toString()}`,
  );
}
