# Skills

A skill is an instruction sheet the agent can reach when it needs it. It lives in the
workspace, so it is a folder you commit beside the project it describes rather than
something an install carries in its settings.

Every skill's name and description reach the prompt on every turn — about twenty tokens
each. The instructions themselves stay on disk until the agent decides the skill applies
and opens the file with `read_file`.

## The folder

```
<workspace>/skills/
  code-review/
    SKILL.md          ← required
    checklist.md      ← whatever else the skill needs
  release-notes/
    SKILL.md
```

`SKILL.md` is frontmatter and a body:

```markdown
---
name: code-review
description: Review a diff for correctness, then for style. Use before opening a PR.
---

Read the diff with `exec git diff`. Take correctness first: off-by-ones, error paths
that swallow, anything that changes behaviour the tests do not cover. Then style —
and only against `checklist.md`, not against your own taste.
```

**A skill is a directory, not a file**, and that is the reason: the directory is where
the skill's own material goes. `checklist.md` above is an ordinary workspace file, so the
agent reads it with the tool it already has, and it costs nothing until it is read.

Three rules, each of which fails to that skill alone rather than to the turn:

- **The directory name is the skill's id.** It is what appears in the path the model is
  given, so it is the one identifier that cannot disagree with anything. A frontmatter
  `name` saying something else is a warning in the log, and the directory wins.
- **`description` is required.** It is the entire basis on which the agent decides to
  open the file. A skill without one is not advertised at all — an index line reading
  `**deploy**:` teaches the model that the skill is about nothing.
- **A malformed skill is skipped, never fatal.** A missing `SKILL.md`, an unclosed
  frontmatter fence, a description that is only whitespace: each costs one skill and one
  `warn` line. A folder in a workspace is whatever a person or a previous turn left there,
  and a turn that refuses to start because of it is a worse outcome than one that runs
  with four skills instead of five.

### The frontmatter is not YAML

It is `key: value` lines between two `---` fences, with one optional pair of surrounding
quotes stripped. Blank lines and `#` comments are skipped, and so is anything that is not
`key: value` — a `tags:` list left in the block does not refuse the file, it is simply not
read.

That is a deliberate stopping point rather than a subset on the way to something. Nothing
else in this repository parses YAML, and what a skill's frontmatter holds is three strings.
See the header of `crates/core/src/frontmatter.rs`.

### Which agents see a sheet

An optional `agents:` line narrows a sheet to the agents it names. Without one — which is
every sheet written before this existed — it is in every agent's catalogue.

```
---
description: How this repository refactors.
agents: coder, team-lead
---
```

Scope is declared in the file rather than by where the file sits, so a sheet stays in
`skills/` beside the others and stays something a person can list, read and commit. It is
a property of the **catalogue**: it decides which sheets an agent is _told about_, not
which it may open. `read_file` and the `skill` tool will still open a sheet that was never
advertised, and that is not a hole — a skill is prose, and the jail and the exec guard have
never read a word of the prompt.

**Comma-separated is the only form**, and that is a property of the reader rather than a
preference. The frontmatter parser anchors a field on a letter, so a `- coder` block item
never matches — it is discarded, and a YAML block list arrives here indistinguishable from
an empty `agents:`. One concession is made to habit: a surrounding `[coder, writer]` is
accepted, because that is the other thing somebody writes. Ids are lower-cased, trimmed and
de-duplicated.

**It fails open.** An `agents:` line that yields no usable id leaves the sheet visible to
every agent, and logs a warning. The two ways of being wrong are not symmetric: a sheet
shown too widely costs prompt that `/skills` will show you, and a sheet hidden from
everybody is one that silently stopped working with nothing anywhere to find. An id that is
not a legal agent id is dropped with a warning naming it; if that leaves nothing, the whole
line falls open.

