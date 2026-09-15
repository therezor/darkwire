# API

REST and WebSocket on the same port as the UI. Default `http://127.0.0.1:3000`.

Every schema in this document exists twice: as a Zod object in `@ghostwire/protocol`,
which is what the browser parses, and as the Rust type the server validates against. The
**OpenAPI 3.1 document is generated from the server's own route manifest and those Rust
types** — served at `/api/openapi.json` — rather than written by hand, because a document
with a source of its own is how a generated document starts lying.

Two gates keep the three honest. A test reflects over the Zod schema modules and fails if
an exported schema was not registered, so nothing can be added to the wire without
appearing in the description. And a per-schema drift test generates the Rust side of each
name and compares it against what the Zod side dumped, after normalising away the
spellings the two generators differ on for no reason — so a field, a bound, a default or
a variant that one side has and the other does not fails CI.

## Authentication

`POST /api/auth/login` with a username and password sets an `httpOnly; SameSite=Strict`
cookie named `ghost_session`. There is **no token in the response body**.

A `Bearer` header is also accepted and wins over the cookie, for scripts. Browser clients
should send `credentials: 'same-origin'` and nothing else.

Three auth classes appear in the tables below:

| Class      | Means                                                             |
| ---------- | ----------------------------------------------------------------- |
| `public`   | No credential.                                                    |
| `required` | Cookie or bearer.                                                 |
| `signed`   | An HMAC-signed, expiring URL rather than a session — for `<img>`. |

Errors are always `{ "error": { "code", "message", "details?" } }` with `code` drawn from
a fixed list: `unauthorized`, `bad_request`, `not_found`, `rate_limited`,
`provider_error`, `tool_error`, `config_invalid`, `not_configured`, `session_busy`,
`internal`.

Listing endpoints use cursor pagination, never offset.

## REST

### System

| Method | Path                | Auth       |
| ------ | ------------------- | ---------- |
| GET    | `/api/health`       | `public`   |
| GET    | `/api/status`       | `required` |
| GET    | `/api/openapi.json` | `required` |

### Auth and first-run setup

| Method | Path                  | Auth       | Notes                                                 |
| ------ | --------------------- | ---------- | ----------------------------------------------------- |
| POST   | `/api/auth/login`     | `public`   | Rate limited, and throttled in two scopes.            |
| POST   | `/api/auth/logout`    | `required` |                                                       |
| GET    | `/api/auth/me`        | `required` |                                                       |
| GET    | `/api/setup`          | `public`   | Says only whether the install is claimed.             |
| POST   | `/api/setup/claim`    | `public`   | Takes the one-time console code.                      |
| POST   | `/api/setup/password` | `required` | The wizard's first password and every later rotation. |

Passwords are at least 12 characters. The username defaults to `ghost`, is lower-cased by
the schema, and changing either revokes every other session.

### Settings

