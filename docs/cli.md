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
| `-s, --session <key>`     | The session to continue. Default `cli:default`.          |
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

The prompt takes the window on the way in by scrolling it, not by clearing it: whatever
your shell had printed goes into the scrollback, one scroll up, and the composer lands on
the bottom row and stays there.

What the program draws after that is a strip at the bottom of the window: the composer,
the status rows, the menu when one is open, and the run a turn currently has open.
Everything else has been **printed**. The conversation belongs to the terminal, which
means it rewraps itself when you resize the window, you can select and copy it with the
mouse, and your emulator's own search can find it. Nothing redraws it, so nothing can
duplicate it.

A pipe gets none of this: a stdout that is not a terminal gets a plain prompt and no
escape sequences at all.

### Slash commands

Type `/` in the prompt. Tab completes a slash command and nothing else — never a
filename, never a session key — so the completion list can never be a guess about what
you meant.

| Command          | What it does                                |
| ---------------- | ------------------------------------------- |
| `/help`          | This list                                   |
| `/messages [n]`  | The last n messages, with their seq numbers |
| `/clear`         | Forget this session's history               |
| `/exit`, `/quit` | Leave                                       |

**Sessions**

| Command           | What it does                              |
| ----------------- | ----------------------------------------- |
| `/sessions [n]`   | Pick a session to continue, or list them  |
| `/new [title]`    | Start a fresh session and attach to it    |
| `/session [key]`  | Show this session, or attach to another   |
| `/rename <title>` | Rename this session                       |
| `/delete [key]`   | Delete one, defaulting to this one        |
| `/branch [ref]`   | Fork up to `<ref>` and attach to the fork |

**Messages**

| Command              | What it does                                     |
| -------------------- | ------------------------------------------------ |
| `/edit <ref> <text>` | Replace a message and re-run from there          |
| `/regenerate [ref]`  | Re-run the last turn, or the one `<ref>` started |

