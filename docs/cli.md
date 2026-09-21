# CLI

**Who this is for:** anyone driving DarkWire from a terminal rather than a browser. It is
the reference for `darkwire` — every command, every flag, and every slash command inside the
chat prompt. If you are setting up for the first time, start with
[Getting started](getting-started.md) and come back here.

One binary, `darkwire`. The terminal and the browser are two views of one install, not two
programs: they share a single `darkwire.db`, so a session you start here is the row the
browser sidebar lists, and a turn started from either goes through the same loop and the
same approval gate.

## Commands

```
darkwire [chat] [message...]      talk to the agent — the default command
darkwire init                     configure this install, in a wizard
darkwire serve                    serve the web UI and the API on one port
darkwire agent     list
darkwire environment list
darkwire sandbox   health | list | start | stop | restart
darkwire extension list | approve <id> | revoke <id>
darkwire help [command]
```

`chat` is the default, so `darkwire "what changed today"` and `darkwire chat "what changed
today"` are the same command.

### Global flags

| Flag                  | Does                                                                  |
| --------------------- | --------------------------------------------------------------------- |
| `--home <dir>`        | The DarkWire root. Beats `$DARKWIRE_HOME`, which beats `~/.darkwire`. |
| `--log-level <level>` | `trace`, `debug`, `info`, `warn`, `error` or `fatal`.                 |
| `--verbose`           | Report what the install is doing, not only the answer.                |
| `--no-color`          | Disable colour. Rarely needed — see below.                            |
| `-v`, `--version`     | Print the version.                                                    |
| `-h`, `--help`        | Print help. `darkwire help <command>` does the same for one.          |

The log level defaults to `error` — or `info` while serving, because a server that says
nothing while it works reads as hung.

Colour is detected rather than assumed: `NO_COLOR`, `FORCE_COLOR`, `TERM=dumb` and
"stdout is a file rather than a terminal" are all honoured, so `darkwire chat > log`
writes prose and not escape codes. `--no-color` is the override for the case detection
gets wrong.

Secondary text — the header labels, the hints, the status rows — is bright black rather
than the faint attribute. Faint is optional in ECMA-48, and the Linux console and PuTTY
are two of the terminals that do colour perfectly well without implementing it; on those
every one of those rows drew at the weight of ordinary prose.

## `darkwire chat`

Three shapes, decided by how you call it:

```bash
darkwire chat                            # a prompt, with slash commands and Tab completion
darkwire chat "summarise notes.md"       # one turn, then exit
git log --oneline -20 | darkwire chat "what changed"   # a pipe target
```

| Flag                      | Does                                                     |
| ------------------------- | -------------------------------------------------------- |
| `-s, --session <key>`     | A session to continue. Without it, a new one is started. |
| `-a, --agent <id>`        | The agent this session runs on.                          |
| `-m, --model <id>`        | Model id, overriding the configured default.             |
| `-p, --provider <id>`     | Provider instance id, overriding the configured default. |
| `-w, --workspaces <dir>`  | The folder the workspaces live in.                       |
| `-W, --workspace-id <id>` | Which workspace inside it new sessions land in.          |
| `--new`                   | Clear the session before this turn.                      |
| `--json`                  | One agent event per line, as JSON.                       |
| `--no-reasoning`          | Hide the model's reasoning for this run.                 |
| `--no-tools`              | Run the turn with no tools registered at all.            |

**`-w` and `-W` are deliberately different flags** for two different things, and the
capital is the narrower one: `-w` says which folder holds the workspaces, `-W` picks one
inside it. Reaching for the wrong one moves your files rather than switching folder.

`--json` is the scripting surface. Each line is one event from the same stream the web UI
consumes, so a script can watch tool calls go by rather than waiting for prose.

The prompt draws a small live area on the **last few rows of your ordinary screen**.
Everything an exchange finishes with is printed above it and belongs to the terminal from
then on: its scrollback, its wheel, its selection, its search, whatever your accessibility
tools do with a terminal. When you leave, the conversation is still there, above the shell
prompt, the way the output of any other command would be.

