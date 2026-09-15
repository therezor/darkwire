# Architecture

One process, and one binary. It serves the agent, a REST API, a WebSocket and the UI —
compiled into the executable — on a single port, and writes to one SQLite file. Nothing in
it is heavy enough to justify a split-process topology and the
reconnect-and-fall-back-to-HTTP client that would need.

## The crates

Fourteen, plus the four TypeScript packages that stayed. Each crate has its own
tests and its own coverage bar.

| Crate                    | Does                                                                                                                          |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------- |
| `ghostai-protocol`       | The wire types as serde + schemars, mirroring the zod schemas. No I/O.                                                        |
| `ghostai-i18n`           | i18next-compatible lookup over the shipped locale bundles, with typed key constants.                                          |
| `ghostai-core`           | Message types, `SessionStore`, `WorkspaceStore`, the message bus, logging, `Clock`, config loading, history windowing         |
| `ghostai-security`       | `WorkspaceJail`, `guard_exec`, the guarded fetch, the credential vault, nonce fencing, toolbox policy and extension approvals |
| `ghostai-providers`      | The provider registry, the `openai-chat` wire, SSE parsing, resilience, token counting                                        |
| `ghostai-tools`          | The `Tool` trait and registry, the built-in tools, the local and container runners                                            |
| `ghostai-environment`    | The isolated container service/client, shared lifecycle pool, and egress gateway                                              |
| `ghostai-mcp`            | The MCP client, connection lifecycle and the bridge from a remote tool onto `Tool`                                            |
| `ghostai-agent`          | `AgentLoop`, the approval contract, prompt assembly, steering, subagents                                                      |
| `ghostai-channels`       | The `Channel` contract, `ChannelManager`, `TurnProjection` and the Telegram adapter                                           |
| `ghostai-extension-host` | Discovery, the approval check, the JSON-RPC subprocess host, and what an extension contributed                                |
| `ghostai-runtime`        | The composition root: config → provider, jail, store, registry, one loop per agent                                            |
| `ghostai-server`         | axum: REST, the WebSocket hub, auth, the embedded UI, OpenAPI                                                                 |
| `ghostai-tui`            | A domain-free terminal toolkit: key decoding, display-width text, a transient selection region                                |
| `ghostai`                | **The binary.** Every command and flag, and the UI compiled into it.                                                          |

| Still TypeScript      | Does                                                 |
| --------------------- | ---------------------------------------------------- |
| `@ghostwire/web`      | The React SPA                                        |
| `@ghostwire/protocol` | Zod schemas → types, JSON Schema and OpenAPI         |
| `@ghostwire/i18n`     | The i18next instance, locale negotiation, typed keys |
| `@ghostwire/e2e`      | Playwright, plus the optional design-fidelity gate   |

**`protocol` and `i18n` exist twice on purpose.** The browser is the reason: it
parses those schemas on every response and every WebSocket frame, and it reads
those locale bundles at runtime, so the TypeScript copies are not build-time
artefacts that could be generated and thrown away. They stay the source of
truth, the Rust crates mirror them, and a per-schema JSON Schema drift gate
compares the two on every CI run — which is what keeps "mirror" a checkable
claim rather than an intention.

### Layering

```
{ protocol, i18n } → core → security → { providers, tools } → { mcp, agent, environment } ─┬→ runtime ──┐
                                                                              │            │
                     core → channels ──────→ extension-host ──────────────────┘            ├→ ghostai
                                                                                           │  (binary)
        { protocol, i18n } → web (TypeScript)          agent … → server ───────────────────┘
                             tui
```

`protocol`, `i18n` and `tui` declare no workspace dependency at all. `tui` is a
terminal toolkit that knows nothing about this application, which is why it sits
beside the roots rather than under them, and why only the binary reaches for it.

**`server` does not depend on `runtime`.** It takes a `ServerRuntime` trait and
the binary supplies the implementation, so the transport never names the
composition root — which is the same rule as "the agent must never reach back
into the HTTP server", applied one layer up.

Dependencies only run downward, and the rule is mechanical: a crate that does
not list another in its `[dependencies]` cannot `use` it. That is Cargo doing
what pnpm's isolated `node_modules` used to do — the manifests _are_ the layer
graph, and an undeclared import fails to compile rather than merely to lint.

One consequence is visible in the subagent design below: delegation lives in
`AgentLoop` rather than in a tool, because `ghostai-tools` sits underneath it
and a tool's context has no event sink.

## A turn

`AgentLoop::run(input, &token)` spawns the turn and hands back a `Turn`: a bounded stream
of `AgentEvent`, a completion carrying the `TurnResult`, and a guard. **Dropping the `Turn`
cancels the turn's token**, so abandoning the stream unwinds the turn through exactly the
path an explicit stop takes, and a consumer that stops reading stops the turn rather than
filling memory behind it. There is no `on_token` callback anywhere.

