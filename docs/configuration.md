# Configuration

One YAML file: `~/.darkwire/config.yaml`, or `$DARKWIRE_HOME/config.yaml`, or
`--home <dir>/config.yaml`. No JSON, no TOML, no XDG.

**A missing file is normal.** The schema produces a complete tree from `{}`, so every key
below has a default and an install with no config file runs. A _malformed_ file is a hard
error listing the dotted paths that failed — it will not silently fall back to defaults.

**It is safe to commit.** Credentials are never in it; they live in the encrypted vault.

Writes are atomic: validate, write `config.yaml.tmp` at mode `0600`, rename.

## Conventions in this tree

- `0` means "no limit" on anything named `*TimeoutMs` or `*PerMinute` — with one stated
  exception, `tools.maxOutputChars`, which is also an allocation bound.
- Durations always carry their unit in the name.
- Every nested object is prefaulted, so `{}` parses to a fully populated tree.
- There are no schema transforms anywhere, so the whole thing stays representable as JSON
  Schema and the OpenAPI document is generated rather than written.
- A relative path resolves against the DarkWire root, never against the process working
  directory.

---

## `workspaces`

The folder the workspaces live in, as a single root-level string.

```yaml
{ 'workspaces': '/Users/you/DarkWire/workspaces' }
```

Every workspace is one directory in here, the default included. They are siblings, so
none of them can reach another's files.

Root-level rather than on an agent, because an agent _works in_ a workspace and does not
own one: the folder is a property of the session, and several agents with separate
identities opening the same one is what the feature is built around. `agents.list.<id>`
therefore has no such key.

Empty means `~/DarkWire/workspaces`. Deliberately not defaulted to that literal string:
writing one machine's home directory into the file makes the config non-portable and a
container mount silently wrong. A relative path is resolved against the root, never
against the process working directory.

This tree does **not** follow `DARKWIRE_HOME`. That variable moves DarkWire's own state:
the settings, the database, the vault and the logs. The files you work on are yours and
stay where you put them. `DARKWIRE_WORKSPACES` is what moves them, and `--workspaces`
wins over both for one run.

## `agents.list.<id>`

