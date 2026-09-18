# Memory

An agent's memory is a folder of plain markdown files in the workspace, one per memory,
under `memory/`. An index of them goes into every prompt on that folder; the contents
stay on disk until the model asks for one. The agent reads, writes and removes them with
the `memory` tool, and a person edits them with an editor. There is no database, no
embedding and nothing to migrate: a memory you can read is one you can correct.

> **On an existing install, nothing is remembered until you grant the tool.**
> `DEFAULT_AGENT_TOOLS` seeds a _newly created_ agent, so an install that predates this
> feature has no `memory` key in its permission map, and an absent tool is a denied one.
> Grant it in Settings → Agents, or run `/memory on`. This applies to `skill` too.

## Why one file per memory

The shape is the whole design, and two properties follow from it that a single
accumulating file cannot have:

- **Nothing is re-sent to be re-read.** Only the index lines reach the prompt, so a
  memory costs a line until something opens it. The alternative is everything ever
  learned costing its full length on every request of every turn, whichever one the
  question is about.
- **A memory can be corrected.** Saving a key that exists replaces it, so something that
  has changed does not become two contradictory lines with nothing to say which is
  current. The failure mode of a wrong memory is every future turn on that folder.

## The switch is the tool

There is no `memoryEnabled` config key, deliberately. A tool already carries `allow`,
`ask` or `deny` per agent, already appears in the settings UI, and already lives in
`config.yaml`, which is exactly "this capability is on, off, or gated". A boolean beside
it would be a second way to say the same thing, and two switches for one thing is how
they come to disagree.

**There is a switch on the screen, and it writes that permission.** The agent editor's
_Memory and skills_ section carries `Remember across sessions` and `Use the workspace's
skills`, and each one sets `tools.memory` / `tools.skill` to `allow` or `deny`. It is the
same value the Tools table below shows, and the row moves when the switch does. What the
switch buys is that the row does not read as a feature: `memory` sits in an alphabetical
list beside `read` and `exec`, where turning it off looks like denying one call rather
than switching off the whole capability.

Denying `memory` removes the prompt section as well as the tool. That gating is in
`crates/runtime/src/runtime.rs`, and it is what makes one switch enough. An agent that
cannot read or correct its memory should not still be paying to be told what it knows.

**`toolsEnabled: false` removes it too**, and that is the broader condition. Off, the
request advertises no tool list at all, so nothing can open a memory: the index is a list
of keys the model has no way to use, and the prose telling it to use them is false. The
agent editor says so on the Memory box, and names _that_ reason rather than the
permission, because an operator told the narrower one would go and flip the wrong switch.

| You want                            | Do this                                      |
| ----------------------------------- | -------------------------------------------- |
| This agent to stop remembering      | The switch, `/memory off`, or `memory: deny` |
| To approve each thing it records    | Set `memory` to `ask`, the section stays     |
| The section gone, but the tool kept | `memoryPrompt: " "`, a single space          |

**The permission does not split.** There is no way to keep the index in the prompt while
denying the write: an agent told what it knows and forbidden to correct it is the shape
this feature exists to avoid. The other direction, recording without paying for the
index, is the last row of the table, and it is the prompt template rather than a second
switch.

`/memory off` changes the **agent**, not the session: every conversation on that agent is
affected, which is the same thing ticking the box in Settings does.

## The files

```
<workspace>/
└── memory/
    ├── auth-sessions.md
    ├── package-manager.md
    └── ui-stack.md
```

```markdown
# PostgreSQL-backed sessions

Sessions live in PostgreSQL and expire after 30 days. JWT-only auth was rejected
because revocation has to be immediate.

Related: [[auth-library]], [[database-choice]]
```

**The key is the identity.** It is the filename without `.md`, the only argument every
action takes, and what a `[[link]]` in another memory refers to. Saving a key that
already exists _replaces_ that memory, which is the whole of how a wrong one gets
corrected rather than contradicted by a second one beside it.

**There is no frontmatter, and no description field.** What an index line needs is a key
and a title. The key is already the filename, and the title is the first heading, so
neither has to be asked for or stored twice. A description field would be a second thing
to get wrong: a model that writes one disagreeing with the body leaves an index line that
is a lie, and every request pays for it whether or not it is any good.