Per iteration, up to `maxToolIterations` (default 40):

1. **Drain the steering queue.** Anything the operator typed while the turn was running
   is appended as a user message, prefixed so the model can tell it apart from the
   original request. Capped at 16 pending.
2. **Check the cancellation token, then the wall clock.** `loopWallTimeoutMs` is checked
   at the _top_ of the iteration — a turn should not discover it is out of time halfway
   through a provider call.
3. **Rebuild the runtime half of the prompt** and assemble the request as
   `[system] + history(sessionKey)`.
4. **Stream from the provider.** `assistant.delta` and `reasoning.delta` go out as they
   arrive; the terminal event carries the finished `ChatResult`.
5. **No tool calls** → append the assistant message and finish, unless steering arrived
   while the model was talking, in which case the loop continues.
6. **Tool calls** → authorize, execute, and append the assistant message and every tool
   result in one transaction.

`stopReason` is one of `complete`, `aborted`, `wall_timeout`, `max_iterations`, `error`.

### Invariants

These explain most of the surrounding design, and each one exists because its absence
caused a specific failure:

- **History is append-only.** A provider's prompt cache keys on an exact prefix, so no
  stored message is ever mutated. Regenerate and edit drop a _suffix_, which changes no
  prefix and is therefore allowed.
- **An error response is never appended.** A provider 400 in the transcript poisons the
  session forever — every subsequent turn replays it.
- **A denied or cancelled tool call still gets a `tool` message.** Providers reject an
  assistant turn whose `tool_calls` went unanswered, so a refusal is a _result_, not an
  omission.
- **Tool definitions and the turn's nonce are computed once per turn.** Recomputing them
  per iteration would rewrite the cached prompt prefix five or ten times a turn.
- **`messages[0]` is rewritten, not supplemented.** Two system messages is a shape some
  providers reject and others quietly reorder, and the ordering is what the cache depends on.
- **One cancellation mechanism.** A single `CancellationToken` threads from the request
  through the loop, the provider request, tool execution and any child process. A timeout
  is a `child_token()` of it, not a second mechanism.

### Events

Every event the loop yields is a server message minus its sequence number — the hub just
stamps a counter, and a test asserts that property rather than trusting it.

`turn.start` · `assistant.delta` · `reasoning.delta` · `tool.call` · `tool.progress` ·
`tool.approvalRequest` · `tool.result` · `notice` · `error` · `turn.end` ·
`subagent.event` · `context.usage`

`tool.progress` is emitted on a fixed 15-second heartbeat while a tool runs, so a slow
command looks alive rather than hung.

`context.usage` is the one that is not about the turn. It goes out at the end of each
iteration, once the tool results are written, and reports what the next request would
cost — the same numbers `describe_context` gives the REST route and the CLI, measured from
the prompt the iteration already composed rather than from a second assembly. The
measurement runs through `wire_encode`, the module the transport builds its body
with, so the figures price the request rather than the stored records and nothing the
provider never receives is billed to the window. Only the
root loop emits it: a subagent measures its own session, which is not the one anybody is
reading, and `ContextUsageEvent` sits outside `NestedAgentEvent` so that is a compile
error rather than a convention.

`notice` is the loop telling the operator something without derailing the turn:
`prompt_injection`, `degraded`, `truncated_history`, `provider_fallback`,
`approval_denied`, `agent_fallback`, `tools_disabled`.