| Method | Path                        | Auth       | Notes                                                                                        |
| ------ | --------------------------- | ---------- | -------------------------------------------------------------------------------------------- |
| GET    | `/api/settings`             | `required` | Credentials never appear — only a `credentialsPresent` boolean per instance.                 |
| PATCH  | `/api/settings`             | `required` | Deep-partial. Also carries `renameAgents`. See [patch semantics](configuration.md#patching). |
| PUT    | `/api/settings/credentials` | `required` | **Write-only.** Nothing reads a key back out.                                                |
| POST   | `/api/settings/reload`      | `required` | Re-reads `config.json` from disk.                                                            |

### Providers and models

| Method | Path                  | Auth       | Notes                                    |
| ------ | --------------------- | ---------- | ---------------------------------------- |
| GET    | `/api/providers`      | `required` | Registry types and configured instances. |
| POST   | `/api/providers/test` | `required` | Checks an endpoint before it is saved.   |
| GET    | `/api/models`         | `required` | Per instance.                            |
| POST   | `/api/models/refresh` | `required` | Re-fetches the catalogue.                |

### Sessions

| Method               | Path                          | Auth       | Notes                                                                                       |
| -------------------- | ----------------------------- | ---------- | ------------------------------------------------------------------------------------------- |
| GET / POST           | `/api/sessions`               | `required` | Lists every origin. `origin` narrows to one, `excludeOrigin` drops one, `workspace` scopes. |
| GET / PATCH / DELETE | `/api/sessions/:key`          | `required` | `PATCH` renames, or moves the session to another agent or workspace.                        |
| GET / DELETE         | `/api/sessions/:key/messages` | `required` |                                                                                             |
| GET                  | `/api/sessions/:key/context`  | `required` | What would be sent to the model, and the token breakdown.                                   |
| GET                  | `/api/sessions/:key/turns`    | `required` | Per-turn stats: model, provider, iterations, stop reason, tokens.                           |
| POST                 | `/api/sessions/:key/branch`   | `required` | Forks the session at a message.                                                             |

### Agents, tools, toolboxes

| Method | Path                          | Auth       | Notes                                                                       |
| ------ | ----------------------------- | ---------- | --------------------------------------------------------------------------- |
| GET    | `/api/agents`                 | `required` | **Read-only.** Agents are created and edited through `PATCH /api/settings`. |
| GET    | `/api/tools`                  | `required` | What is registered, with source and risk band.                              |
| GET    | `/api/toolboxes`              | `required` | Installed toolbox capability policies and approval state.                   |
| GET    | `/api/containers`             | `required` | Independently installed execution-container definitions and approval state. |
| GET    | `/api/sandboxes`              | `required` | Live container instances, sharing and busy state.                           |
| POST   | `/api/sandboxes`              | `required` | Start, stop, restart, or health-check an instance.                          |
| GET    | `/api/mcp`                    | `required` | Each configured MCP server's live state. See below.                         |
| GET    | `/api/extensions`             | `required` | Every discovered extension, its state and its warnings.                     |
| POST   | `/api/extensions/:id/approve` | `required` | Records the digest of the files on disk **now**.                            |
| POST   | `/api/extensions/:id/revoke`  | `required` | Drops the approval. The files stay installed.                               |
| GET    | `/api/commands`               | `required` | The slash commands extensions contribute.                                   |
| POST   | `/api/commands/:id`           | `required` | Runs one. Answers with text, not a resource key.                            |

**The two extension writes are `POST`, not a settings patch, and not idempotent.** An
approval records the digest of the bytes on disk at that moment; putting it in
`config.json` would make it survive an edit to the very files it was about. Nothing about
either is safe to replay across such an edit, which is what rules out `PUT`.

`GET /api/mcp` is read-only, like `/api/toolboxes`: a server is created, edited and
deleted through `PATCH /api/settings`, because it is configuration. What this route
carries is what the settings tree cannot — the state a server is actually in, the reason
it is not connected, and the URL an operator must visit when it wants authorizing. A
build with no MCP client answers `{"servers": []}` rather than a 501: it has no MCP
servers, which is the question being asked.

### Files

Everything resolves inside the workspace jail.

| Method       | Path                    | Auth       | Notes                                                                  |
| ------------ | ----------------------- | ---------- | ---------------------------------------------------------------------- |
| GET / DELETE | `/api/files`            | `required` | List a directory; delete an entry.                                     |
| POST         | `/api/files/upload`     | `required` |                                                                        |
| GET / PUT    | `/api/files/text`       | `required` | `PUT` takes `expectedModifiedAtMs` and refuses a stale write.          |
| POST         | `/api/files/directory`  | `required` |                                                                        |
| POST         | `/api/files/move`       | `required` | Rename and move.                                                       |
| POST         | `/api/files/signed-url` | `required` | Mints a short-lived media URL.                                         |
| GET          | `/api/media/:token`     | `signed`   | Serves the bytes. No session needed, expires by default in 10 minutes. |

### Workspaces

| Method         | Path                                | Auth       | Notes                                                    |
| -------------- | ----------------------------------- | ---------- | -------------------------------------------------------- |
| GET / POST     | `/api/workspaces`                   | `required` |                                                          |
| PATCH / DELETE | `/api/workspaces/:id`               | `required` | `PATCH` can rename the label or move the folder on disk. |
| POST           | `/api/workspaces/:id/sessions/move` | `required` | Moves sessions between workspaces.                       |

### Notifications

| Method | Path                          | Auth       |
| ------ | ----------------------------- | ---------- |
| GET    | `/api/notifications`          | `required` |
| POST   | `/api/notifications/read`     | `required` |
| POST   | `/api/notifications/:id/read` | `required` |
| DELETE | `/api/notifications/:id`      | `required` |

### Automation

| Method | Path                            | Auth       | Notes                                            |
| ------ | ------------------------------- | ---------- | ------------------------------------------------ |
| GET    | `/api/automation/jobs`          | `required` | Every job. Unpaged.                              |
| POST   | `/api/automation/jobs`          | `required` | 201 with the job and its computed next run.      |
| GET    | `/api/automation/jobs/:id`      | `required` | So a deep link to the editor resolves.           |
| PATCH  | `/api/automation/jobs/:id`      | `required` | Recomputes the next run when the schedule moves. |
| DELETE | `/api/automation/jobs/:id`      | `required` | Cascades to the job's run history.               |
| POST   | `/api/automation/jobs/:id/run`  | `required` | **202** with a `pending` run — see below.        |
| GET    | `/api/automation/jobs/:id/runs` | `required` | Keyset-paged, newest first.                      |

`POST .../run` answers **202, not 200**. A turn takes minutes, and a handler that awaited
one would hold the request open past every timeout between the browser and the server. The
run row comes back `pending`; its result arrives later as a notification, and the client
refreshes the run list. It is also the one route where a single HTTP call starts an
unbounded agent turn, so it carries its own rate limit.

A cron expression the scheduler cannot honour is a **422 naming the field**, not a 500:
`parse_cron` answers with a `config` error, whose default mapping is a 500 because a config error
normally means the install is broken. Here it means the operator typed something.

The route table is a manifest that the router registers _from_ and the auth-matrix test
iterates over, so a route cannot exist without a declared auth class, and a manifest entry
with no handler is a type error.

---

## WebSocket

`GET /ws`, authenticated. A plain GET without an upgrade answers `426`. Optional query
parameters `?session=` and `?agent=`.

`PROTOCOL_VERSION` is `2`, sent in the `connected` frame.

### Client → server

| Type              | Does                                                     |
| ----------------- | -------------------------------------------------------- |
| `ping`            | Answered with `pong`.                                    |
| `user.message`    | Starts a turn. Content may be text or parts, for images. |
| `turn.steer`      | Injects guidance into a running turn.                    |
| `turn.stop`       | Aborts.                                                  |
| `turn.regenerate` | Drops the last answer and re-runs.                       |
| `user.edit`       | Rewrites a user message and re-runs from it.             |
| `session.new`     | Starts a session.                                        |
| `session.switch`  | Rebinds this socket.                                     |
| `session.resume`  | `{ lastSeq }` — replays what was missed.                 |
| `tool.approve`    | Answers an approval prompt, with a scope.                |

### Server → client

Turn events: `turn.start` · `assistant.delta` · `reasoning.delta` · `tool.call` ·
`tool.progress` · `tool.result` · `tool.approvalRequest` · `notice` · `turn.end` ·
`subagent.event`

Session and connection: `connected` · `pong` · `error` · `message.ack` ·
`message.queued` · `context.usage` · `session.status` · `session.reset` ·
`session.replay` · `session.truncated` · `notification` ·
`tools.changed` · `steer`

`context.usage` is filed here rather than with the turn events on purpose. It is emitted
at the end of every tool iteration and reports what the _next_ request would cost —
`estimatedTokens`, `contextWindowTokens` and the same `breakdown`
`GET /api/sessions/:key/context` returns, from the same measurement — which prices the
encoded request rather than the stored records, so a field the wire never carries (an
assistant's `reasoning`, a tool's `risk`) is neither sent in that response nor counted in
the breakdown. It carries no
`turnId`, because it describes the conversation rather than the turn that grew it, and it
is emitted by the root loop only: a subagent's window belongs to the child's session, and
reporting it would move a meter that is describing somebody else's history.

### Sequencing and replay

Every session-scoped server event carries a monotonic `seq`. A reconnecting tab sends
`session.resume { lastSeq }` and the server replays from a per-session ring buffer
(`server.replayBufferSize`, default 512) — so a refresh mid-answer rebuilds the in-flight
turn instead of losing it.

`connected`, `pong` and `error` are the only unsequenced frames; they are addressed to one
client rather than to the session.

Past the ring the answer is `session.replay { complete: false }` and the stored tail. **512
frames is not many** — every nested delta of a delegation is one — so this is the ordinary
outcome of reloading during a subagent, not a corner case.

The ring is not the only thing retained, and this is the case the second structure exists
for. The server also keeps the turn that is _running_, whole, from its `turn.start`
(`server.turnLogMaxBytes`, default 16 MiB). Adjacent deltas of the same part are merged
into one frame as they are retained, so a hundred thousand tokens of answer costs one
entry and the budget is spent on tool output rather than on length. When a resume falls
past the ring while such a turn is open, the reply is both halves at once —
`session.replay { complete: false, resumingTurnId, messages }` followed by that turn's own
frames:

- **`messages`** is the stored tail, and it is history as usual.
- **`resumingTurnId`** names the one turn the two sources overlap on. Its finished
  iterations are in the tail _and_ in the frames. The frames are the fuller account: they
  carry the iteration still streaming, and they carry the `subagent.event`s of every
  delegation, which never enter the parent session's history at all. A client drops that
  turn from the tail it was just handed **and** from whatever it still holds itself, then
  rebuilds it from the frames. The user message that opened the turn is not dropped — no
  frame recreates it.

`resumingTurnId` is only ever set alongside `complete: false`; a covered resume is already
sending those frames from the ring. It is also absent when nothing is running, and when
the log overran its budget — a half-log would render a turn that began in the middle, so
the server falls back to the stored tail alone rather than replaying part of one.

A client is still expected to render a `subagent.event` whose delegating `tool.call` it
never saw: the wrapper carries `agentId` and `label` so the card can name itself, and that
is what the fallback path leaves it with.

**The server emits deltas and the client accumulates.** The server never holds a second
copy of the answer text.

### Concurrency and backpressure

One session runs one turn at a time; further messages queue FIFO up to 8, and past that
the client gets `session_busy` rather than a silent drop. A socket that buffers more than
4 MiB is closed with code 1013 — a client that cannot keep up is better told than starved.

An agent event and a sequence number are the whole of a server message, which is what lets
the CLI, the browser and a channel consume identical events.