### The title is derived

`derive_title` in `crates/core/src/memory.rs` takes the first markdown H1, else the first
line with anything on it, else the key. Then the markdown comes off, the whitespace
collapses, and the rest is cut to 80 characters.

Stripping is deliberately partial. Heading hashes, block quotes, list markers, links,
backticks, `**bold**`, `*italic*` and `~~strikethrough~~` all go. Underscores stay,
because `_italic_` in a title is rare and `snake_case_names` is not, and a rule catching
both would mangle the commoner one.

A memory is never skipped for lacking a heading, which is what makes a hand-written file
with no ceremony a working memory.

## What reaches the prompt

```
## Memory

Use exact keys with the `memory` tool. Do not guess a key.

auth-sessions: PostgreSQL-backed sessions
package-manager: Use Bun instead of npm
ui-stack: Tailwind CSS and shadcn/ui
```

**Almost data-only.** The section is paid for on every request, so every rule that can
sit in the tool description instead does. What is left is the one thing the data cannot
say for itself: the keys are exact. A small model that invents `auth-session` for a
listed `auth-sessions` spends a turn on the error.

**Every word of it is editable.** It is `agents.list.<id>.memoryPrompt`, and it follows
the contract the other prompt templates keep: empty inherits the built-in, a single space
deletes the section, anything else is this agent's own. See [Prompts](prompts.md).

| Placeholder | Is                                                                  |
| ----------- | ------------------------------------------------------------------- |
| `{{index}}` | The generated lines. **The section's content.**                     |
| `{{count}}` | How many lines the index carries. Available, unused by the default. |

**There is no `{{path}}`.** The tool takes a key and derives the folder itself, so a path
in the section would be a second spelling of the same address: two hundred lines' worth
of tokens for something the model never types, and one more thing it can reconstruct
wrongly.

The section lands in the **static** half of the prompt, the provider's cached prefix, and
is placed _after_ skills. Sections are appended in order so the cached prefix grows at
the end, and memory is the section a turn can rewrite, so it sits where a change
invalidates the least. See [Prompts](prompts.md).

## The tool

```
memory(action, key, content?)
```

| Action   | Takes          | Does                                                      |
| -------- | -------------- | --------------------------------------------------------- |
| `read`   | `key`          | Returns the file's content, and nothing around it.        |
| `save`   | `key`, content | Writes `memory/<key>.md`. An existing key is replaced.    |
| `delete` | `key`          | Removes it. A key with nothing under it is not a failure. |

The description carries the operating rules, because it is in front of the model at the
moment it decides to call and is sent once rather than on every request:

```
Persistent memory. Every key is listed in the Memory section of your prompt.
read and delete take an exact listed key. save creates a key or replaces one.
Saving overwrites the whole memory, so read a key before you change it.
One topic per memory, a short kebab-case key, content starting with a # heading.
Do not use file tools to reach memory.
```

Line three is the one that prevents data loss. `save` takes the whole content, so a model
that hears "sessions moved to Redis" and writes a one-line replacement drops every other
detail the memory carried. Read, then modify, then save.

**No path argument.** The folder is derived from the jail root, so there is nothing for a
model to get wrong and nothing for the jail to adjudicate. That is the whole reason this
is not a worse `write`.

A key is not a path, but it does reach a filename, so the guarantee that used to be free
is restored in `memory_slug`. The result is `[a-z0-9-]` and nothing else, so it cannot
contain a separator or a `..` and cannot leave `memory/` by construction rather than by a
check somebody could forget to call. It **slugs rather than refuses**: `Auth Sessions`
becomes `auth-sessions`, and the result says which key was used.

**Why `read` and `delete` are here at all**, when `read` and `exec` could reach the files:
because a permission is per agent. An agent that holds `memory` may well not hold the
filesystem tools, whether it is chat-only or behind lazy tool discovery. Leaving the
reading to one tool and the removing to another gives that agent an index it cannot open
and a wrong memory it cannot remove. One permission covers the whole feature.

A change lands in the **next** turn's prompt, not this one: the static half is built once
per turn.