The hub retains what it emits in two structures, because a reconnect and a reload ask
different questions. The **replay ring** is bounded by a frame count and answers "what did
I miss since `seq`", across turns. The **turn log** holds the turn that is running, whole,
and answers "what has this turn done so far" — the one a reload asks, and the one no frame
budget can answer, since a delegation spends a frame per token of its subagent. It merges
adjacent deltas of the same part as it retains them, so its size follows the turn's output
rather than its frame count, and it is bounded in bytes. See
[API](api.md#sequencing-and-replay) for how the two are combined in a resume.

### History windowing

`history_for_llm` runs four ordered steps: keep the last `maxMessages` (default 500), start
at the first `user` message, align to a legal tool-call boundary, then truncate tool
results (default 8,000 characters, head and tail with the middle marked).

The boundary alignment is the part that matters. A window that cuts through a tool
exchange leaves either an orphaned tool result or an unanswered tool call, and both are a
provider 400. Truncation is head-and-tail rather than head-only because the end of a
command's output is usually where the error is.

## Subagents

An agent can be given other agents to delegate to. A subagent is not a different kind of
thing — it is an ordinary entry in `agents.list` that another entry points at, so a
researcher is configured, tested and used on its own, and being someone's subagent is a
relationship rather than a mode.

Each becomes a tool named `ask_<id>` (hyphens become underscores), taking a single
required `task` string. The tool _description_ is the operator's own guidance, which is
the part that decides when the model reaches for it.

- **Delegation lives in the loop, not in a tool.** A subagent's turn is a real turn on a
  real loop, and its events stream out wrapped in `subagent.event`.
- **It runs in a session of its own**, in the caller's workspace, linked through metadata
  the way a fork is. That session is excluded from the sidebar and deleted with its
  parent — and is what lets a reloaded transcript fetch the run back.
- **Depth is capped at 3, and cycles are refused** — both as a tool _result_ rather than
  an error, so the model can adapt instead of the turn dying.
- **Nesting forwards rather than recurses.** A grandchild's event is passed through with
  only its `turnId` rewritten, which keeps the wire schema non-recursive.
- **An approval inside a subagent bubbles to the operator** scoped to the session
  they are looking at, not to the delegation.
- **The timeout is the caller's** `subagentTimeoutMs`, composed as a `child_token()` of
  the caller's. It kills the child, not the parent turn.

## What is on disk

Everything under `~/.ghostai`, or `$GHOSTAI_HOME`. Directories are created `0700`.

| Path                       | Contents                                                                                                              |
| -------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| `config.yaml`              | The settings tree. Written atomically via a `0600` temp file and a rename.                                            |
| `ghost.db`                 | One SQLite file, one connection, one WAL.                                                                             |
| `vault.json`, `vault.key`  | The encrypted credential vault.                                                                                       |
| `workspace/`               | The jail root. Named workspaces are subdirectories of it.                                                             |
| `shared/<workspaceId>/`    | The layer agents in one folder share — **outside the jail**, so `write_file` cannot rewrite what an agent is told.    |
| `policy/toolboxes/`        | Installed grant lists. Outside the workspace, so injection cannot edit the policy the agent runs under.               |
| `policy/tool-definitions/` | Reusable operation definitions. No digest of their own: each is covered by the digest of every toolbox that names it. |
| `policy/containers/`       | Container definitions, installed independently of any toolbox.                                                        |
| `runs/<containerId>/`      | Sandbox command transcripts. Outside the workspace — a symlink-planting escape was demonstrated before this moved.    |
| `extensions/<id>/`         | Installed extensions. Approved by a digest over every byte, so state is written elsewhere.                            |
| `extension-data/<id>/`     | What an extension writes at runtime — a sibling of its install directory, never a child.                              |
| `logs/`                    | —                                                                                                                     |

### The database

SQLite, compiled into the binary through `rusqlite`'s bundled amalgamation — no
prebuilds, no shared library to find, no compiler on the install path. That argument used
to come with an asterisk: `node:sqlite` was unflagged only in Node 22.13, so the
no-prebuilds claim was bought with a Node floor an operator had to meet. A single
executable has no floor at all, so what is left is the claim without the asterisk.

One `Connection` is shared by every store, behind a re-entrant lock, so all writes land in
one WAL and a store method that opens a transaction and then calls another store's method
cannot deadlock on itself. Every table is `STRICT`.

| Table                                            | Holds                                                             |
| ------------------------------------------------ | ----------------------------------------------------------------- |
| `sessions`                                       | Key, title, origin, agent, workspace, metadata, sequence counters |
| `messages`                                       | Append-only, `(session_key, seq)` unique, cascade-deleted         |
| `turn_stats`                                     | Per turn: model, provider, iterations, stop reason, token counts  |
| `workspaces`                                     | Id, label, root                                                   |
| `auth_secrets`, `auth_sessions`, `auth_throttle` | Password, username, setup code, sessions, throttle counters       |
| `notifications`                                  | The bell and the archive                                          |
| `extension_approvals`                            | The sha256 over every byte of each approved extension directory   |
| `automation_jobs`                                | Schedule and payload as JSON, plus the indexed `next_run_at_ms`   |
| `automation_runs`                                | One row per execution: status, output, warnings, session key      |

`seq` is both the ordering and the pagination cursor. Timestamps are not usable for
either, because a turn writing parallel tool results collides on them.

`sessions.origin` is `web`, `cli`, `telegram`, `automation`, `subagent`, or an extension id.
Session listing excludes `subagent` **and `automation`** unless asked for one by name.
Both are real rows and neither is a session: one turn, started by a model. Automation
is the one that scales badly if it leaks — a job on a five-minute interval writes about
105,000 sessions a year, and the sidebar is a list of sessions a person had.

A job's `schedule` and `payload` are JSON columns rather than a flat set of nullable ones.
They are discriminated unions, and the union exists precisely so `{kind: 'cron', atMs: 5}`
cannot be represented; spreading them into columns would rebuild that. `state` _is_
decomposed, because `next_run_at_ms` has to be indexable for the timer's due query. Run
history is trimmed per job rather than globally — one shared ceiling would let a busy job's
afternoon evict a nightly job's whole year.