**Every agent states its own settings, and nothing inherits.** A field an entry does not
name is filled by the schema's own default — the values in the table below — never by
another agent's answer. So a hand-written entry stays short (`{"label": "Coder",
"model": "qwen3:8b"}` is a whole agent) while "what does this agent run on" is
answerable from the entry alone.

`default` is prefaulted into existence: it is the agent every unbound conversation runs
on, and with nothing above it there would otherwise be nothing for a fresh install to
run. The id also names the agent's directory on disk, so it follows the workspace id
rules.

| Key                   | Type                                     | Default   | Notes                                                                                                                                         |
| --------------------- | ---------------------------------------- | --------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `model`               | string                                   | `''`      | Empty means **unconfigured**, not "pick one". The agent is still listed and editable; only a turn on it is refused, with a message saying so. |
| `provider`            | string                                   | `'auto'`  | An instance id, a bare provider type, or `auto`.                                                                                              |
| `maxTokens`           | int > 0                                  | `8192`    | Output cap per response.                                                                                                                      |
| `contextWindowTokens` | int > 0                                  | `65536`   | What the context inspector measures against.                                                                                                  |
| `temperature`         | 0–2                                      | _unset_   | Absent means the request carries no `temperature` at all and the provider applies its own, the only correct answer for models that reject it. |
| `maxToolIterations`   | int > 0                                  | `40`      | Tool rounds in one turn.                                                                                                                      |
| `toolTimeoutMs`       | int ≥ 0                                  | `0`       |                                                                                                                                               |
| `loopWallTimeoutMs`   | int ≥ 0                                  | `0`       | Wall-clock cap on a turn, checked at the top of each iteration.                                                                               |
| `subagentTimeoutMs`   | int ≥ 0                                  | `0`       | Applies to delegations _this_ agent makes.                                                                                                    |
| `reasoningEffort`     | `off\|minimal\|low\|medium\|high\|xhigh` | _unset_   | Absent sends nothing, which is not the same as `off`. See below.                                                                              |
| `approvalTimeoutMs`   | int > 0                                  | 5 min     | How long an `ask` prompt stays open before it counts as denied. `0` is refused: an approval that never expires holds the turn open forever.   |
| `maxOutputChars`      | int > 0                                  | `8192`    | Head+tail budget for one tool result. `0` is refused: `read` sizes its buffer from this, so `0` would read one byte of every file.            |
| `exec`                | object                                   | see below | What the `exec` tool may run for this agent. Whether it has `exec` at all is the `tools` map below.                                           |
| `lazyDiscovery`       | boolean                                  | `false`   | Send `tool_search` plus the pinned tools instead of every permitted tool. See [Lazy discovery](tools.md#lazy-discovery).                      |
| `pinnedTools`         | string[]                                 | `[]`      | Tools that stay in the list while `lazyDiscovery` is on. Names, not permissions. Replaced whole on a save.                                    |

`reasoningEffort` is sent as the wire's own `reasoning_effort`, verbatim, and only
`off` is translated — into whatever the provider spells "do not think" as
(`ProviderSpec.reasoningOffBody`). Which levels mean anything is the model's business,
not this project's: `minimal` is OpenAI's, `xhigh` is Qwen3.8's top rung and its default.
An endpoint that rejects the field has the parameter dropped and the turn retried, with a
`degraded` notice saying so. An endpoint that _accepts and ignores_ it is the quiet case
and there is no notice for it — Ollama replaces a model's chat template with a generic
one, and the reasoning level lives in the template it discards, so the value has no
effect there. llama.cpp's `llama-server` run with `--jinja` forwards it to the real
template, and does.

Four of these are editable from a session without opening the settings panel: `/model`,
`/effort` and `/temperature` in the browser's composer and at the terminal's prompt write
`model`, `provider`, `reasoningEffort` and `temperature` onto the agent that session runs
on. They save, so a choice made at a prompt is the same choice on the next launch. See
[the CLI reference](cli.md#slash-commands).

There is no key here for skills. Every sheet the workspace holds is indexed in the prompt
and the agent opens the one it needs; see [Skills](skills.md). `pinnedSkills` and
`maxPinnedSkills` used to live in this table and are the worked example of the paragraph
below — a `config.yaml` still carrying them parses, and loses them the next time it is
written.

An agent entry carries no catch-all field, so it drops what it does
not know: a `config.yaml` carrying a key this table does not list parses without error and
loses it on the next write. There is no migration and no error, which is the whole of
the upgrade path — a declared key nothing reads is worse than a missing one, because it
reads as a setting that does nothing and the file gives no way to find that out.

How much memory costs in the prompt is **not** here either: it is bounded by a count of
files and a short derived title rather than a token budget, and none of the bounds is
configurable. See [Memory](memory.md).

Whether an agent may remember at all is **not** here: it is the `memory` tool's permission
in `agents.list.<id>.tools`. Skills work the same way through `skill`. One switch per
feature, and it is the one already in the permission map.

### The rest of an entry

Beside the settings above, an entry carries the keys below — what the agent _is_, rather
than what a turn on it sends.

`workspaces` is not among them, and cannot be: the working folder is root-level and shared
by every agent that opens it. See [`workspace`](#workspace).

Entries are created two ways, and both land in the same shape: the web UI's agent
editor, and editing this file by hand. `darkwire agent list` prints what is here
([CLI](cli.md#darkwire-agent)). However an entry got here, it is edited the same way
afterwards.

| Key                | Type                                 | Default           | Notes                                                                                                        |
| ------------------ | ------------------------------------ | ----------------- | ------------------------------------------------------------------------------------------------------------ |
| `label`            | string                               | `''`              | Falls back to the id.                                                                                        |
| `systemPrompt`     | string                               | `''`              | The agent's **whole** identity prompt as a template. Empty inherits the built-in. See [Prompts](prompts.md). |
| `livePrompt`       | string                               | `''`              | The per-iteration live-state block. Empty inherits; a single space deletes the section.                      |
| `wrapUpPrompt`     | string                               | `''`              | Appended in the last few iterations. Empty inherits; a single space silences it.                             |
| `platformPrompt`   | string                               | `''`              | Fills `{{platformPolicy}}`, the `## Running commands` section. Two built-ins, host and confined.             |
| `toolPolicyPrompt` | string                               | `''`              | The tool-output policy. A template naming neither `{{tag}}` nor `{{nonce}}` saves with a warning.            |
| `memoryPrompt`     | string                               | `''`              | The memory section. Only rendered while the `memory` tool is granted. See [Memory](memory.md).               |
| `skillsPrompt`     | string                               | `''`              | The skills section. Only rendered while the `skill` tool is granted. See [Skills](skills.md).                |
| `promptMode`       | `template\|raw`                      | `'template'`      | `raw` makes `systemPrompt` the entire system message — nothing is placed around it.                          |
| `toolPrompts`      | `Record<string, ToolPromptOverride>` | `{}`              | Per-tool replacements for the description and the argument descriptions. See [Tools](tools.md).              |
| `enabled`          | boolean                              | `true`            |                                                                                                              |
| `tools`            | `Record<string, allow\|ask\|deny>`   | see below         | **Replaces, never merges.** A tool absent from the map is not enabled.                                       |
| `environment`      | `{ name, alwaysUseOwn, network }`    | `{ name: '', … }` | Where this agent's commands run; empty means the host. See [Environments](environments.md).                  |
| `subagents`        | `{ id, prompt, permission }[]`       | `[]`              | Agents this one may delegate to, in the order the model sees them.                                           |