That is one decision, and most of the prompt's good behaviour follows from it. There is no
scrollback of our own to be worse than the emulator's, no scroll keys to learn, and nothing
to lose on exit.

The live area holds only what is still changing: the answer as it arrives, the composer,
the plan, the command list, a picker when one is open, and the status rows. It is sized
from its contents every frame and never takes more than half the window, so the
conversation above stays visible. A resize lays it out again at whatever the window now
is, rather than patching it.

**The prompt never takes the alternate screen.** A program that takes the second buffer
owns the whole window, which means implementing scrolling, selection and search itself,
and the session vanishing when it exits.

What does take it is anything opened _over_ the prompt: `ctrl-t`, `/help`, `/context`,
`/memory`, `/session` and `/workspace`. Each is a document or a list read as one, and
those take the second buffer honestly — they are opened, read, and closed, and the
conversation is exactly where it was. The alternative is laying twenty rows over the
prompt, and the live area grows by scrolling the conversation into the terminal's
scrollback, which closing it again cannot undo.

**A resize redraws the window.** The rows above the prompt belong to the terminal, and an
emulator that reflows them when the width changes puts them where the new width says
rather than where they were. So the window is cleared and the end of the conversation
written again at the new width. Without that, dragging a window narrower leaves a copy of
the composer behind for every step of the drag.

**The mouse is left alone.** Nothing asks the terminal for mouse events, so selecting a
line of an answer and copying it works the way it does in every other program, with no
modifier held. The wheel scrolls the conversation because the conversation is the
terminal's.

A pipe gets none of this: a stdout that is not a terminal gets a plain prompt and no
escape sequences at all.

### Slash commands

Type `/` in the prompt. Tab completes a slash command and nothing else — never a
filename, never a session key — so the completion list can never be a guess about what
you meant.

| Command          | What it does                  |
| ---------------- | ----------------------------- |
| `/help`          | This list                     |
| `/clear`         | Forget this session's history |
| `/exit`, `/quit` | Leave                         |

**Sessions**

| Command           | What it does                               |
| ----------------- | ------------------------------------------ |
| `/session [key]`  | Pick one over the prompt, or attach by key |
| `/new [title]`    | Start a fresh session and attach to it     |
| `/rename <title>` | Rename this session                        |
| `/delete [key]`   | Delete one, defaulting to this one         |
| `/branch [ref]`   | Fork up to `<ref>` and attach to the fork  |

A session is written when you say something in it, not when you start one. `/new`,
`/session <key>` and the prompt you get on launch all move you to a name; the row appears
on the first message, in the workspace and on the agent the turn actually ran under. So
opening a prompt, thinking better of it and leaving puts nothing in `/session`, and the
listing holds conversations rather than the debris of changing your mind. `/new <title>`
and `/rename` before that first message hold the name and put it on the row when it is
written, over the one derived from what you asked.

**Messages**

| Command              | What it does                                     |
| -------------------- | ------------------------------------------------ |
| `/edit <ref> <text>` | Replace a message and re-run from there          |
| `/regenerate [ref]`  | Re-run the last turn, or the one `<ref>` started |