Nothing stops a person, or the model through `write`, editing these by hand. That is
intended; see the placement section below.

## The commands

`/memory` exists in the terminal REPL and in Telegram.

| Command                     | Where          | Does                                                                 |
| --------------------------- | -------------- | -------------------------------------------------------------------- |
| `/memory`                   | REPL, Telegram | Whether the tool is granted, how many memories, what the index costs |
| `/memory on`, `/memory off` | REPL           | Grants or denies the `memory` tool on this agent                     |

**Telegram's `/memory` takes no verbs.** It reports, and when the tool is denied it says
where to grant it rather than granting it: the same status the REPL prints, without the
two commands that reconfigure the agent.

There is no `/memory edit`. `read` and `write` already open these, and a command whose
whole job is to hand a path to an editor is what [Tools](tools.md) argues against.

There is no command that summarises a session into memory either. A summary of a
conversation is not a fact about a workspace, and `memory/` holds the second kind.

## The bounds

**None of these is configurable, and there is no token budget.**

| Cap                      | Value | What it bounds                   |
| ------------------------ | ----- | -------------------------------- |
| `MAX_MEMORIES`           | 200   | How many are advertised at all.  |
| `MAX_MEMORY_TITLE_CHARS` | 80    | How long one index line runs.    |
| `MEMORY_MAX_BYTES`       | 12 KB | How much of one file is read.    |
| `MAX_MEMORY_NAME_CHARS`  | 64    | How long a key may be.           |
| content, in the tool     | 2000  | How much one `save` call writes. |

**A count of files, not a token budget**, and the two are not interchangeable. An index
line is a handful of tokens, so a budget in tokens would afford more lines than
`MAX_MEMORIES` ever admits: a lever whose value never decided anything. Keeping memory on
disk and out of the prompt is a capability question, and it is answered by the `memory`
tool's permission rather than by a number.

The ceiling is the product of the first two rows: 200 lines of at most 80 characters is
roughly 4k tokens in the static half if a workspace really fills the folder. That is the
cost of a very large memory store, and it is paid once per turn in the cached prefix
rather than per request.

`MEMORY_MAX_BYTES` is the same figure as `SKILL_MAX_BYTES` and the argument transfers: it
is what reading one of these costs when the model opens it. One memory does not need more.

See [Configuration](configuration.md).

## Where this lives, and what it costs

In the workspace, which is inside the jail, which means `write` and `exec` can both edit
these files.

`crates/core/src/paths.rs` once reserved an agent's own directory _beside_ the workspace
for exactly this reason: the jail root _is_ the workspace, so memory kept inside it is
writable by the agent, and that turns prompt injection into a way of rewriting the agent's
own system prompt. **That argument is correct, the reservation was removed, and the files
were put here anyway.**

What buys it: memory committed beside the project it describes, visible in a directory
listing, diffable in review, and correctable with an editor. A memory an operator cannot
see is one they cannot fix, and the failure mode of a wrong memory is every future turn on
that folder.

What follows from the placement, in code:

- **The `memory` tool is the intended path, not an enforced one.** `write` can still
  replace any of these files wholesale.
- **Every write is atomic.** A temp file beside the target, then a rename, so a crash
  cannot leave half a memory for the next turn to index.
- **Writes are serialised per workspace.** A save reports how many memories the folder
  holds afterwards, so it reads the folder as well as writing to it, and two landing
  together would each report a count that did not know about the other's file.
- **Reads are bounded** at 12 KB per file and 200 files, so a folder someone filled costs
  a prompt section and not the process.

If you want the injection-proof arrangement, set `write` to `ask` or `deny`.

## What is not built yet

- **No search.** The index is in the prompt on every turn, so the model picks a key from
  what it can already see. A search action would be a second way to find something that
  is already in front of it.
- **No ranking.** The index is alphabetical, which is what keeps the cached prefix stable.
- **No automatic learning.** Everything in `memory/` was written by a model calling the
  tool or by a person with an editor. A periodic pass folding what a session established
  into memory is a real idea, and nothing in the tree does it. There is no marker of how
  far such a pass has read, because there is no pass.
- **No settings panel for the files themselves**, and no REST route reading them. The
  section's wording is editable per agent; the content is a folder.