The nine prompt templates share one rule: **`''` inherits the built-in, and a single space
deletes the section.** Empty has to keep meaning "I have not chosen" or an install would
freeze on the wording that shipped the day each agent was made, which leaves a space as
the only way to say "I want this gone". `systemPrompt` is the exception — whitespace-only
counts as empty there, because an identity-less agent is never what was meant.

### `agents.list.<id>.toolPrompts.<tool>`

| Key           | Type                     | Default | Notes                                                                                           |
| ------------- | ------------------------ | ------- | ----------------------------------------------------------------------------------------------- |
| `description` | string                   | `''`    | Replaces what the tool tells the model it does. Empty inherits; a single space advertises none. |
| `fields`      | `Record<string, string>` | `{}`    | Top-level argument name → its description. A name the schema does not have is a warning.        |

Keyed by advertised tool name, so it reaches built-ins, MCP and extension
tools and `ask_<id>` subagent tools alike — and for a subagent it wins over
`subagents[].prompt`, being the more specific of the two. **Types, `required` and `enum`
are not here**: they stay generated from the tool's own argument type, which is also what
a call is deserialised into, so the advertised schema cannot drift from what the tool will
accept.

A new agent is seeded with:

```yaml
{
  'read': 'allow',
  'ls': 'allow',
  'write': 'allow',
  'edit': 'allow',
  'exec': 'ask',
  'memory': 'allow',
  'skill': 'allow',
}
```

`memory` and `skill` are the switches for their two prompt sections — denying the tool
also removes the section it feeds. They are seeded on, because an agent that silently
fails to remember reads as broken rather than as unconfigured. This is the seed for a
_new_ agent: an install that predates them has neither until an operator grants it.

That seeding is the one place a tool's risk band turns into a permission, and it happens
at creation where the operator can see the result and change it. Nothing reads a risk band
at call time.

### `agents.list.<id>.environment`

| Key             | Type                    | Default  | Notes                                                                              |
| --------------- | ----------------------- | -------- | ---------------------------------------------------------------------------------- |
| `name`          | string                  | `''`     | An installed environment name, or empty to run on the host.                        |
| `alwaysUseOwn`  | boolean                 | `false`  | Pins the agent here whoever delegates to it. Off follows the caller.               |
| `network.mode`  | `none\|allowlist\|open` | `'none'` | Refused unless one is named: egress is enforced by its gateway.                    |
| `network.allow` | string[]                | `[]`     | What `allowlist` permits. One entry per destination, any of the four shapes below. |

**This is the only place egress is configured.** There is no second ceiling in the
environment definition, so what an agent may reach is one value in one file.

An entry is exactly one of four shapes:

| Entry             | Matches                             |
| ----------------- | ----------------------------------- |
| `10.0.0.0/8`      | Any address in the block            |
| `93.184.216.34`   | That address                        |
| `api.example.com` | That host, exactly                  |
| `.example.com`    | That host and every subdomain of it |

**A name entry never authorises a raw address, and an address entry never authorises a
name.** `curl http://10.0.0.5` needs the block; `curl http://internal.corp` needs the
name. Letting a block admit a hostname would mean resolving every unlisted name to find
out whether its answer lands inside, which turns a refused destination into a DNS query
the caller chose the name of.

Blocks and addresses are enforced by the gateway's packet filter. Names are enforced by
the egress proxy, which sees the name rather than the address a name resolved to, and so
is the one thing DNS rebinding cannot defeat. An entry in a range nothing may reach, such
as `169.254.0.0/16`, is refused when the config is read: the packet filter has no notion
of that table, so this is the only check there is.

**Nothing inside an environment resolves a name.** The proxy does it, on the engine's
side of the boundary. That is why there is no resolver to configure, and it has three
consequences worth knowing before you meet them as a hang:

- Traffic that is not HTTP reaches an address, never a name.
- A name is reachable on 80 and 443 only, because the proxy pins the scheme and port.
- Only a client that honours `HTTP_PROXY` reaches a name at all. curl, wget, pip and
  python-requests do. Node's `fetch` does not.

There is no `image`, `runtime`, `caps` or `limits` here, deliberately. Those live in the
environment definition, a file an operator writes. A value with no representation in this schema cannot be reached by a
config patch — which is what keeps the hardening operator-only while the egress request
is not.

### `tools.web`