A sheet can also be scoped by an agent that installs it — see
[Sheets a preset brings](#sheets-a-preset-brings).

## What reaches the prompt

In the **static** half — the part a provider caches for the life of a session:

```
## Skills

Instruction sheets kept in this workspace under `skills/`. A line below is a
summary, not the skill — open the file with `read_file` before acting on what it names.

- `skills/code-review/SKILL.md` — **code-review**: Review a diff for correctness, then style.
- `skills/release-notes/SKILL.md` — **release-notes**: Draft release notes from a git range.
```

Every skill is here, and none of their bodies. Skills are sorted by name, because the
section sits in the provider's cached prefix and a `readdir` order that varied between
hosts would move that prefix for no visible reason.

In the **runtime** half — the trailing block rebuilt on every iteration and never cached —
whatever this message named:

```
### Skill: code-review

Read the diff with `exec git diff`. Take correctness first: …
```

A named skill therefore appears twice: as its index line above and as its body here. That
is deliberate and it costs about twenty tokens. Dropping the line would mean varying the
cached half with the message, which is the expense the two halves exist to avoid — and it
would cost the whole prefix rather than a line of it.

There **is** a `skill` tool, and it exists for a reason the argument against it never
addressed.

The argument against was good, and it was about reading: a tool whose whole job is to
return the bytes of a workspace file is a worse `read_file` — one more name in every
agent's permission map and one more schema in every request, to reach a file the agent can
already open. That is still true, and it is still what [Tools](tools.md) opens by saying.

What it did not cover is the **permission**. A tool carries `allow`, `ask` or `deny` per
agent, already in the config and already in the settings UI, and that is exactly "this
capability is on, off, or gated". Without a `skill` tool there is no way to turn skills off
for one agent short of adding a `skillsEnabled` boolean beside the permission map — a
second switch for one thing, and the way the two come to disagree.

So the cost the old argument names is real and is now paid deliberately. What it buys:
denying `skill` removes the catalogue from the prompt as well as the tool, and the whole
feature has one switch rather than two. The same reasoning put a `memory` tool beside it;
see [Memory](memory.md).

Two consequences worth knowing. **On an existing install nothing is indexed until the tool
is granted** — `DEFAULT_AGENT_TOOLS` seeds a newly created agent, so a config that predates
this has no `skill` key, and an absent tool is a denied one. And the model can still reach
a sheet with `read_file`: the tool is the intended path, not the only one.

The tool does **not** enforce `agents:`, and that follows from the same sentence. Scope
decides what an agent is told about; a second copy of the rule inside the tool would gate
one door while `read_file` walks past the other, which buys nothing and gives two rules the
chance to disagree.

### Seeing what a workspace holds

The catalogue is in the agent's prompt, not on your screen. `/skills` prints it — the name
and description of every sheet the workspace holds — in `ghostai chat` and from the Telegram
bot. It is a listing and nothing more; it is gated on the same `skill` permission, because
a catalogue this agent cannot open is not worth printing.

It lists **every** sheet, marking the ones scoped to other agents rather than dropping
them. Somebody runs `/skills` precisely when a sheet is not working, and a listing that
hid it would leave nowhere to find out why.

## The budget

| Cap               | Value          | What it bounds                                            |
| ----------------- | -------------- | --------------------------------------------------------- |
| `SKILL_MAX_BYTES` | 12 KB          | One body. Past it, the body is cut and says that it was.  |
| `MAX_SKILLS`      | 100            | Index lines. Past it, the rest are not advertised at all. |
| description       | 200 characters | One index line stays one line.                            |

`MAX_SKILLS` is applied **before** scope, and that order is deliberate: the cap bounds the
per-turn read, and applying it after would mean opening a thousand directories to find the
twelve one agent sees. Past a hundred directories, then, which sheets an agent sees follows
alphabetical order rather than who they are for.

These are constants rather than settings. They exist to stop a workspace that has
accumulated files quietly costing five figures of prompt on every turn, and that hazard
does not vary by install the way a taste for long prompts does.

## Where these live, and what it costs

They are in the workspace, which is inside the jail, which means `write_file` and `exec`
can both edit them. That is worth stating plainly rather than leaving to be discovered,
because there is a real argument for the opposite: a directory _beside_ the workspace
would be one prompt injection could not reach, and so could not use to rewrite what an
agent believes.

`crates/core/src/paths.rs` used to reserve `~/.ghostai/agents/<id>/` for exactly that.
Nothing ever wrote to it. [Memory](memory.md) declined it, skills declined it, and
per-agent skill scope declined it too — each on the same grounds, and each time because
both are meant to be read, reviewed and committed beside the project they describe, and
neither is that if it lives somewhere the agent cannot list. After a third pass the
reservation was removed rather than kept for a fourth.

The same attack exists here: a turn that is talked into writing `skills/deploy/SKILL.md`
changes what a later turn on that workspace is told. What buys the risk is the thing that
makes skills useful at all — a skill folder is meant to be read, reviewed and committed
beside the project it describes, and a directory the agent cannot see in a listing is not
that. A skill is also not a capability: it is prose, and the jail and the exec guard have
never read a word of the prompt. Deleting or forging one changes what the agent _knows_,
not what it _can do_ — the argument [Prompts](prompts.md) makes about the system prompt,
which skills are now part of.

What follows from the placement, in code:

- **A symlinked skill directory is skipped.** The scan asks a directory entry for its own
  file type rather than asking whether the path is a directory: the first does not follow
  a symlink and the second does, so a link pointing out of the workspace is never
  followed. There is a test asserting it, because the difference is one method call that
  nobody would notice being changed back.
- **Every body is bounded** before it reaches the prompt, so a 400 MB file dropped into
  `skills/` is 12 KB of prompt and a log line.
- **Nothing is written.** The loader only reads.

If you want the injection-proof arrangement instead, keep the workspace's `skills/` out of
the agent's write permissions: an agent whose `write_file` is `deny` or `ask` reads its
skills and cannot author them.

## The section is a template you own

`agents.list.<id>.skillsPrompt` holds the heading and the prose, on the contract the
other seven keep: empty inherits `DEFAULT_SKILLS_TEMPLATE`, a single space deletes the
section, anything else is this agent's own. It is edited under **Advanced prompt
settings** in the agent editor. See [Prompts](prompts.md).

What stays in code is the _shape_ of the index line, because that is what the catalogue and
`read_file` agree on, not prose. `{{index}}` carries its own leading blank line, so a
template that places it straight after its prose leaves no gap when there is nothing to
list.

The template has no placeholder for a body. A sheet is never templated into the prompt at
all — the agent opens the file the index names, long after the section is rendered.

Whether the section is placed at all is the `skill` tool's permission, not this template.
Denying it removes both.

So does `toolsEnabled: false`, which is broader: with no tool list advertised there is
nothing to open a sheet _with_, and a catalogue of paths plus prose naming `read_file` is
cost the model cannot act on. Half a section whose own wording points at a tool that is not
there is worse than no section, and an agent with no tools and a fixed instruction sheet is
what `systemPrompt` is for.

## Sheets a preset brings

An agent preset may name sheets to copy in, as directory names under the catalogue's
`skills/`:

```json
{ "schema": "ghostai.agent-preset/1", "id": "coder", "skills": ["code-review"] }
```

`ghostai preset install coder` then writes `<workspace>/skills/code-review/` and says so.
`ghostai agent install` does the same when a catalogue is reachable; it never fetches one,
so on a box that has not run `ghostai preset update` it reports the sheets as missing and
installs the agent anyway.

Four things about it, each of them a decision rather than an omission:

- **It is a copy, not an install.** Byte for byte, with nothing rewritten and nothing
  recorded. The sheet carries its own `agents:` line, so what an operator reads in the
  catalogue is what lands in the workspace, and editing it afterwards is editing their own
  file. If a sheet's `agents:` does not include the preset that brought it, the install
  says so — that is legal, and usually a mistake.
- **There is no hash.** A container definition carries one because its bytes decide a
  boundary; a sheet is prose, and the preset's own `systemPrompt` — from the same
  catalogue, over the same network — already sets the bar. Running the command at a
  terminal is the operator action. See [Environments](environments.md).
- **Nothing refuses.** A sheet that is missing, symlinked, or over the copier's bounds
  costs that sheet and a line in the report. A missing _container_ refuses, because the
  server would refuse to boot on an agent whose placement does not exist; an agent with
  one fewer index line boots fine.
- **A sheet already in the workspace is left alone** unless `--force` is passed, which is
  the same rule the `agents.list` entry follows and for the same reason: it may carry your
  edits. `--force` overwrites file by file rather than emptying the directory first, so
  anything you added inside a sheet folder survives.

Sheets live in a workspace and a preset does not, so `-W, --workspace-id <id>` says which
one. It defaults to `default`, and a named workspace has to exist already — the id is
validated for shape but the registry lives in SQLite, so without that check a typo would
create a tree no UI ever lists.

## What is not built yet

- **No settings panel for the skills themselves.** They are authored by writing files, and
  no screen is planned: a skill is a folder committed beside the project rather than
  configuration, so no settings screen lists one as coming.
- **Nothing uninstalls a sheet.** A preset copies files in; removing the agent leaves them,
  because after the copy they are ordinary workspace files that may have been edited. Delete
  the folder.
