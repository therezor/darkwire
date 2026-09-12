# CLI

**Who this is for:** anyone driving GhostAI from a terminal rather than a browser. It is
the reference for `ghostai` — every command, every flag, and every slash command inside the
chat prompt. If you are setting up for the first time, start with
[Getting started](getting-started.md) and come back here.

One binary, `ghostai`. The terminal and the browser are two views of one install, not two
programs: they share a single `ghost.db`, so a session you start here is the row the
browser sidebar lists, and a turn started from either goes through the same loop and the
same approval gate.

## Commands

```
ghost [chat] [message...]      talk to the agent — the default command
ghostai init                     configure this install, in a wizard
ghostai serve                    serve the web UI and the API on one port
ghostai preset    list | install [ids...] | update
ghostai agent     install <name-or-path> [--force] | list
ghostai toolbox   list | approve <id> | revoke <id>
ghostai extension list | approve <id> | revoke <id>
ghostai help [command]
```

`chat` is the default, so `ghost "what changed today"` and `ghostai chat "what changed
today"` are the same command.

### Global flags

| Flag                  | Does                                                               |
| --------------------- | ------------------------------------------------------------------ |
| `--home <dir>`        | The GhostAI root. Beats `$GHOSTAI_HOME`, which beats `~/.ghostai`. |
| `--log-level <level>` | `trace`, `debug`, `info`, `warn`, `error` or `fatal`.              |
| `--verbose`           | Report what the install is doing, not only the answer.             |
| `--no-color`          | Disable colour. Rarely needed — see below.                         |
| `-v`, `--version`     | Print the version.                                                 |
| `-h`, `--help`        | Print help. `ghostai help <command>` does the same for one.        |

The log level defaults to `error` — or `info` while serving, because a server that says
nothing while it works reads as hung.

Colour is detected rather than assumed: `NO_COLOR`, `FORCE_COLOR`, `TERM=dumb` and
"stdout is a file rather than a terminal" are all honoured, so `ghostai chat > log`
writes prose and not escape codes. `--no-color` is the override for the case detection
gets wrong.

Secondary text — the header labels, the hints, the status rows — is bright black rather
than the faint attribute. Faint is optional in ECMA-48, and the Linux console and PuTTY
are two of the terminals that do colour perfectly well without implementing it; on those
every one of those rows drew at the weight of ordinary prose.

## `ghostai chat`

Three shapes, decided by how you call it:

```bash
ghostai chat                            # a prompt, with slash commands and Tab completion
ghostai chat "summarise notes.md"       # one turn, then exit
git log --oneline -20 | ghostai chat "what changed"   # a pipe target
```

| Flag                      | Does                                                     |
| ------------------------- | -------------------------------------------------------- |
| `-s, --session <key>`     | The session to continue. Default `cli:default`.          |
| `-a, --agent <id>`        | The agent this session runs on.                          |
| `-m, --model <id>`        | Model id, overriding the configured default.             |
| `-p, --provider <id>`     | Provider instance id, overriding the configured default. |
| `-w, --workspace <dir>`   | The workspace **root** — moves the whole tree.           |
| `-W, --workspace-id <id>` | Which workspace inside that root new sessions land in.   |
| `--new`                   | Clear the session before this turn.                      |
| `--json`                  | One agent event per line, as JSON.                       |
| `--no-reasoning`          | Hide the model's reasoning stream.                       |
| `--no-tools`              | Run the turn with no tools registered at all.            |

**`-w` and `-W` are deliberately different flags** for two different things, and the
capital is the narrower one: `-w` says where the whole tree lives, `-W` picks a workspace
inside it. Reaching for the wrong one moves your files rather than switching folder.

`--json` is the scripting surface. Each line is one event from the same stream the web UI
consumes, so a script can watch tool calls go by rather than waiting for prose.

The prompt takes the window on the way in: the frame is drawn from the top of a cleared
screen rather than from wherever the shell left the cursor. It clears the screen only —
whatever your shell had printed is still in the scrollback, one scroll up. A pipe gets
none of this: a stdout that is not a terminal gets a plain prompt and no escape sequences
at all.

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

| Command      | What it does                                    |
| ------------ | ----------------------------------------------- |
| `/context`   | What the next turn would send to the model      |
| `/stats [n]` | The last n turns: model, tokens, tokens/s, time |