How this install reaches the web, for `web_fetch` and `web_search`. Install-wide
rather than per agent, unlike `exec` and the result budget: these describe the shape of
an outbound connection and the machine making it, not anything one agent should answer
differently from another. **What an agent may reach** is still the agent's, in
[its allow-list](#agentslistidenvironment).

| Key              | Type            | Default   | Notes                                                                                               |
| ---------------- | --------------- | --------- | --------------------------------------------------------------------------------------------------- |
| `searchProvider` | `auto\|searxng` | `'auto'`  | What `web_search` asks. Both are keyless.                                                           |
| `searchUrl`      | string          | `''`      | The SearXNG instance, for `searxng`. Ignored otherwise.                                             |
| `userAgent`      | string          | `''`      | Empty sends a browser profile. A value is sent verbatim, with no client hints.                      |
| `timeoutMs`      | number          | `20000`   | Per fetch. Shown in seconds in Settings.                                                            |
| `readTimeoutMs`  | number          | `15000`   | Per page read inside a search. Shown in seconds in Settings.                                        |
| `maxBytes`       | number          | `5242880` | Bytes, counted as the body arrives and after decompression. Bounds memory, not what the model sees. |
| `cacheEntries`   | number          | `64`      | Pages and result sets held in memory. `0` disables.                                                 |
| `cacheTtlMs`     | number          | `900000`  | How long an entry stays usable. `0` disables. Shown in seconds.                                     |

**There is no API key, deliberately.** Both backends are keyless, because a search tool
should not require an account with a search company. `auto` scrapes public front doors
and promises nothing; `searxng` points at an instance you run, which is the configuration
that actually holds.

Editable in **Settings → Tools**, which is the whole of this block on one screen.

The cache is per process and keyed by agent, so a page one agent fetched is never served
to another: two agents have different allow-lists, and sharing would be an egress bypass
through the cache. It is deliberately not durable, because an entry that outlived the
policy it was fetched under would be the same bug with a longer fuse.

### `agents.list.<id>.subagents[]`

| Key          | Type               | Default   | Notes                                                                                                                                    |
| ------------ | ------------------ | --------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `id`         | string             | —         | An id in `agents.list`.                                                                                                                  |
| `prompt`     | string             | `''`      | **The tool description the model reads.** This is what decides when it fires. Empty falls back to a sentence naming the agent.           |
| `permission` | `allow\|ask\|deny` | `'allow'` | Allow by default: an agent given a subagent is one whose operator wants it used. The tools the _subagent_ runs are gated by its own map. |

---

## `providers.<instanceId>`

Keyed by an **instance id you choose**, not by a provider id — which is how two Ollama
servers become two entries.

| Key            | Type                    | Default | Notes                                                           |
| -------------- | ----------------------- | ------- | --------------------------------------------------------------- |
| `type`         | string                  | —       | **Required.** A registry id: `ollama`, `openai`, `custom`, …    |
| `label`        | string                  | `''`    | Shown in the UI. Falls back to the type's display name.         |
| `apiBase`      | string                  | _unset_ | Falls back to the type's default. Required for `custom`.        |
| `extraHeaders` | `Record<string,string>` | `{}`    | Replaced wholesale by a patch, not merged.                      |
| `models`       | string[]                | `[]`    | A fallback list for endpoints that do not answer `GET /models`. |
| `enabled`      | boolean                 | `true`  | A disabled instance is kept and skipped.                        |

**No API key field.** Keys live in the vault under the namespace `providers`, keyed by the
same instance id — so the two entries below can hold different tokens, and this file can
be committed.

```yaml
{
  'agents': { 'defaults': { 'provider': 'ollama-gpu', 'model': 'qwen3:8b' } },
  'providers':
    {
      'ollama': { 'type': 'ollama' },
      'ollama-gpu':
        {
          'type': 'ollama',
          'label': 'GPU box',
          'apiBase': 'http://gpu.lan:11434/v1',
        },
    },
}
```

See [Providers](providers.md) for the registry table and the resolution order.

---

## `server`

| Key                       | Type        | Default       | Notes                                                                             |
| ------------------------- | ----------- | ------------- | --------------------------------------------------------------------------------- |
| `host`                    | string      | `'127.0.0.1'` | A non-loopback host with `auth.enabled: false` **refuses to start**.              |
| `port`                    | int 1–65535 | `3000`        | One port for the API, the WebSocket and the UI.                                   |
| `replayBufferSize`        | int ≥ 0     | `512`         | Events retained per session so a reconnecting tab can replay across turns.        |
| `turnLogMaxBytes`         | int ≥ 0     | 16 MiB        | Budget for keeping the _running_ turn whole, so a reload comes back to all of it. |
| `auth.enabled`            | boolean     | `true`        |                                                                                   |
| `auth.sessionTtlMs`       | int > 0     | 30 days       |                                                                                   |
| `auth.rateLimitPerMinute` | int ≥ 0     | `0`           | `0` is off.                                                                       |
| `auth.signedUrlTtlMs`     | int > 0     | 10 minutes    | Lifetime of the HMAC-signed URLs that serve workspace media to `<img>`.           |

**These two are not alternatives, and `turnLogMaxBytes` is the one that decides whether a
reload comes back to the whole answer.** `replayBufferSize` counts events, and 512 is
fewer than it sounds: every delta is one, and every delta a _subagent_ produces is one
too, so a delegation of any length pushes the call that started it out of the buffer.
Raising it is not the fix — it only widens how far back a reconnect can pick up _across_
turns, and it costs that many whole events per live session.

The turn that is running is kept separately and in full, from its `turn.start`, which is
what a reload actually needs. Adjacent deltas of the same part are merged as they are
retained, so length is nearly free and the budget is spent on tool output — a turn that
reads fifty large files is the shape that reaches 16 MiB; a turn that writes for ten
minutes is not. Only a session with an open turn holds one, so the ceiling is the number
of turns running at once rather than the number of live sessions.

Past the budget the log is dropped and the server says so by naming no turn, because a
half-log would rebuild a turn that began in the middle. A resume then falls back to the
stored tail alone — the behaviour that made reloading mid-delegation lose the run. `0`
turns the log off outright and makes that the behaviour always. See
[API](api.md#sequencing-and-replay) for what a client does with either answer.

`0.0.0.0` and `::` count as remote. All of `127.0.0.0/8`, `localhost` and `::1` count as
loopback. The refusal is a startup error rather than a warning because a warning scrolls
past, and the result is an unauthenticated shell-capable agent on a LAN address.

---

## `tools`

The MCP servers, and nothing else. **Which tools an agent may call is not here**, that
is `agents.list.<id>.tools`, and neither is how a tool runs for an agent: `exec`, the
approval timeout and the result budget are all per agent, in the table above. See
[Tools & permissions](tools.md).

### `agents.list.<id>.exec`

| Key               | Type     | Default                       | Notes                                                                                                                                                                                                     |
| ----------------- | -------- | ----------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `timeoutMs`       | int ≥ 0  | `0`                           |                                                                                                                                                                                                           |
| `pathAppend`      | string   | `''`                          | Appended to the child's `PATH`.                                                                                                                                                                           |
| `allowedBinaries` | string[] | `[]`                          | `argv[0]` allow-list, matched on basename. **Empty means "anything not denied"**, the opposite convention to `tools` on the agent, and deliberately so: this narrows a tool the operator already enabled. |
| `deniedBinaries`  | string[] | `[]`                          | Checked first.                                                                                                                                                                                            |
| `envAllowlist`    | string[] | `['PATH','HOME','LANG','TZ']` | Everything else is scrubbed from the child's environment.                                                                                                                                                 |
| `maxOutputBytes`  | int > 0  | `1048576`                     | Enforced while the child writes, not after it exits.                                                                                                                                                      |

There is no `enable` switch: an agent that should not run commands sets `exec: deny` in
its `tools` map, like any other tool. There are no patterns here for `$(...)`, backticks
or `| sh` either. The exec tool takes an `argv` vector and spawns `argv[0]` directly, so
there is no string for a shell metacharacter to live in. Scanning for them would reject
legitimate commands while blocking nothing.

### `tools.mcpServers.<id>`

The id is part of every tool name this server contributes (`mcp_<id>_<tool>`),
and those names are the keys of every agent's permission map — so renaming a
server revokes its tools from every agent that had been granted one. The
MCP servers panel treats the id as fixed once created for that reason.

| Key                      | Type                                                         | Default          | Notes                                            |
| ------------------------ | ------------------------------------------------------------ | ---------------- | ------------------------------------------------ |
| `type`                   | `stdio\|sse\|streamableHttp`                                 | _unset_          | Inferred from `command` vs `url`.                |
| `command`, `args`, `env` | string, string[], record                                     | `''`, `[]`, `{}` | For `stdio`.                                     |
| `url`, `headers`         | string, record                                               | `''`, `{}`       | For the HTTP transports.                         |
| `oauth`                  | `{ authUrl, tokenUrl, clientId, scopes, callbackTimeoutMs }` | _unset_          | HTTP only. Tokens go to the vault.               |
| `toolTimeoutMs`          | int ≥ 0                                                      | `0`              | `0` waits as long as it takes.                   |
| `enabledTools`           | string[]                                                     | `['*']`          | Upstream names; a trailing `*` matches a prefix. |
| `enabled`                | boolean                                                      | `true`           |                                                  |

**`type` is inferred but never guessed at `sse`.** An entry with a `command` is
`stdio`, one with a `url` is `streamableHttp`, and one with both is refused —
reaching the deprecated transport takes an explicit `"type": "sse"`. An entry
that names neither is refused too, as a row on the MCP servers panel rather than
a settings save the operator loses.

**An explicit `"type": "sse"` is dialled as Streamable HTTP anyway**, and the
server's status row says so. The MCP client this build uses ships no SSE client
transport at all, and Streamable HTTP serves the same URL shape, so the
alternative to dialling it that way is refusing the entry outright. What an
operator should do about it depends on the server: if it speaks Streamable HTTP,
delete the `"type"` — the warning goes away and nothing else changes. If it
speaks only the legacy transport, it cannot be reached from here, and the
connection error says that in as many words rather than leaving the cause to be
guessed at from a timeout.

**`env` and `headers` replace rather than merge**, like `providers.<id>.extraHeaders`:
they are edited as one block of text, and merging key by key would leave no way
to remove an entry. `null` at `tools.mcpServers.<id>` deletes the server, and
`null` at its `oauth` says it does not use OAuth.

Two guards do **not** apply here, and the reasoning is in the module headers of
`crates/mcp`. A stdio `command` does not go through `guard_exec`: that guard
constrains argv a _model_ wrote inside the workspace jail, and it refuses the
absolute paths and `npx`-shaped invocations every MCP server uses. A `url` does
not go through the guarded fetch's SSRF blocklist, because the commonest MCP
endpoint by far is `http://127.0.0.1:…`, which is exactly what that blocklist
exists to refuse. Both are operator configuration, in the same trust class as
`providers.<id>.apiBase`. What _is_ enforced: the child gets `env` plus a
minimal inherited set rather than this process's whole environment, its stderr
goes to the log rather than to yours, and the URL must parse as `http`/`https`.

---

## `ui`

| Key        | Type          | Default | Notes                                                                                                                                                                          |
| ---------- | ------------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `locale`   | BCP-47 string | `'en'`  | Unknown tags fall back to the nearest match and ultimately to English, rather than failing to parse and taking the whole file down. **`en` is the only shipped locale today.** |
| `timezone` | IANA name     | `'UTC'` | The one clock this install reads and writes. See below.                                                                                                                        |

### `ui.timezone` is the only timezone

Everything is **stored** in UTC — every persisted instant is epoch milliseconds — so this
is not a storage format. It is the answer to _whose clock_, and there is deliberately one
of it rather than three. A job used to carry its own `tz`, the scheduler had a default,
and the browser rendered in whatever zone it happened to be in; predicting when a job
fired meant holding all three in your head.

It governs both halves, and that is the point:

- Every timestamp in the web UI is **rendered** in it, with the zone named.
- Every wall-clock time is **read** in it — a cron expression and the one-shot picker
  alike — so `0 9 * * *` fires at 9am on the same clock the next-run line is printed
  against, and nobody converts anything by hand.

**Changing it reschedules existing cron jobs.** That follows from the above and is not a
side effect worth hiding: an expression is a wall-clock time, so its stored instant is
only valid against the zone it was computed in. `PATCH /api/settings` recomputes them.
Interval and one-shot jobs do not move — neither has a wall clock in it.

**Always a concrete IANA name, never a rule.** The settings select offers `System`, but
resolves it to a real zone before saving. Storing the rule would mean the server resolved
it to the _host_ zone while a browser resolved it to the _reader's_ — the disagreement
this field exists to end.

**UTC rather than the host zone as the default.** A server's own zone is a property of
where it happens to be running: it moves when the box moves, it is whatever the image was
built with, and on a laptop it follows the traveller. `0 9 * * *` would then fire at a
different real instant after a migration nobody connected to it.

## `channels`

| Key             | Type    | Default |
| --------------- | ------- | ------- |
| `sendProgress`  | boolean | `true`  |
| `sendToolHints` | boolean | `false` |

This object is **loose** by design: each channel — built-in or from an extension — parses its own
block, so installing a channel does not require a schema change here. Both keys above are
read once when the channel manager is built, so they are install-wide rather than per
conversation.

### `channels.telegram`

| Key              | Type       | Default                                      |
| ---------------- | ---------- | -------------------------------------------- |
| `enabled`        | boolean    | `true`                                       |
| `allowlist`      | string[]   | `[]` — **and empty refuses to start**        |
| `admins`         | string[]   | `[]` — empty means everyone on the allowlist |
| `agentId`        | string     | _unset_                                      |
| `workspaceId`    | string     | _unset_                                      |
| `pollTimeoutSec` | 1–50       | `30`                                         |
| `editIntervalMs` | number ≥ 0 | `2000`                                       |
| `apiBase`        | string     | `https://api.telegram.org`                   |

**The bot token does not go here.** Put it in the credential vault under
`channels`/`telegram`, or in `TELEGRAM_BOT_TOKEN`. A `token` key in this block is read as
a last resort and logs a warning at startup, because `config.yaml` is a plain file that
backups, dotfile repositories and screen shares all reach.

An `allowlist` entry is `<telegram id>` or `<telegram id>|<label>`; the label is for
whoever reads the file and the logs, and nothing matches on it. One list covers people and
groups, because Telegram numbers them apart — a user id is positive and a group id is
negative. **Inside a group both must be listed**, the group and the person typing: being
in the room is not a decision you made.

To find your id: message the bot and read the log line it writes for the sender it
refused. That is the whole onboarding path.

**Settings → Channels edits all of this from the browser**, and puts the token in the
vault rather than in this file. Saving there restarts the channel, so a bot switched on in
the UI connects without anyone touching the terminal — and the panel says whether it did.
The keys below the line (`pollTimeoutSec`, `editIntervalMs`, `apiBase`) are file-only:
they are tuning knobs, and a panel row for each would be four rows nobody reads.

`admins` gates the commands that reach past one conversation — `/model`, which moves the
whole install onto another model, and the workspace manager `/workspace` opens.

> The bot's `/model` is still the install-wide one described here, and still lasts only
> as long as the process. The web UI's and the terminal's now edit the agent the session
> runs on and save it; see [the CLI reference](cli.md#slash-commands).

Telegram is registered only when a token resolves, so an install that has never configured
a bot starts exactly as it did before. A token that resolves and is then refused by the
API fails startup rather than leaving a channel silently dead.

## `scheduler`

Read and honoured. Edited in **Settings → Automation**. The jobs themselves are a page of
their own — a list an operator keeps is not a setting.

Four knobs, and every one of them is true of the **engine**. None describes a task —
that is what a job is for.

| Key                       | Type    | Default                               |
| ------------------------- | ------- | ------------------------------------- |
| `scheduler.enabled`       | boolean | `true` — the master switch            |
| `scheduler.concurrency`   | int > 0 | `2` — concurrent runs across all jobs |
| `scheduler.catchUpOnBoot` | boolean | `true`                                |
| `scheduler.runRetention`  | int > 0 | `200` — runs kept **per job**         |

> **There is no `scheduler.timezone`.** The zone a cron expression is read in is
> [`ui.timezone`](#uitimezone-is-the-only-timezone), because it is also the zone every
> timestamp is rendered in — one install-wide answer to "whose clock" rather than a
> scheduler knob and a display convention that could disagree.

> **There is no `scheduler.heartbeat` block, and there should not be.** A heartbeat _is_ a
> scheduled job: its interval is the job's schedule, its task file and decision model are
> the job's payload, and its on/off is the job's own `enabled`. A config block restating
> all of that is a second vocabulary for one concept — and the half an operator would
> configure while the other half is what actually runs. Create a job whose payload kind is
> "read a task file and decide".

**`catchUpOnBoot` coalesces.** A job whose time passed while the process was down runs
**once** when it comes back, not once per missed occurrence — a five-minute job that was
down for a weekend would otherwise produce hundreds of runs at boot. With it off, a missed
one-shot is recorded `skipped` rather than deleted (a reminder that vanished without trace
is worse than one that says it was missed), and a recurring job simply rearms.

**The cron dialect is five fields** — minute, hour, day-of-month, month, day-of-week —
with `*`, lists, ranges, `/step`, and names (`JAN`, `MON`). A six-field expression is
refused by name rather than absorbed: every dialect that grew a seconds column put it at
the front, so reading `0 * * * * *` as five fields plus a stray runs something sixty times
more often than asked. **A job carries no zone of its own** — the schema refuses one
rather than ignoring it — and the expression is read in `ui.timezone`.

**Daylight saving is handled rather than assumed.** A wall-clock time the zone skips
(spring forward) has no occurrence that day; one that happens twice (fall back) fires at
the **earlier** instant, so an hourly job sees the repeated hour once and there is a single
two-hour gap in real time. Firing on both would mean a job written "hourly" running
twenty-five times that day.

When **day-of-month and day-of-week are both restricted, a day matches if _either_ does**.
`0 0 13 * 5` is "the 13th, and also every Friday", not "Friday the 13th".

### The heartbeat payload

A job's payload is either **a fixed message** or **a heartbeat**: read a file, and let a
cheap model decide whether there is anything to do. Three steps, and only the middle one
is an agent turn.

1. **Decide.** The task file is read through the workspace jail and capped at 64 KiB, then
   one _direct provider call_ with a single tool, forced. Not a turn — registering a
   `heartbeat` tool in the shared registry would leak it into every ordinary chat and every
   subagent, and an agent turn for a yes/no would write two messages into a session every
   interval forever. The answer is `skip` or `run`, with a reason.
2. **Run.** Only on `run`, and using the instruction the model gave. A real turn, through
   the hub, on the job's agent.
3. **Evaluate.** A second forced call: is this worth interrupting anyone? `no` still writes
   the run and its output — it just raises no notification. That is the whole reason a
   heartbeat every thirty minutes is tolerable rather than infuriating.

Three rules make it safe to leave running:

- **Fail-closed on acting, fail-loud on reporting.** A decision that cannot be read —
  malformed arguments, or no tool call at all because the resilience ladder stripped
  `tool_choice` — is a **skip with a warning**, never a run. Defaulting to `run` means an
  unbounded agent turn started on garbage, every interval, decided by the cheapest model in
  the install.
- **Cheap paths cost nothing.** No task file, or an empty one: skipped, with no provider
  call at all. That is the normal state of a fresh install and it must not bill.
- **Errors always notify.** The evaluate step never gets to veto a failure. A job that has
  quietly not worked for a week is worse than a spurious toast.

## `extensions`

Full detail in [Extensions](extensions.md).

| Key                        | Type     | Default | Notes                                                                 |
| -------------------------- | -------- | ------- | --------------------------------------------------------------------- |
| `extensions.load`          | string[] | `[]`    | Extra directories, beside `~/.darkwire/extensions`. Paths, not specs. |
| `extensions.disabled`      | string[] | `[]`    | Ids. A disabled extension is discovered and not loaded.               |
| `extensions.allowOverride` | boolean  | `false` | Lets a later-discovered id shadow an earlier one instead of erroring. |
| `extensions.settings.<id>` | object   | `{}`    | One extension's own block, loose — it parses its own.                 |

**`load` takes a path, never a package spec.** Nothing here fetches at load time; an
extension is a directory an operator put on the box, which is what keeps an air-gapped
install air-gapped.

**Per-extension settings are a sub-object rather than the loose top level `channels`
uses.** This block already has keys of its own, so an extension whose id happened to be
`load` would otherwise overwrite one. `null` at `extensions.settings.<id>` deletes the
block.

**Credentials do not go here.** An extension's secret belongs in the vault under the
`extensions` namespace, keyed by extension id, the same way a channel's bot token does.

---

## Patching

The UI and CLI send deep-partial patches through `PATCH /api/settings`. The rules:

- **A key the patch does not mention is untouched.** The patch schema strips defaults, so
  an absent key genuinely means "not mentioned" rather than "reset to default".
- **Arrays replace.**
- **These paths replace wholesale rather than merging** — `providers.*.extraHeaders`
  (merging per header would make deleting one impossible, since a patch has no syntax for
  "absent") and `agents.list.*` (an agent is edited as a whole, and almost every field is
  an override that may legitimately be cleared).
- **`null` deletes, and only at these paths** — `providers.*`, `tools.mcpServers.*`,
  `agents.list.*`.
  Everywhere else `null` is a validation error.
- **The merged tree is re-parsed** through the full schema, so a bad combination is a 400
  rather than a broken next boot.

Agents are created, renamed and deleted through `PATCH /api/settings` (which carries a
`renameAgents` field); `GET /api/agents` is read-only. Credentials go through
`PUT /api/settings/credentials`, which is write-only — nothing reads a key back out.

---

## Environment variables

| Variable                     | Does                                                                       |
| ---------------------------- | -------------------------------------------------------------------------- |
| `DARKWIRE_HOME`              | The root directory. Same as `--home`.                                      |
| `DARKWIRE_PASSWORD`          | Fallback for `darkwire serve --password`.                                  |
| `DARKWIRE_USERNAME`          | Fallback for `darkwire serve --username`.                                  |
| `DARKWIRE_LANG`              | Locale override. Ranks above `config.ui.locale`, which ranks above `LANG`. |
| `DARKWIRE_LOG_LEVEL`         | Then `LOG_LEVEL`, then `info`.                                             |
| `DARKWIRE_DEBUG`             | Any non-empty value prints stack traces instead of the friendly message.   |
| `DARKWIRE_FIDELITY_ORIGINAL` | Path used by the optional e2e design-fidelity gate. Without it, it skips.  |

Provider keys — read only when the vault has no entry for that instance, because **the
vault wins over the environment**:

`OPENAI_API_KEY` · `ANTHROPIC_API_KEY` · `OPENROUTER_API_KEY` · `GEMINI_API_KEY` ·
`DEEPSEEK_API_KEY` · `GROQ_API_KEY` · `XAI_API_KEY` · `VLLM_API_KEY`

One key per registry entry, whether or not that entry can be built today. `anthropic` is
the one entry that cannot: it names the `anthropic-messages` wire, and this build ships an
adapter for `openai-chat` alone, so an instance of it is refused at construction rather
than falling back. `gemini` speaks `openai-chat` against Google's compatibility endpoint
and works. See [Providers](providers.md).

---

## CLI flags that override config

| Flag                        | Overrides                                                |
| --------------------------- | -------------------------------------------------------- |
| `--home <dir>`              | The root. Same as `DARKWIRE_HOME`.                       |
| `--host` / `--port`         | `server.host` / `server.port`.                           |
| `--workspaces <dir>`        | The folder the workspaces live in.                       |
| `--workspace-id <id>`       | Which workspace new sessions land in. A different thing. |
| `--model` / `--provider`    | Every agent's `model` / `provider`, for one invocation.  |
| `--ui <dir>`                | Serve a UI built somewhere else.                         |
| `--password` / `--username` | Sets the login credential without the wizard.            |