Both truncate and re-run rather than appending. History is append-only for the provider's
cache, so what these drop is always a suffix — see
[Architecture](architecture.md#a-turn).

**Context and cost**

| Command        | What it does                                    |
| -------------- | ----------------------------------------------- |
| `/context`     | What the next turn would send to the model      |
| `/tasks`       | The plan this session is running on             |
| `/tasks clear` | Empties the list                                |
| `/stats [n]`   | The last n turns: model, tokens, tokens/s, time |

`/context` prints the same measurement the browser's context inspector draws and
`GET /api/sessions/:key/context` returns, so all three agree.

`/tasks` prints the list the agent's [`todo`](tools.md#todo) tool writes, in the markers
the prompt uses, so what it shows is what the model reads. The list belongs to the
session, so it survives `/clear` and a restart.

The tokens/s figure divides by the time the model spent generating, not by the
turn's wall clock — so a cold local model that spent thirty seconds loading its
weights reports the speed it actually decodes at rather than a thirtieth of it.
Turns recorded before that was measured, and replies that arrived in a single
frame, still divide by the wall clock and read as they always did. The browser's
turn-info popover breaks the same turn down further, including the wait before
the first token.

**What a turn shows**

| Command                     | What it does                            |
| --------------------------- | --------------------------------------- |
| `/output`                   | What a turn shows, and what it does not |
| `/output <field> [on\|off]` | Flip one — `reasoning`, `stats`         |

Reasoning and tool output arrive **folded**: one labelled row each, which opens
on a keystroke. A turn is read for its answer, and a terminal that prints every
line of the working out buries the one thing you came for.

The fold that is still running counts up (`⠋ thinking… 4s`), so a long run of
reasoning never reads as a terminal that has stopped.

`ctrl-t` and `ctrl-o` flip them. Each does two things: it opens or closes the run
that is still open, and it decides how the next one arrives. The second half is
the one that matters, because a run is printed in the state it was in when it
finished, and printed text belongs to the terminal. Press the key once and
everything after it arrives the way you asked.

A run too long to sit above the composer is printed before it finishes and loses
its fold with the rest. A tool that prints ten thousand lines would otherwise
hold the window on a promise you could still change your mind about.

`ui.reasoning` in the config file is where that choice lives across runs. It has
three values rather than two: `collapsed` is the default, `expanded` prints it as
it streams, and `hidden` stops it reaching the terminal at all. `--no-reasoning`
means `hidden` for one run. `ui.expandToolOutput` is the switch for the other
half, and it is a switch because tool output is written either way.

What a turn cost is the third of these, and it arrives hidden rather than
summarised. `· 2 steps · 3.9k in / 134 out · 2.5s · 68.9 tok/s` is worth having
and is not worth a row under every answer, and unlike the other two there is
nothing to promise: it is on screen or it is not. `ctrl-y` shows it, and so does
`/output stats on`, which is the same switch spelled twice. `ui.expandTurnStats`
is where the choice lives across runs.

On a pipe, on a dumb terminal or under `--json` there is nothing to fold, so
`collapsed` prints as it always did, only `hidden` silences anything, and a
turn's cost prints when `ui.expandTurnStats` is on.

**Keys**

| Key                          | What it does                                    |
| ---------------------------- | ----------------------------------------------- |
| `ctrl-g`                     | Every command, searchable                       |
| `/`                          | The command list, filtered as you type          |
| `tab`                        | Take the highlighted command                    |
| `return`                     | Run the highlighted command                     |
| `ctrl-t`                     | Fold or unfold the reasoning                    |
| `ctrl-o`                     | Fold or unfold what tools printed               |
| `ctrl-y`                     | Show or hide what the turn cost                 |
| `ctrl-l`                     | Draw the screen again                           |
| `ctrl-c`                     | Stop the turn, or leave                         |
| `ctrl-d`                     | Leave                                           |
| `up`, `down`                 | The last thing you asked, and the one before    |
| `ctrl-a`, `ctrl-e`           | Start of line, end of line                      |
| `ctrl-b`, `ctrl-f`           | Back, forward                                   |
| `alt-left`, `alt-right`      | Back a word, forward a word                     |
| `ctrl-u`, `ctrl-k`, `ctrl-w` | Clear the line, clear to the end, delete a word |

`/help` opens over the prompt rather than printing into the conversation, with
four tabs: commands, the turn, setup and keys. Left and right move between them,
the arrows and page keys scroll, and escape closes it. So the keys are
discoverable from inside the prompt rather than only from here, and reading them
does not leave sixty rows of reference in your scrollback between the question
and the answer to it.

On a pipe, under `--json` or on a dumb terminal there is nowhere to lay an
overlay, so the same table is written to the stream exactly as it always was.

Typing `/` opens the command list **beside** the line rather than over it: what
you typed stays visible and editable while the list filters under it. `ctrl-g`
is the other half of the same idea, for when you do not know the name to start
typing. It opens the whole table, searchable, in place of the prompt.

**The plan**

An agent doing multi-step work writes one with its `todo` tool, and it sits
above the box you type into, three tasks at a time around whatever is in hand
with a `+N more` when the list is longer. It is rewritten rather than appended,
so a turn that revises its plan six times leaves one list on screen and nothing
in the scrollback. `/tasks` prints it in full and `/tasks clear` empties it.

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

| Command           | What it does                                                     |
| ----------------- | ---------------------------------------------------------------- |
| `/memory`         | How many memories this workspace holds, and what the index costs |
| `/memory on\|off` | Let this agent remember, or stop it                              |
| `/skills`         | The sheets this workspace holds                                  |

`/memory on|off` **changes the agent, not just this session** — it is the `memory` tool's
permission, which is the one switch rather than two. See [Memory](memory.md).

**Workspaces**

| Command                         | What it does                              |
| ------------------------------- | ----------------------------------------- |
| `/workspaces`                   | List them, marking the current one        |
| `/workspace <id>`               | Show or switch where new sessions land    |
| `/workspace new <name>`         | Create one                                |
| `/workspace rename <id> <name>` | Rename the label, without moving anything |
| `/workspace rm <id>`            | Detach; refuses while sessions name it    |
| `/workspace move <from> <to>`   | Move sessions between workspaces          |

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
