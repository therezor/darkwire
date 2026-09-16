# Tools and permissions

What an agent can actually _do_, and who decided it could. Two halves: the tools
themselves — eight built in, plus whatever MCP servers and extensions contribute — and the
`allow | ask | deny` map that gates every one of them, per agent.

The short version, if you read one paragraph: **enablement and permission are the same
map.** A tool the map does not mention is not enabled, and not in a way that has to be
checked — it never reaches the tool registry entries the model is sent, so there is nothing for
it to call and nothing to refuse.

## The built-ins

Eight, and the count is deliberate: anything expressible as a command is `exec`'s job. A
`grep` tool would be a worse `rg`, and a `move_file` tool would be a worse `mv`.

Six of them are capabilities the agent cannot get any other way. The last two —
`memory` and `skill` — are not, and are here for a second reason: a tool carries a
per-agent permission, so being a tool is what makes each feature switchable without a
config flag beside it that could disagree. Denying either removes its prompt section too.
See [Memory](memory.md) and [Skills](skills.md).

| Tool         | Args                                        | Risk band | Does                                                                                        |
| ------------ | ------------------------------------------- | --------- | ------------------------------------------------------------------------------------------- |
| `read_file`  | `path`, `offset?`, `limit?`                 | `safe`    | Reads a file in the workspace.                                                              |
| `list_dir`   | `path`, `recursive?`, `maxEntries?`         | `safe`    | Lists a directory.                                                                          |
| `write_file` | `path`, `content`                           | `write`   | Creates or overwrites.                                                                      |
| `edit_file`  | `path`, `oldText`, `newText`, `replaceAll?` | `write`   | Exact-match replacement.                                                                    |
| `exec`       | `argv: string[]`, `timeoutMs?`              | `exec`    | Runs a program. On the host, or in a [container](environments.md) when the agent names one. |
| `automation` | `action`, plus a name, message and schedule | `exec`    | Schedules a turn for later. See below.                                                      |
| `memory`     | `name`, `description`, `type`, `body`       | `write`   | Records one durable fact in [memory](memory.md). No path argument.                          |
| `skill`      | `name`                                      | `safe`    | Opens one of the workspace's [skills](skills.md).                                           |

All file paths resolve inside the workspace jail; see [Security](security.md). `exec`
takes an argv array, never a command string.

Setting `tools.exec.enable: false` removes `exec` from the definitions entirely rather
than advertising a tool that will refuse — a model told about a tool that always fails
spends iterations rediscovering that. `automation` follows the same rule against
`scheduler.enabled`.

### `automation`

The only built-in that acts on the _future_, and the only one **absent from
`DEFAULT_AGENT_TOOLS`** — a new agent cannot reach it at all until an operator grants it.
That asymmetry is the point: a single approved `exec` runs once, and a single approved
`automation` create runs forever, unattended, on a timer.

Grant it per agent, in **Agents → the agent → Tools**, by moving its row off `Disabled`.