Both truncate and re-run rather than appending. History is append-only for the provider's
cache, so what these drop is always a suffix — see
[Architecture](architecture.md#a-turn).

**Context and cost**

| Command    | What it does                               |
| ---------- | ------------------------------------------ |
| `/context` | What the next turn would send to the model |
| `/tasks`   | The plan this session is running on        |

`/context` **opens over the prompt**, with four tabs: the numbers, the system prompt in
full, one row per tool with what it costs, and one row per message in the window. The
numbers are the same measurement the browser's context inspector draws and
`GET /api/sessions/:key/context` returns, so all three agree. The other three tabs are
the follow-up question: the only thing anyone asks after "tools: 4,102" is _which_ tools.

`/tasks` **opens over the prompt** too, one row per task in the markers the prompt uses,
so what it shows is what the model reads. `ctrl-x` drops the one under the cursor and the
list stays up, so three can go in one gesture; escape closes it. A delete acts rather than
asking, because the model rewrites the whole list on its next planning step anyway. If a
turn rewrote the plan while the list was open, nothing is dropped and it says so. The list
belongs to the session, so it survives `/clear` and a restart.

The tokens/s figure divides by the time the model spent generating, not by the
turn's wall clock — so a cold local model that spent thirty seconds loading its
weights reports the speed it actually decodes at rather than a thirtieth of it.
Turns recorded before that was measured, and replies that arrived in a single
frame, still divide by the wall clock and read as they always did. The browser's
turn-info popover breaks the same turn down further, including the wait before
the first token.

**Keys**

| Key                          | What it does                                     |
| ---------------------------- | ------------------------------------------------ |
| `ctrl-g`                     | Every command, searchable                        |
| `/`                          | The command list, filtered as you type           |
| `tab`                        | Take the highlighted command                     |
| `return`                     | Run the highlighted command                      |
| `ctrl-t`                     | Show the whole transcript, or `/transcript`      |
| `ctrl-o`                     | How the next tool's output arrives               |
| `ctrl-y`                     | Whether the next turn's cost is printed          |
| `ctrl-l`                     | Draw the screen again                            |
| `ctrl-c`                     | Stop the turn, or leave                          |
| `ctrl-d`                     | Leave                                            |
| `shift-return`               | A new line in the message, rather than sending   |
| `up`, `down`                 | Between the lines, then the last thing you asked |
| `ctrl-a`, `ctrl-e`           | Start of line, end of line                       |
| `ctrl-b`, `ctrl-f`           | Back, forward                                    |
| `alt-left`, `alt-right`      | Back a word, forward a word                      |
| `ctrl-u`, `ctrl-k`, `ctrl-w` | Clear the line, clear to the end, delete a word  |

**Reasoning and what a turn cost live in the transcript**, in full and whatever
the switches say. That is what `ctrl-t` and `/transcript` are for, and why there
is no `/output` or `/stats` command any more: one place to look, rather than two
commands deciding what the conversation is allowed to carry. `ctrl-o` and
`ctrl-y` still flip what the _next_ turn prints inline.

`/help` takes the window rather than printing into the conversation, with four
tabs: commands, the turn, setup and keys. Left and right move between them, the
arrows and page keys scroll, and escape closes it, and the conversation is
exactly where you left it. It is a document, which is the one thing worth the
second screen buffer: laying twenty rows over the prompt instead would mean
growing the live area, and the live area grows by scrolling the conversation
into the terminal's scrollback, which closing it again cannot undo.

On a pipe, under `--json` or on a dumb terminal there is nowhere to lay an
overlay, so the same table is written to the stream exactly as it always was.

The command list takes the status bar's rows rather than pushing anything down,
and it is the same size whatever you type into it. Both of those are about the
conversation above: the prompt sits at the foot of the ordinary screen, so a
live area that grew would scroll the conversation into the scrollback, and
shrinking it again cannot bring those rows back. A list that is one size and
takes rows that were already there costs the conversation nothing.

`shift-return` puts a new line in the message you are writing; `return` still
sends it. On a terminal too old to tell `return` and `shift-return` apart,
`alt-return` and `ctrl-j` do the same thing and need nothing of the terminal.
While a message has more than one line, `up` and `down` move between them and
reach the history from the ends.

Typing `/` opens the command list **beside** the line rather than over it: what
you typed stays visible and editable while the list filters under it. `ctrl-g`
is the other half of the same idea, for when you do not know the name to start
typing. It opens the whole table, searchable, in place of the prompt.

**The plan**

An agent doing multi-step work writes one with its `todo` tool, and it sits
above the box you type into, three tasks at a time around whatever is in hand
with a `+N more` when the list is longer. It is rewritten rather than appended,
so a turn that revises its plan six times leaves one list on screen and no trail
of older ones. `/tasks` opens it in full, with `ctrl-x` to empty it.

**Agents and models**

| Command            | What it does                                                  |
| ------------------ | ------------------------------------------------------------- |
| `/agent [id]`      | Show agents, or move this session onto one                    |
| `/model [id]`      | Show the models this install can reach, or pick one           |
| `/effort [level]`  | How hard to ask the model to think, or `default` to send none |
| `/temperature [n]` | The sampling temperature, or `default` to send none           |

`/agent`, `/model` and `/effort` open a picker when given no argument — arrow keys move,
typing filters, and the cursor starts on what is in force. `/temperature` does not, and
the asymmetry is the subject rather than an omission: a temperature is a number in a
range, which a list cannot enumerate, so bare `/temperature` answers the question a
picker would have answered by opening. On a pipe or a dumb terminal every one of them
prints the listing instead, marking the current row with `*`.

The last three **edit the agent this session runs on, and save** — the entry under
`agents.list`, whether that is `default` or one you moved onto with `/agent`. There is
no settings layer above an agent, so the agent is the only place these live: the change
follows it everywhere it runs, and it is still there on the next launch.

`/effort` and `/temperature` distinguish `off` from `default`. `off` sends a parameter
asking the model not to think; `default` sends no reasoning parameter at all, which is
the only thing that works against an endpoint that rejects the field. A temperature of
`0` is a value in the same way — it is not an absence.

`/model` refuses under `--model`. That flag is a statement about the process that the
config cannot move, so a `/model` that appeared to work and changed nothing would be
worse than one that will not.

**Memory and skills**

| Command           | What it does                                            |
| ----------------- | ------------------------------------------------------- |
| `/memory`         | What this workspace remembers, and what the index costs |
| `/memory on\|off` | Let this agent remember, or stop it                     |
| `/skills`         | The sheets this workspace holds                         |

`/memory` **opens over the prompt**, with two tabs: the index exactly as every prompt on
this folder carries it, and what it costs. Nothing remembered yet is a sentence instead,
because two empty tabs are a worse answer than one line.

`/memory on|off` **changes the agent, not just this session** — it is the `memory` tool's
permission, which is the one switch rather than two. See [Memory](memory.md).

**Workspaces**

| Command           | What it does                   |
| ----------------- | ------------------------------ |
| `/workspace`      | Manage them, over the prompt   |
| `/workspace <id>` | Switch where new sessions land |

Bare `/workspace` opens the manager: one row per workspace with its id and how many
sessions are in it, which is the number that decides whether a removal will be refused.
Return switches. `ctrl-r` renames it on a line you type into, `ctrl-x` detaches it after
asking, `ctrl-v` sends its sessions to another one, and the `+ new workspace…` row makes
one. The list stays up between them, so three renames are one visit.

**There are no verbs in the command**, which is what makes `/workspace <id>` mean one
thing. The token after `/workspace` is always an id, so a workspace called `new` is
switched to with `/workspace new` and that means exactly what it looks like.

An extension can add commands of its own; they appear in this list and in `/help` from
the same table, so one cannot exist in the completer and not the listing. See
[Extensions](extensions.md).

## `darkwire init`

The terminal half of the first-run wizard: language, workspace, provider, model. The
provider step lists models from the endpoint itself, so on a machine running
`ollama serve` the model question is a list rather than a text box.

It **needs a real terminal** and refuses a pipe rather than reading EOF as an answer, and
it writes nothing until every question has been answered — a wizard abandoned halfway
leaves the install exactly as it was.

## `darkwire serve`

Serves the UI, the REST API and the WebSocket on one port.

| Flag                     | Does                                                            |
| ------------------------ | --------------------------------------------------------------- |
| `-H, --host <host>`      | Bind address, overriding the configured default.                |
| `-P, --port <port>`      | Port, overriding the configured default.                        |
| `-w, --workspaces <dir>` | The folder the workspaces live in, overriding the config.       |
| `--password <password>`  | Set or rotate the login password. Or `DARKWIRE_PASSWORD`.       |
| `--username <username>`  | The login name, alongside `--password`. Or `DARKWIRE_USERNAME`. |
| `--ui <dir>`             | A built UI to serve instead of the bundled one.                 |

It starts with nothing configured and prints a one-time setup code. Two refusals are
worth knowing before you meet them:

- **A non-loopback bind with authentication off refuses to start.** Not a warning — the
  process exits. See [Security](security.md#binding).
- **`--ui <dir>` must contain an `index.html`.** A directory that does not is an error at
  startup rather than a blank page later.

If `@darkwire/web` has not been built, `serve` says so and runs the API alone rather than
serving nothing at a URL it just printed.

## `darkwire agent`

Lists the agents this install is configured with, straight out of `config.yaml`:

```bash
darkwire agent list
```

Each row is the agent id and whether it is enabled, with its label and its delegation
roster under it. It is read-only on purpose. An agent is a system prompt, a set of tool
permissions, an optional environment and a roster of agents it may delegate to, and the
form that validates all of that is the **Agents** tab in the web UI. A flag list would be
a second, thinner way to write the same thing wrong.

## `darkwire environment` and `darkwire extension`

Environment definitions are read-only from the CLI. Authoring one is a form with a dozen
fields and five of them decide what a container may do, which is a screen rather than a
flag list; the **Environments** tab in Settings is where that lives.

```bash
darkwire environment list        # every installed environment definition and its hardening
```

Each entry still carries a digest, and it is identity rather than consent. Two environment
definitions that differ never share a warm instance; editing a definition while a command
is running cancels that command and names the drift; an idle container whose definition
moved is swept.

`extension` is the one that still has three verbs — `list`, `approve <id>`, `revoke <id>`
— because its approval is a record in a store rather than a file an operator edits, and
its digest covers every byte of the install directory rather than a manifest.

See [Environments](environments.md) and [Extensions](extensions.md).

Environment instances are managed through the isolated service:

```bash
darkwire sandbox health
darkwire sandbox list
darkwire sandbox start --environment <id> --workspace <id>  # an id, not a directory
darkwire sandbox stop <instance>
darkwire sandbox restart <instance>
```

`--socket` overrides the service socket. Stop and restart never refuse a busy instance:
the container goes away under whatever it was running and the next command starts a fresh
one. See [Sandbox service](sandbox-service.md).

## Environment

| Variable                    | Does                                                                                                           |
| --------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `DARKWIRE_HOME`             | DarkWire's own state. Beaten by `--home`, beats `~/.darkwire`.                                                 |
| `DARKWIRE_WORKSPACES`       | The workspaces folder. Beaten by `--workspaces`, beats `~/DarkWire/workspaces` and the config. Empty is unset. |
| `DARKWIRE_PASSWORD`         | Fallback for `serve --password`.                                                                               |
| `DARKWIRE_USERNAME`         | Fallback for `serve --username`.                                                                               |
| `DARKWIRE_LANG`             | Locale. Ranks above `config.ui.locale`, which ranks above `LANG`.                                              |
| `DARKWIRE_LOG_LEVEL`        | Then `LOG_LEVEL`, then `info`.                                                                                 |
| `DARKWIRE_DEBUG`            | Any non-empty value prints stack traces instead of the sentence.                                               |
| `DARKWIRE_SANDBOX_SOCKET`   | The sandbox service socket. Naming one stops `serve` starting its own.                                         |
| `DARKWIRE_DATA_DIR`         | `darkwire-environment serve-env`: the absolute host path the daemon sees.                                      |
| `DARKWIRE_CONTAINER_ENGINE` | `docker` or `podman`. Defaults to `docker`.                                                                    |
| `DARKWIRE_GATEWAY_IMAGE`    | The egress gateway image, needed for `allowlist` egress.                                                       |
| `DARKWIRE_ENVIRONMENTS`     | `serve-env`: environments to register. Defaults to `dev`.                                                      |

Provider API keys are read from the environment **only when the vault has no entry** for
that instance — the vault wins. [Configuration](configuration.md#environment-variables)
has the full list and [Providers](providers.md) explains the precedence.

## Exit codes

`darkwire` sets `process.exitCode` and returns rather than calling `process.exit`, so a
piped answer is never truncated by the process leaving before its output has flushed.
`--help` and `--version` are successful exits.
