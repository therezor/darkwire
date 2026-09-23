# Tools and permissions

What an agent can actually _do_, and who decided it could. Two halves: the tools
themselves (fourteen built in, plus whatever MCP servers and extensions contribute) and the
`allow | ask | deny` map that gates every one of them, per agent.

The short version, if you read one paragraph: **enablement and permission are the same
map.** A tool the map does not mention is not enabled, and not in a way that has to be
checked — it never reaches the tool registry entries the model is sent, so there is nothing for
it to call and nothing to refuse.

## The built-ins

Fourteen. The test each one passes is that it is a capability the agent cannot obtain as
cheaply any other way, which for most of them means `exec` cannot do the job: a command
needs an approval nobody may be there to give, and it needs a binary the container image
may not ship. A `move_file` tool would still be a worse `mv`, and there is no such thing.

`grep` and `find` are the two that look like commands and are not. Both are ripgrep
compiled in rather than spawned, so they are confined to the workspace by construction,
bound their own output, and work in an image carrying neither `rg` nor `fd`. Searching
is also what an agent does before almost every edit, so paying an approval for it once
a turn is the difference between an agent that reads the code and one that guesses.

Ten of them are capabilities the agent cannot get any other way. `memory`, `skill` and
`todo` are not, and are here for a second reason: a tool carries a per-agent permission,
so being a tool is what makes each feature switchable without a config flag beside it
that could disagree. Denying any of the three removes its prompt section too. See
[Memory](memory.md) and [Skills](skills.md). `tool_search` is the door to the other tools
when [lazy discovery](#lazy-discovery) is on, and is registered only then.

| Tool          | Args                                                      | Risk band | Does                                                                                        |
| ------------- | --------------------------------------------------------- | --------- | ------------------------------------------------------------------------------------------- |
| `read`        | `path`, `offset?`, `limit?`                               | `safe`    | Reads a file, 2000 lines at a time, and says how many are left.                             |
| `ls`          | `path`, `recursive?`, `maxEntries?`                       | `safe`    | Lists a directory. Hides nothing.                                                           |
| `grep`        | `pattern`, `path?`, `glob?`, `mode?`, and more            | `safe`    | Searches file contents by regex. Skips what `.gitignore` skips.                             |
| `find`        | `pattern`, `path?`, `limit?`                              | `safe`    | Finds files by glob, most recently modified first.                                          |
| `write`       | `path`, `content`                                         | `write`   | Creates or overwrites.                                                                      |
| `edit`        | `path`, `oldText`, `newText`, `replaceAll?`, or `edits`   | `write`   | Exact-match replacement, one block or several atomically.                                   |
| `exec`        | `argv: string[]`, `timeoutMs?`                            | `exec`    | Runs a program. On the host, or in a [container](environments.md) when the agent names one. |
| `automation`  | `action`, plus a name, message and schedule               | `exec`    | Schedules a turn for later. See below.                                                      |
| `memory`      | `action`, `key`, `content?`                               | `write`   | Reads, saves or deletes one [memory](memory.md). No path argument.                          |
| `skill`       | `name`                                                    | `safe`    | Opens one of the workspace's [skills](skills.md).                                           |
| `todo`        | `tasks: {text, status}[]`                                 | `safe`    | Replaces the session's task list. See below.                                                |
| `tool_search` | `query?`, `activate?: string[]`                           | `safe`    | Finds hidden tools by words, or adds named ones to the list. See below.                     |
| `web_fetch`   | `url`, `format?`, `maxChars?`                             | `network` | Reads a page as markdown, main content only. See below.                                     |
| `web_search`  | `query`, `count?`, `read?`, `recent?`, `site?`, `region?` | `network` | Searches the web and reads the top results. See below.                                      |

All file paths resolve inside the workspace jail; see [Security](security.md). `exec`
takes an argv array, never a command string.

`grep` and `find` are the only tools that leave anything out: both skip what
`.gitignore` skips, and `.git` itself. `ls` hides nothing, because it answers what is in
a directory and a listing that omits things reads as an empty directory. A search
answers where the code is, and a vendored copy of the answer is noise.

There is no install-wide switch for `exec`. An agent that should not run commands sets
`exec: deny` in its permission map, and the tool leaves that agent's definitions with
it. What `exec` may run, and for how long, is on the agent too (`agents.list.<id>.exec`).
`automation` is the one built-in an install can switch off as a whole, against
`scheduler.enabled`: with no scheduler there is nothing for it to write to.

### `todo`

The plan a long turn runs on. One call replaces the **whole** list — there is no add,
no complete and no reorder — so the model sends what the list should now be and the last
call wins. Ten tasks, a hundred characters each, at most one `doing`; the caps are in the
schema, so an oversized call is refused before the tool runs.

The list is stored on the session, in its metadata bag, and placed in the **runtime half**
of the prompt, so it arrives on every iteration:

```text
## Tasks

[x] Inspect auth
[>] Update sessions
[ ] Add tests
```

That is what it is for. A forty-step turn that wrote a plan at step three can read it
back at step thirty, and an operator watching can see where it has got to without reading
every tool call.

**A subagent gets its own list.** A delegated run opens its own session, and the port the
tool receives is bound to the session whose turn it is, so neither run can see or clear
the other's plan.

Reading it back from somewhere other than the prompt:

- `/tasks`, at the terminal and in a Telegram chat. Both open the list to be acted on as
  well as read: `ctrl-x` at the terminal empties it, and a Telegram chat gets a button
  that does the same.
- `GET /api/sessions/{key}/tasks`, which the web UI's panel above the composer reads, and
  `DELETE` on the same path, which its Clear button calls.

**The list is stamped with the point in the conversation it was written at** — the seq the
next message would have taken — and that is what makes re-running a turn forget its plan.
`/edit` and `/regenerate` truncate, and a plan written during the turn they are re-running
describes answers that have just been deleted; it is dropped by the same comparison the
messages are cut by. A plan from an earlier turn stays, because it still describes work the
transcript has a record of.

`/clear` takes the plan with the history, for the same reason. `/branch` does not carry it
into the fork: the stamp is in the source's sequence space and a fork reseats from 1, so
the number would point at the wrong message.

Emptying it by hand is `todo` with `tasks: []`, `ctrl-x` in `/tasks`, its button in a
chat, or Clear on the panel. There is no way to drop a single task, and that is the
decision rather than the gap: the tool replaces the whole list on its next planning step,
so one removed by hand is back a moment later.

### `automation`

The only built-in that acts on the _future_, and one of three **absent from
`DEFAULT_AGENT_TOOLS`** — a new agent cannot reach it at all until an operator grants it.
That asymmetry is the point: a single approved `exec` runs once, and a single approved
`automation` create runs forever, unattended, on a timer. The two web tools are the other
two, for the neighbouring reason: they reach outside the machine.

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

### `web_fetch` and `web_search`

Both **absent from `DEFAULT_AGENT_TOOLS`**, like `automation`. Grant them per agent in
**Agents → the agent → Tools**, and note the risk band is `network`, so a granted row
seeds to `ask` rather than `allow`.

`web_fetch` returns a page as markdown: the article, with navigation, adverts and
boilerplate scored away rather than stripped by tag name. A documentation page is
typically 80 to 95% markup and chrome, so this is roughly a tenth of the tokens `curl`
would cost and reads far better. `format: "raw"` returns the body untouched, which is
what an API returning JSON wants.

`web_search` returns a numbered list **and reads the first three results**. That default
is the important one: with reading opt-in, a model handed six one-line snippets answers
from the snippets. Three, because one source is an opinion and two that agree is a
coincidence. `read: 0` gives the list alone.

Both stay inside the agent's `maxOutputChars`. The tool computes its own budget and cuts
at a line boundary, naming what it dropped, because the registry's own truncation keeps
the head **and** the tail: a result that overflowed would come back with its middle
removed. When the budget cannot carry three extracts, it reads fewer and says so.

Everything they cannot do says what to try instead: a page rendered client-side names the
absence of a JavaScript engine, a PDF names `exec`, and a 401, 403 or 429 says the site is
refusing automated requests and that retrying will not help.

**How the install reaches the web is install-wide; what an agent may reach is the
agent's.** The backend, the user agent and the caps are
[`tools.web`](configuration.md#toolsweb), on one screen in **Settings → Tools**, because
they describe this machine. The allow-list is
[on the agent](configuration.md#agentslistidenvironment), because it describes that
agent. Under `allowlist` a search backend is reachable only if the operator listed its
host, and the refusal names the hosts to add. On the host, an agent with no network mode
set reaches the public internet, which is what `exec` there can already do.

**Search is best effort by default.** `auto` scrapes public search front doors in
rotation, then falls back to Hacker News through Algolia's keyless API. Those front doors
change their markup without notice and rate-limit a server address faster than a
residential one; one of the three served a captcha while this was being written. The
configuration that actually holds is a SearXNG instance you run, named in
**Settings → Tools**.

**There is no API key, and that is the point.** Both backends are keyless, because a
search tool should not require an account with a search company.

Requests carry a current browser user agent and the client hints that match it, because a
great many sites serve a challenge page to anything that looks automated. Set
`tools.web.userAgent` to identify yourself honestly instead; the client hints are dropped
with it, since a hint set naming Chrome beside a custom agent gives the whole thing
away. TLS fingerprinting is **not** addressed: rustls does not present Chrome's
handshake, so a site behind a fingerprinting CDN refuses these requests whatever headers
they carry, and the refusal says so rather than inviting a retry.

### Lazy discovery

Every tool the agent may call is normally sent, schema and all, on every request. With a
few MCP servers that is thousands of tokens a turn, and a small model chooses worse from
a long list. Switching on **Find tools on demand** in the agent editor
(`agents.list.<id>.lazyDiscovery`) sends the short list instead: `tool_search`, then the
tools pinned on that agent (`pinnedTools`, the pin on each tool row), and nothing else.
The rest are reachable by name. Per agent, because the agent on a small local model wants
it and the agent on a large hosted one may not.

- **Search**: `tool_search({query: "github issue"})` answers with up to ten names and one
  line each, matched on the name, the description, the title and the argument names and
  descriptions. Names already in the list say so. No schema is in the answer.
- **Activate**: `tool_search({activate: ["mcp_github_create_issue"]})` puts the tool in
  the list from the very next request, for the rest of the session. Several names at
  once is one round trip. The answer names what changed and nothing more; the schema
  arrives in the tools array, where it is paid for once.
- **Pins** grant nothing: a pinned tool the agent denies is still not sent. Pin what
  every turn needs (`read`, `grep` and `find`, say) and leave the rest to search.
- **A hidden tool called by name anyway runs.** The permission map is what decides, not
  the advertised list, and the call counts as an activation.

The system prompt gains one fixed section, `## Finding tools`, saying that the list is
short and how to lengthen it. It names no hidden tool and no count, so an activation never
changes the cached half of the prompt. An operator can rewrite `tool_search`'s description
like any other tool's, in the agent editor.

`tool_search` takes no permission: it reveals nothing the agent could not already call and
runs nothing itself, so the scope always admits it and the editor shows it without a
permission select, in its own group under the switch. An agent whose permitted tools are
all pinned has nothing to hide and is sent the whole list even with the switch on. Activations are in memory: a restart or
a settings save starts each session short again, and the model gets a tool back by
searching for it or calling it.

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
"tools": { "read": "allow", "ls": "allow", "exec": "ask" }
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

**Every surface that can ask, asks.** The web UI shows a card, Telegram a card with
buttons, and `darkwire chat` a question under the composer. All three show the command
and offer the same answers: once, this session, a standing rule for `exec`, and no.

**A surface that cannot ask refuses.** A one-shot `darkwire chat "…"`, `--json` and a pipe
have nothing to answer with, so an `ask` call is refused at once, with no prompt announced,
and the model is told nobody could approve it. **With no gate installed, `ask` runs the
tool.** Only `darkwire chat --yes` builds a loop that way.

A prompt nobody answers is refused when the agent's `approvalTimeoutMs` passes. The model
is told nobody answered in time, which it reads differently from a person saying no.

### Command rules

`exec` is the one built-in whose permission depends on its arguments. An agent's
`exec.rules` decide per command, and the `exec` row's own permission is the answer for a
command no rule matches. Rules are checked where every call is authorised, before it
runs, so they can turn `ask` into `allow` or `deny` without a prompt.

A rule is an argv pattern and an action, and it applies wherever the agent runs:

```yaml
- { action: allow, argv: [cargo, test, '*'] } # cargo test, with any arguments
- { action: deny, argv: [git, push, '*'] } # never push
```

- Each token matches one argument exactly. A final `*` matches any remaining arguments,
  none included. There is no other wildcard.
- The first token matches the program's basename, so `git` covers `/usr/bin/git` and
  `git.exe`. A rule may not name a path.

**The most specific rule wins, whatever the order.** Of the rules that match, the one
with more literal tokens wins, then an exact rule over a wildcard, then `deny` over `ask`
over `allow`. So `deny *` plus a few `allow` rows
is an allow-list, and `deny cargo test --release` stays in force beside
`allow cargo test *`. It also means a rule saved from a prompt can never outrank a
narrower one written by hand.

**A shell is its own permission.** A call whose program is a shell (`bash`, `sh`, `zsh`,
`pwsh` and the rest) is capped at `exec.shell`. A wildcard rule cannot lift it, because
`bash *` covers every program there is. Only a rule naming the exact command, such as
`allow bash ci.sh`, decides a shell call on its own, and `shell: deny` refuses every shell
whatever the rules say. On the host the guard refuses the `-c` family regardless.

**A wildcard on a program that runs programs allows everything.** `allow env *`,
`allow sudo *`, `allow python *` and `allow xargs *` each grant any command. The rules
match argv; they do not know what a program does with it.

### Answering

An approval prompt takes one of two scopes:

| Scope     | Remembered                                                                                                                                                           |
| --------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `once`    | Not at all.                                                                                                                                                          |
| `session` | For the session, keyed on the **root** session — so answering "this session" inside a subagent means the session you are looking at, not the one-delegation session. |

**What "this session" covers is the tool's to say.** For most tools it is any call to the
same tool. For `exec` it is the exact command, keyed on a hash of its argv, so approving
`cargo test` does not approve `rm -rf target`.

**A refusal is remembered exactly like an approval.** Denying for the session is a real
answer.

**A standing answer is configuration.** For `exec` the prompt offers **Always allow…**,
which saves an `allow` [command rule](#command-rules) on the agent and runs the call. The
server checks the rule first: it must cover the command, and no more specific rule may
still override it. A rule that fails either check is refused, the prompt stays open, and
nothing is written. A shell call gets no "always": a rule for one covers every program.
For any other tool, a standing answer is its permission set to `allow` on the agent.

A saved rule applies from the next turn. The turn that saved it keeps the settings it
started with, though the approved command is remembered for the session.

**A remembered answer is never announced.** The loop asks the gate what it already holds
before it emits `tool.approvalRequest`, so a repeated command runs without a card
appearing and vanishing again.

### Timeouts and denial

`tools.approvalTimeoutMs` (default 5 minutes) is a property of the deployment, and the
loop owns it rather than the gate — a gate that hangs must not hang the turn. A gate that
throws denies.

Four denial reasons, and they are phrased for two different readers: `policy`, `rule`,
`declined`, `timeout`. The model gets a tool result worded to stop it retrying the same
call; the operator gets a notice worded for a human. `rule` tells the model that a
different command may be allowed, where `policy` means the tool itself is off.

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