The model's surface is a strict subset of the operator's — `create`, `list`, `delete`. No
`update`, because repointing an existing job's payload is the one edit nobody watches
happen; no `run`, no enable/disable. Schedules are the same three kinds a
[scheduled job](configuration.md#scheduler) has, one at a time: `every_minutes`, `cron`,
or an ISO `at`. There is no `tz` argument: a cron is read in the install's
[`ui.timezone`](configuration.md#uitimezone-is-the-only-timezone), which is the zone named
beside the current time in the model's own prompt — so the hour it writes is the hour it
sees, with nothing to convert.

**The run happens on the agent that scheduled it, and in a session of its own.** The
port stamps the caller's `agentId` onto the payload — the tool cannot, because a tool
running on arguments a model wrote must not be able to schedule a turn as somebody else.
The session is a fresh `automation:{jobId}`, so a scheduled turn cannot see the
session that created it; the tool says so in its own description, because a `message`
that refers back to "what we discussed" otherwise fails silently a week later. `list` and
the create confirmation both report the schedule and the resolved next run, which is how a
model checks that its cron was read the way it meant.

Everything the tool cannot be trusted with lives on the other side of `AutomationPort`,
which the composition root binds to the calling agent and session before the tool ever
sees it. Three refusals, each answered as a tool _result_ rather than a throw:

| Refusal       | Why                                                                                                                                                                                                                                      |
| ------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `nested`      | A scheduled run may not schedule. Otherwise a job that says "keep an eye on things" creates another that says the same, without bound. Read from `sessions.origin`, because the stored row is the only thing a caller cannot argue with. |
| `at-capacity` | A cap per agent, so a model in a loop meets a wall rather than filling the table.                                                                                                                                                        |
| `not-yours`   | An agent lists and deletes only what it created; the operator's jobs are invisible to it. One answer for "no such job" and "not yours", so ids cannot be probed for the difference.                                                      |

Note that the subagent chain guard does **not** cover the first: `turn.chain` is empty for
a turn a person started, and the scheduler starts turns the same way.

Jobs an agent made carry `createdBy`, so the panel can say which agent asked and link back
to the session that caused it.

More tools arrive two ways: from a subagent, which appears as `ask_<id>` (see
[Architecture](architecture.md#subagents)), and from an MCP server.

**A [container](environments.md) adds no tools.** It decides _where_ `exec` runs, not what
an agent may call — the agent's `tools` permission map stays the whole authority, with or
without one. So an agent gains nothing by being given a container and loses nothing by
having one taken away, beyond where its commands land.

## MCP servers

An operator adds one in **Settings → MCP servers**, over stdio, Streamable HTTP or the
legacy SSE transport. Its tools land in the same registry as the built-ins and appear as
ordinary permission rows in the agent editor, so **nothing is granted implicitly** — an
absent entry in an agent's map already means "not enabled", and an existing agent gains
no capability until someone says so. See [Configuration](configuration.md#toolsmcpserversid)
for the settings and the two security decisions behind them.

Four things about the bridge are worth knowing before reading `crates/mcp`:

- **The name is qualified and generated.** `mcp_<server>_<tool>`, sanitised into the
  `[A-Za-z0-9_-]{1,64}` every provider accepts, with a digest suffix when it would not
  fit. Two servers can both advertise `search`; one shared registry cannot hold two of
  them, and `ToolRegistry::register` treats a duplicate as a `conflict` rather than
  letting load order decide which one a call reaches.
- **The schema is passed through, not converted.** Every other tool derives its JSON
  Schema from its own argument type; an MCP server supplies the JSON Schema directly, so
  `bridge_tool` implements `Tool` against it. Round-tripping it through a Rust type would
  advertise a shape the server did not describe — every converter is lossy on `$ref`,
  `oneOf` and `format` — and the call would then fail _at the server_, which reads as the
  model being broken. What the bridge validates is the contract `tool_conformance` states
  and no more:
  an object, no unknown keys, required keys present, declared types honoured, and `"10"`
  accepted where a number is wanted. Anything deeper is the server's own business.
- **A band is `safe` only if the server said so.** `readOnlyHint: true` earns `safe` and
  `destructiveHint: true` earns `exec`; silence earns `network`, because an MCP call is
  third-party code over a socket by construction. Bands remain advisory — see below.
- **A server going away is a state, not an error.** Its tools are unregistered, the
  browser is told through `tools.changed`, and it reconnects on a widening backoff with
  no attempt cap. A call that lands in the window between gets an `isError` result the
  model can read. An unreachable server never fails a settings save.

An agent that had been granted a tool whose server is currently down keeps its row,
badged **not installed** — `agents.list.*` is replaced wholesale on save, and a list
built only from the live registry would silently drop the operator's opinion.

### Defining one

One argument type is the only copy of a tool's shape. `schemars` derives the JSON Schema
the model is shown from it, `jsonschema` validates every call against that same schema,
and serde then turns the value into the struct the handler takes — a hand-written schema
could drift from the type, and the drift would show up as a model call that validates and
then crashes.

Numbers coerce, because models emit `"10"` as often as `10`.

`ToolRegistry::execute` **never fails the turn**. A failure comes back as a result carrying
`isError` and an error kind, because a throw at that point would take down the turn rather
than letting the model read what went wrong and try something else. `definitions()` is
memoised and sorted by name, so the prompt prefix a provider caches does not shuffle
between requests.

Every registration carries a source — `builtin`, `mcp` or `extension` — so uninstalling
an extension can remove exactly its tools, with no module-cache surgery and no restart.
The source is the _coarse_ grain, though, and neither MCP nor extensions use it for a
single owner going away: `unregister_by_source(ToolSource::Extension)` would take every other
extension's tools with it, so the names each owner last contributed are remembered and
removed by name. That is `ToolSink`, and one implementation serves both.

## Extension tools

An extension's tools arrive through the MCP bridge rather than being defined in
process — descriptors from `tools/list`, bridged against the schema they carry — and the
host rewrites the name to `ext_<extension>_<tool>` on the way in, with
the same 64-character cap and digest tail `mcp_<server>_<tool>` gets. What arrives in the
registry is an ordinary `Tool` and nothing downstream can tell the difference.

**Registering one grants nothing.** It joins the registry, and every agent still decides
for itself whether it may call it through `agents.list.<id>.tools`, where an absent name
means disabled. There is no permission vocabulary in an extension's manifest,
deliberately: one reachable from a file the extension ships would be a way to grant
something the operator never enabled. See [Extensions](extensions.md).

### Rewriting what a tool says about itself

A tool's description is the sentence that decides whether the model reaches for it, and
it used to be a string literal beside the handler — the one part of the payload an
operator could read in the context inspector and not change.
`agents.list.<id>.toolPrompts` is the key that fixes that, per agent:

```json
"toolPrompts": {
  "exec": {
    "description": "Run a program. Prefer `rg` over `grep`.",
    "fields": { "argv": "argv array; argv[0] is the binary.", "timeoutMs": "0 is no limit." }
  }
}
```

**Prose only, and the boundary is load-bearing.** `type`, `required`, `enum` and the rest
of the schema stay generated from the tool's argument type — which is also what
`parse_args` validates a call against. Letting an operator supply a schema would let the advertised shape
drift from the accepted one, and the failure mode is a model dutifully passing a field
that then fails validation on every call: an agent that looks broken for a reason nothing
reports. For the same reason a `fields` name the schema does not have is dropped and
reported rather than added.

Top-level arguments only. A path syntax reaching `argv.items` would be a second
mini-language to specify and validate, for a field whose parent can say the same thing in
a sentence.

In the editor each tool row carries a pencil that opens a dialog — a box for the
description and one per argument, each showing **the tool's own wording as its
placeholder**. That is the whole question an operator is answering: whether the built-in
is good enough. A box that said "the built-in description" instead cost them a trip to
this page to find out what it was. The row then shows whichever description the model
actually receives, so the list cannot disagree with the payload.

The rewrite happens in `AgentLoop::tool_definitions`, after the subagent definitions are
appended — one pass covering built-ins, MCP and extension tools and `ask_<id>` alike, and
the reason `toolPrompts` beats `subagents[].prompt`. It cannot
happen in the registry: `definitions()` is memoised and shared by every agent in the
process, so one agent's wording would become everyone's.

A key naming no tool this agent advertises is an `unknown_tool_prompt` config warning, not
an error — a tool leaves the list when an MCP server goes down or `exec` is switched off,
and neither should stop an agent that was working a moment ago.

## Permissions

**Per tool, per agent, and one map rather than a selection plus a policy.**

```json
"tools": { "read_file": "allow", "list_dir": "allow", "exec": "ask" }
```

| Value    | Means                                                                        |
| -------- | ---------------------------------------------------------------------------- |
| `allow`  | Runs unattended.                                                             |
| `ask`    | The operator sees the arguments and answers before it runs.                  |
| `deny`   | Refused, with a result the model can read.                                   |
| _absent_ | **Not enabled at all** — it never reaches the definitions sent to the model. |

Enabling a tool and choosing its permission are one act. The alternative — a selection
list plus a separate policy table — is how a newly created agent quietly ends up holding
every tool the registry happens to carry.

Note this is the opposite convention to `tools.exec.allowedBinaries`, where empty means
"anything not denied". That is deliberate: an allow-list of _binaries_ narrows a tool the
operator already turned on, while this is the list of tools themselves.

A new agent is not born empty either — it is seeded with the file tools, `memory` and `skill` on
`allow` and `exec` on `ask`, because an agent that can do nothing looks broken to whoever
just made it. That seeding is the only place a risk band becomes a permission, and it happens once,
at creation, where it is visible and editable.

### Risk bands decide nothing

`safe`, `write`, `exec` and `network` are metadata. They badge the tool card and the
approval prompt so an operator can see at a glance what class of thing is being asked
for, and they seed a new agent's map. **Nothing reads a band at call time.** There used to
be a risk-band-to-policy table in config; it was replaced by the per-agent map because a
band is a property of a tool and a permission is a property of a deployment.

### Where the check happens

Between the `tool.call` event and execution — the one point every transport shares. The
browser, the CLI and a channel all get the same enforcement without each implementing it.

The split of responsibility is worth knowing:

- **The loop decides whether to ask.** It reads the agent's map, emits
  `tool.approvalRequest` for `ask`, and owns the deadline.
- **The gate decides the answer.** It is whatever the transport installed.

**With no gate installed, `ask` runs the tool.** That is what keeps `darkwire chat` in a
terminal unchanged — the operator typing the request _is_ the approval, and a prompt with
no UI to answer it would deadlock. Any transport that exposes the agent beyond its
operator's keyboard must install a gate; the server does.

### Answering

An approval prompt takes one of three scopes:

| Scope     | Remembered                                                                                                                                                           |
| --------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `once`    | Not at all.                                                                                                                                                          |
| `session` | For the session, keyed on the **root** session — so answering "this session" inside a subagent means the session you are looking at, not the one-delegation session. |
| `always`  | Per agent, for the life of the process. A standing "always allow `exec`" on a permissive agent must not pre-approve it for a locked-down one.                        |

**A refusal is remembered exactly like an approval.** Denying `always` is a real answer.

Standing approvals are dropped when an agent is deleted and carried across a rename.

### Timeouts and denial

`tools.approvalTimeoutMs` (default 5 minutes) is a property of the deployment, and the
loop owns it rather than the gate — a gate that hangs must not hang the turn. A gate that
throws denies.

Three denial reasons, and they are phrased for two different readers: `policy`,
`declined`, `timeout`. The model gets a tool result worded to stop it retrying the same
call; the operator gets a notice worded for a human.

**An abort during an approval is a cancellation, not a denial.** They have different
consequences for the turn, and conflating them means a stopped turn looks to the model
like a refused tool.

Either way the call still produces a `tool` message — providers reject an assistant turn
whose `tool_calls` went unanswered.

## Auditing

Every tool call and result is stored in the session, and every turn records its model,
provider, iterations, stop reason and token counts. For `exec`, the argv is recorded as
argv — there is no command string to reconstruct or mis-quote, and the record survives the
command running inside a container.