`/context` prints the same measurement the browser's context inspector draws and
`GET /api/sessions/:key/context` returns, so all three agree.

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

## `ghostai init`

The terminal half of the first-run wizard: language, workspace, provider, model. The
provider step lists models from the endpoint itself, so on a machine running
`ollama serve` the model question is a list rather than a text box.

It **needs a real terminal** and refuses a pipe rather than reading EOF as an answer, and
it writes nothing until every question has been answered — a wizard abandoned halfway
leaves the install exactly as it was.

## `ghostai serve`

Serves the UI, the REST API and the WebSocket on one port.

| Flag                    | Does                                                           |
| ----------------------- | -------------------------------------------------------------- |
| `-H, --host <host>`     | Bind address, overriding the configured default.               |
| `-P, --port <port>`     | Port, overriding the configured default.                       |
| `-w, --workspace <dir>` | Workspace root, overriding the configured default.             |
| `--password <password>` | Set or rotate the login password. Or `GHOSTAI_PASSWORD`.       |
| `--username <username>` | The login name, alongside `--password`. Or `GHOSTAI_USERNAME`. |
| `--ui <dir>`            | A built UI to serve instead of the bundled one.                |

It starts with nothing configured and prints a one-time setup code. Two refusals are
worth knowing before you meet them:

- **A non-loopback bind with authentication off refuses to start.** Not a warning — the
  process exits. See [Security](security.md#binding).
- **`--ui <dir>` must contain an `index.html`.** A directory that does not is an error at
  startup rather than a blank page later.

If `@ghostwire/web` has not been built, `serve` says so and runs the API alone rather than
serving nothing at a URL it just printed.

## `ghostai preset`

Picks agents out of the catalogue, and builds the containers those particular agents
need. This is the command that puts a working team on a fresh machine:

```bash
ghostai preset install                  # pick from a list
ghostai preset install coder nano       # or name them, for a script
ghostai preset list                     # what is on offer, and what is installed
ghostai preset update                   # fetch the catalogue again
```

The catalogue is `@ghostwire/presets`, published from the
[`GhostAI-presets`](https://github.com/therezor/GhostAI-presets) repository and versioned
on its own cadence. It is **fetched on demand** into `~/.ghostai/catalogue` — an npm
prefix, so the package itself lands at
`~/.ghostai/catalogue/node_modules/@ghostwire/catalogue`. Nothing is fetched at turn time
and nothing is fetched twice: a copy already there is used until `--refresh` says
otherwise.

| Flag           | Does                                                             |
| -------------- | ---------------------------------------------------------------- |
| `--from <dir>` | Read a checkout of the presets repo instead. Never fetched over. |
| `--refresh`    | Fetch again before reading, even when a copy is here.            |
| `--offline`    | Never fetch. Fails rather than reaching a registry.              |
| `--force`      | Overwrite agents of the same id, which may carry your edits.     |
| `--approve`    | Approve the toolboxes this installs, without asking.             |
| `--no-approve` | Approve nothing, and print the `ghostai toolbox approve` lines.  |

`install` also takes `-W, --workspace-id <id>`, which says where a preset's skill sheets
are copied. It defaults to `default`, and a named workspace has to exist already. Note
that it is `--workspace-id`, not `--workspace`: on `chat` and `serve` that name already
means a _directory_.

**Installing an agent may also write skill sheets.** A preset can name sheets in the
catalogue's `skills/`, and they are copied into the workspace's — the report lists what it
wrote, what it left alone because you may have edited it, and anything a preset named that
this catalogue does not carry. Nothing there refuses: a missing sheet costs one index line
in that agent's prompt. See [Skills](skills.md#sheets-a-preset-brings).

**A toolbox is built because an agent asked for it, never on its own.** You tick agents;
the containers fall out of `toolbox.name` on the ones you ticked. Picking only agents
that need no container is how you install without Docker — there is no flag for it,
because the checkbox already is one.

**Approving is a separate decision, and the policy is printed before the question.**
Building an image and installing its manifest are reversible; approving one is a
statement that you read what that container may do — its network ceiling, the
capabilities it adds back, the hardening it switches off. So the run prints all of that
and then asks, and a run with nobody to ask — a pipe, a CI job — approves nothing and
prints the commands instead. Passing `--approve` is how a script says yes.

`--from` is what a preset author uses:

```bash
git clone https://github.com/therezor/GhostAI-presets
ghostai preset install --from ./GhostAI-presets
```

It is never fetched over. Pointing it at a typo fails rather than quietly using the
registry copy, because a preset installed from somewhere other than where you edited it
is a preset you have not tested.

## `ghostai agent`

Installs one agent preset by id or by path — the single-shot `ghostai preset install` is
built on, and what a script wants when it knows the name. It never touches Docker and
never fetches: use `ghostai preset install` when the agent needs a container that is not
built yet.

A preset is a JSON file holding a system prompt, tool permissions, a toolbox reference
and a delegation roster, and installing it writes one entry in `agents.list`:

```bash
ghostai agent list                      # configured agents, and presets not yet installed
ghostai agent install researcher        # a catalogue preset, by its id
ghostai agent install ./my-agent.json   # a preset you wrote, by path
ghostai agent install nano --force      # overwrite an existing agent of the same id
ghostai agent install coder -W acme     # put its skill sheets in the acme workspace
```

It copies a preset's skill sheets too, when a catalogue is already on the machine. It
never fetches one, so on a box that has not run `ghostai preset update` every sheet a
preset names is reported as missing and the agent installs regardless.

**There is one kind of preset.** A preset is `<id>.json` — the filename is the agent id
— whether or not the agent works in a container; one that does simply sets
`toolbox.name`. So there is one lookup, and the argument is either a path or an id
searched in two directories:

| Searched                       | Holds                                              |
| ------------------------------ | -------------------------------------------------- |
| `~/.ghostai/presets/<id>.json` | Yours. Drop a file in; that is the install.        |
| `<catalogue>/agents/<id>.json` | The catalogue's, once `ghostai preset` fetched it. |

Yours first, so a local preset wins over a catalogue one of the same name. Nothing is
fetched _here_: both are files already on the box by the time this command runs, and a
machine with no catalogue installs only your own presets rather than failing.

Installing is a config merge and nothing more: afterwards the agent is ordinary config,
edited in the web UI like any other. Three rules do the real work:

- A preset naming a toolbox that is not installed and approved is **refused**, with the
  command that fixes it — the server would refuse to boot on the result.
- An id that already exists is **refused without `--force`**, because the existing entry
  may carry your own edits.
- A preset's `subagents` roster is **filtered to the agents installed and enabled at
  that moment**. Install `team-lead` last — or re-run it with `--force` after adding
  specialists — and its delegation roster matches what can actually answer.

A preset deliberately cannot name a model, a provider, or anything from the toolbox
manifest's side of the security boundary. See
[Toolboxes](toolboxes.md#agent-presets).

## `ghostai toolbox` and `ghostai extension`

Both are the same three verbs over a digest-based approval:

```bash
ghostai toolbox list            # every installed toolbox, and whether it is approved
ghostai toolbox approve <id>    # approve its current contents
ghostai toolbox revoke <id>     # stays installed, stops running
```

Approval records the sha256 of the exact bytes you reviewed, so **editing an installed
toolbox or extension revokes its own approval** — the next turn refuses and names the
drift rather than running code nobody looked at. `extension` differs in one way that
matters: its digest covers every byte of the install directory rather than the manifest,
because an extension manifest names a path where a toolbox manifest pins an image.

See [Toolboxes](toolboxes.md) and [Extensions](extensions.md).

## Environment

| Variable            | Does                                                              |
| ------------------- | ----------------------------------------------------------------- |
| `GHOSTAI_HOME`      | The root. Beaten by `--home`, beats `~/.ghostai`.                 |
| `GHOSTAI_PASSWORD`  | Fallback for `serve --password`.                                  |
| `GHOSTAI_USERNAME`  | Fallback for `serve --username`.                                  |
| `GHOSTAI_LANG`      | Locale. Ranks above `config.ui.locale`, which ranks above `LANG`. |
| `GHOSTAI_LOG_LEVEL` | Then `LOG_LEVEL`, then `info`.                                    |
| `GHOSTAI_DEBUG`     | Any non-empty value prints stack traces instead of the sentence.  |

Provider API keys are read from the environment **only when the vault has no entry** for
that instance — the vault wins. [Configuration](configuration.md#environment-variables)
has the full list and [Providers](providers.md) explains the precedence.

## Exit codes

`ghostai` sets `process.exitCode` and returns rather than calling `process.exit`, so a
piped answer is never truncated by the process leaving before its output has flushed.
`--help` and `--version` are successful exits.
