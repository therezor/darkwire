# Prompts

An agent's whole system prompt is text you own. Not a persona appended below a hidden
preamble — the heading, the workspace rules, the platform note, the toolbox
advertisement, the guidelines and the tool-output policy are all editable, from the UI or
from `config.json`. So is every tool description the model reads; that half is in
[Tools](tools.md#rewriting-what-a-tool-says-about-itself).

That is a reversal of an earlier design, and worth stating plainly. The workspace and
guideline paragraphs used to be fixed on the grounds that they were not an operator's to
replace. The objection does not survive contact with what those sentences are: prose
telling the model what is true. The jail and the exec guard are enforced in code and have
never read a word of the prompt. **Deleting the workspace paragraph changes what the agent
_knows_, not what it _can do_.**

The same argument later took the last five sections with it — see
[What you can edit, and what that does not change](#what-you-can-edit-and-what-that-does-not-change).
And if filling in sections is not enough, one switch hands you
[the whole string](#sending-only-the-system-prompt).

## Two halves, and why

A provider's prompt cache keys on an exact prefix: the longest run of leading tokens
identical to the previous request is discounted, and the first differing token ends the
discount for everything after it. A single prompt carrying the current time therefore
costs full price on every iteration of every turn — and a tool-using turn is five or ten
requests over the same history.

So the prompt is assembled from two pieces:

| Half        | Rebuilt                                            | Contains                                                                                                    |
| ----------- | -------------------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| **Static**  | Once per turn, identical for the life of a session | Identity, workspace rules, the platform note, guidelines, the toolbox advertisement, the tool-output policy |
| **Runtime** | Every iteration                                    | Live state, this turn's delimiter, one-off corrections                                                      |

**The halves are two different messages, at two ends of the request.** That is the part
worth being precise about, because it was once got wrong: the runtime half used to be
appended to the system message, which is `messages[0]` — the _front_ of the request.
Everything after it is the session, so a changed iteration counter ended the
discount for the whole history on every request, and a ten-iteration turn over a long
session paid for that history ten times.

What a turn actually sends:

```
system( static half )       ← cached, session-stable
tools                       ← cached, stable per turn
...session history     ← cached, append-only
user( <system-reminder> )   ← the only part re-read at full price
```

The runtime half travels as a trailing **user** turn rather than a second system message:
two system messages is a shape some providers reject and others quietly reorder, and a
provider that hoisted it would put the volatile text back in front of the history, which
is the exact cost this avoids. It is wrapped in a `<system-reminder>` envelope so the
model reads it as operator metadata rather than as something you typed, and it is sent
but never stored — the history is the session, and this is scaffolding for one
request.

Anything that changes between requests must go in the runtime half. A timestamp, a
counter or a nonce placed in the static half invalidates the session's cached prefix on
every single turn, which is exactly the cost this split exists to avoid.

**The tool-output policy is in the static half**, which is why the built-in names no
delimiter. It is the largest block in the prompt that never changes — around two hundred
tokens — and it sat in the runtime half only because it spelled out a tag regenerated
every turn. The tag is now one line of live state and the prose is discounted. Putting
`{{tag}}` or `{{nonce}}` back into your own policy is allowed and moves the whole section
back to the per-step half; the agent editor says so when you do.

The context inspector reports the two halves separately, so the figure you can act on —
what each step of a turn costs again — is a number on the screen rather than an
inference.

**The figures are of the request body.** Each half is priced inside the message it is
sent in rather than as bare text, and anything the wire does not carry is not counted:
the model's own reasoning, which is kept beside the answer to be shown and left out of
history, and a tool's `risk` and `source`, which drive an approval prompt and a badge.
The measurement runs through the same encoder the transport uses, so a field that stops
being sent stops being counted in the same change.

## The eight templates

All eight live on the agent, in `config.json`, and all eight are edited in the agent
editor.

| Config key                          | Fills                      | Default constant                                        |
| ----------------------------------- | -------------------------- | ------------------------------------------------------- |
| `agents.list.<id>.systemPrompt`     | The whole static half      | `DEFAULT_SYSTEM_PROMPT_TEMPLATE`                        |
| `agents.list.<id>.livePrompt`       | The live-state section     | `DEFAULT_LIVE_STATE_TEMPLATE`                           |
| `agents.list.<id>.wrapUpPrompt`     | The out-of-iterations line | `DEFAULT_WRAP_UP_TEMPLATE`                              |
| `agents.list.<id>.platformPrompt`   | `{{platformPolicy}}`       | `DEFAULT_PLATFORM_HOST_TEMPLATE` / `…_TOOLBOX_TEMPLATE` |
| `agents.list.<id>.toolboxPrompt`    | The toolbox advertisement  | `DEFAULT_TOOLBOX_TEMPLATE`                              |
| `agents.list.<id>.toolPolicyPrompt` | The tool-output policy     | `DEFAULT_TOOL_POLICY_TEMPLATE`                          |
| `agents.list.<id>.memoryPrompt`     | The memory section         | `DEFAULT_MEMORY_TEMPLATE`                               |
| `agents.list.<id>.skillsPrompt`     | The skills section         | `DEFAULT_SKILLS_TEMPLATE`                               |

The last five used to be composed in code with no key to reach them. They are the
same kind of thing as the first three — prose telling the model what is true — and the
argument that opened the identity half applies to them unchanged. See
[What you can edit, and what that does not change](#what-you-can-edit-and-what-that-does-not-change).

### Empty means "use the built-in"

A stored prompt of `''` renders the default, so an install that never customised one keeps
receiving improvements to it on upgrade. Materialising the default into every config at
write time would freeze every agent on the wording that happened to ship the day it was
created.

**Whitespace is where the two kinds differ**, and the asymmetry is deliberate:

- `systemPrompt` treats whitespace-only as empty. A template of three newlines is not a
  decision anyone made, and an identity-less agent is never what was meant.
- The other seven do not. **Set any of them to a single space to delete the section
  entirely** — since empty already means "inherit", a space is the only way to say "I want
  this gone".

A space is invisible, so the editor does not ask anyone to type one: each section has a
**Remove section** button that writes it, and says `Removed` with a way back.

Only `systemPrompt` is on the screen by default. The other seven are behind an **Advanced
prompt settings** disclosure in the same section, because most agents only ever want the
first one and a screen that opens with eight editors reads as harder than it is.

**No stored template carries whitespace whose job is invisible.** `wrapUpPrompt` used to
open with two newlines, so that `Current time: {{time}}{{wrapUp}}` broke its paragraph and
still collapsed to nothing when the section did not apply. That is the right output held
in the wrong place: in the editor it showed as a box whose first two lines were empty,
which reads as a mistake somebody left behind. `renderWrapUp` adds the break now, so the
box holds a sentence and the prompt is byte-for-byte what it was.

## Placeholders

`{{name}}`, exactly, with no inner whitespace. **`{{ name }}` — with spaces — is a
literal** and passes through untouched, which is the escape hatch: one sentence to
document, one character class to implement, where a doubling rule would be neither.

Two vocabularies, because the two halves are cached differently and a value in the wrong
one is either useless or expensive.

### Static half — `systemPrompt`

| Placeholder         | Is                                                                          |
| ------------------- | --------------------------------------------------------------------------- |
| `{{name}}`          | The agent's label, or `GhostAI`.                                            |
| `{{workspaceId}}`   | Which workspace the session is bound to.                                    |
| `{{workspaceRoot}}` | The absolute workspace path. Available, and **unused by the default**.      |
| `{{runtime}}`       | `<os> <arch>, GhostAI <version>`. Available, and **unused by the default**. |

The last two are offered but not used, and it is worth knowing why before putting them
back:

- **The absolute root is the path the file tools hide.** Given it, a model writes
  `/Users/you/project/notes/todo.md`, which the jail resolves _inside_ the workspace — it
  lands at `<root>/Users/you/project/notes/todo.md`, a real tree of junk, with no error.
  It is also the one thing in the prompt that leaks your home directory layout to the
  provider.
- **`runtime` names the host OS**, which is where `exec` runs only when the agent has no
  toolbox. For a toolboxed agent it describes a machine none of its commands touch.

They remain in the vocabulary because a custom prompt may reasonably want them, and
removing a placeholder would silently change every stored template that uses it.

**`{{platformPolicy}}` is deliberately not on that list**, even though it is a section
you can edit. The command policy is a _section_ the loop places beside the toolbox
advertisement and the tool-output policy, and a placeholder cannot express "this section
does not apply": it renders to a string, and an empty one leaves the blank lines around it
behind. Raw mode is where you get to place it yourself — see
[Sending only the system prompt](#sending-only-the-system-prompt).

### Runtime half — `livePrompt`

| Placeholder                                                | Is                                                                       |
| ---------------------------------------------------------- | ------------------------------------------------------------------------ |
| `{{time}}`                                                 | Local reading with weekday and zone, plus the ISO instant.               |
| `{{wrapUp}}`                                               | The rendered wrap-up sentence — empty except in the last few iterations. |
| `{{iteration}}`, `{{maxIterations}}`, `{{iterationsLeft}}` | Counters.                                                                |
| `{{tag}}`                                                  | The tool-output delimiter in force for this turn.                        |
| `{{channel}}`, `{{sessionKey}}`                            | Available, and **unused by the default**.                                |

`{{iterationsLeft}}` is also the one placeholder `wrapUpPrompt` may use.

### The five section templates

Each has its own vocabulary, for the reason the two halves do: a value that means nothing
where it is written should be visible as a mistake rather than render as blank.

| Template           | Placeholder                                      | Is                                                        |
| ------------------ | ------------------------------------------------ | --------------------------------------------------------- |
| `platformPrompt`   | `{{runtime}}`, `{{platform}}`, `{{workspaceId}}` | The host, its bare platform name, the workspace.          |
|                    | `{{toolbox}}`, `{{workdir}}`                     | The container and its mount point. Empty on the host.     |
|                    | `{{shellPolicy}}`                                | The generated shell-tooling paragraph for this host OS.   |
| `toolboxPrompt`    | `{{name}}`, `{{workdir}}`                        | The box and where the workspace is mounted in it.         |
|                    | `{{tools}}` / `{{toolList}}`                     | `Installed:` and the bullets / just the bullets.          |
|                    | `{{notes}}`                                      | The manifest's notes.                                     |
| `toolPolicyPrompt` | `{{tag}}`, `{{nonce}}`                           | The turn's delimiter, and the random half of it.          |
| `memoryPrompt`     | `{{index}}`                                      | One line per memory. The section's whole content.         |
|                    | `{{path}}`, `{{count}}`                          | The folder, and how many lines the index carries.         |
| `skillsPrompt`     | `{{index}}` / `{{indexLines}}`                   | The catalogue with a leading blank line / just the lines. |
|                    | `{{path}}`, `{{count}}`                          | The folder, and how many skills there are.                |

**Two defaults, one override.** `platformPrompt` has a different built-in depending on
whether `exec` lands on the host or in a container, because one _function_ has to serve
both. An override does not: placement is `toolbox.name`, a config fact, so an operator
writing this for one agent writes the sentence that is true of it. Which default the
editor starts from — and which one an empty value renders — follows the same fact.

**A placeholder that can render to nothing carries its own leading blank line.**
`{{notes}}` is `'\n\n' + the notes` or `''`, never the notes alone, so a toolbox with none
leaves no gap where the paragraph would have been. `{{wrapUp}}` already worked this way;
the section templates generalise it rather than adding a second rule. The alternative — a
pass afterwards collapsing runs of blank lines — silently rewrites an operator's spacing
to fix a problem the renderer created.

### The default live-state block is two lines

```
## Live state

Current time: {{time}}
Tool output delimiter: {{tag}}{{wrapUp}}
```

It used to be five, and the other three were removed on cost grounds. This half is never
cached, so every line is re-sent on every request of every turn:

- **The session key** is a UUID — twenty tokens of random string per request, for an
  identifier the model cannot use and might echo at the user.
- **The channel** named a difference no instruction drew a consequence from.
- **The iteration counter** is only actionable near the cap. It now prints only in the
  last three iterations, which is what `{{wrapUp}}` is for.

The time is the first line that earns its place: a model has no clock, and without it
"today" and "latest" are answered from a training cutoff. The delimiter is the second, and
it earns more than it costs — naming the tag here is what buys the whole two-hundred-token
tool-output policy a place in the _cached_ half.

## Typos are visible, not silent

`render_prompt_template` never fails — a prompt that fails to build fails every turn on
that agent, and an operator's typo is not a reason to take the agent offline.

An **unknown placeholder is left verbatim**. `{{workspacRoot}}` renders as itself rather
than as an empty string, so the mistake is visible in the prompt instead of quietly
deleting the line that was supposed to say where the workspace is. The editor also warns
before the save; this is the backstop for everything that gets in another way.

Substitution is a single pass and inserted values are never rescanned, so a workspace
named `{{workspaceRoot}}` cannot expand into anything.

## `{{platformPolicy}}` has two defaults

This is the one part of the static half that depends on _placement_. The same built-in
text has to be true whether `exec` lands on the host or in a container, and those two are
opposite on every point that matters: whether the workspace confines the command, whether
a shell is available, and which OS's tools exist. Hence two defaults, and code that picks
between them — `platformPrompt` overrides whichever applies.

Getting it wrong is not cosmetic. The host wording used to be emitted for every agent, so
a toolboxed one was told its commands ran on macOS when they run in Alpine, that they were
_not_ confined to the workspace when only the workspace is mounted, and on Windows that
GNU tools might be missing when the container has them. A model resolving a contradiction
between its prompt and its tools tends to resolve it by refusing.

The sentence that earns its tokens either way: **the file tools are
placement-independent.** They always act on the workspace on this machine, through the
jail, whatever `exec` does. Without that, a model has no way to know that the file it
wrote and the file a command sees are the same file under two names.

## What you can edit, and what that does not change

**Every word of it, including the tool-output policy.** That was the one exception, and
it no longer is. The reasoning it used to rest on — that the policy is the
prompt-injection defence rather than prose — does not survive looking at what the defence
actually consists of:

- `wrap_tool_output` wraps **every** tool result in `<tool_output_<nonce>>` fences and
  escapes any forged delimiter inside the content. It does not read this template.
- The nonce is eight bytes regenerated per turn from the injected `RandomSource`. It does
  not read this template either.

So the policy paragraph is the _explanation_ of a mechanism, not the mechanism. Delete it
and the envelopes are still there and still unforgeable; what is gone is the model's
reason to treat what is inside one as data. That is worth a warning, and it gets one — in
the editor as you type, and as a `tool_policy_missing_nonce` config warning on
`GET /api/settings` — but it is not worth being the single exception to a promise the
rest of the prompt keeps. See [Security](security.md) for the mechanism.

Where it lives follows from what you write in it. The built-in names no delimiter, so it
is session-stable and sits in the static half; a template that spells out `{{tag}}` or
`{{nonce}}` changes every turn and moves back to the runtime half, at the caching cost
the editor warns about.

The same holds one layer down and for the same reason. **Deleting the workspace paragraph
does not widen the sandbox** — `WorkspaceJail` and `guard_exec` are enforced on every call
and have never read a word of the prompt. **Rewriting `exec`'s description does not
change what `exec` accepts** — the schema it validates against is generated from its own
argument type; see [Tools](tools.md#rewriting-what-a-tool-says-about-itself).

Editing any of this changes what the agent _knows_, not what it _can do_.

## Sending only the system prompt

`agents.list.<id>.promptMode` is `template` by default: the two-half assembly above, with
each section filled from the templates the operator owns.

Set it to `raw` and `systemPrompt` **is** the system message. Nothing is prepended,
appended or interleaved — no live-state block, no toolbox section, no tool-output policy.
A template that wants one names its placeholder:

```
# Reviewer

Read files. Say what is wrong. Do not write.

{{toolPolicy}}
```

`RAW_PROMPT_PLACEHOLDERS` is the union of both vocabularies plus the sections the loop
would otherwise have placed: `{{platformPolicy}}`, `{{toolbox}}`, `{{toolPolicy}}`,
`{{nonce}}`, `{{tag}}`, `{{contributors}}`, `{{runtimeSections}}` and `{{correction}}`.

The section templates still apply — `{{platformPolicy}}` renders from `platformPrompt`,
`{{toolbox}}` from `toolboxPrompt` — so raw controls the _layout_ rather than discarding
the wording. `livePrompt` is the one field it ignores, because its entire content is
`{{time}}`, `{{tag}}` and `{{wrapUp}}`, and all three are named here directly.

**What it costs is the cache.** In template mode the static half is a byte-identical
prefix a provider discounts for the life of the session. One blob rebuilt every iteration
has no such prefix if anything in it moves: a `{{time}}` at the top ends the discount for
everything after it, on every request of every turn. A raw template that names no
volatile placeholder renders identically each iteration and caches exactly as well as
before — which is the case an operator writing a fixed instruction sheet lands in without
trying. The editor warns when it is not.

Contributor `staticSection` I/O still happens once per turn in both modes.

**In the editor it is a switch, not a mode.** `Send only the system prompt`, under
`Advanced`, beside the five section templates it hides. It used to be a select at the top
of the section offering `Sections` and `Raw`, which made a rare decision the opening move
of an ordinary screen — and named the two halves of the feature after their
implementation. An operator reaching for this wants "stop adding things to my prompt",
which is a behaviour; `raw` is only what the config calls it.

## Toolbox advertisement

When an agent has a toolbox, the static half gains a section describing it — and this is
**prose composed from a declared list**, not a set of tool schemas.

A research or Kali image carries hundreds of programs a model already knows from
pretraining. Declaring them as schemas would cost thousands of tokens on every request to
say what forty say once. The rules true of _every_ toolbox — that `exec` lands in a
container, that a shell is available there, that only the workspace is mounted, where
oversized output goes — are the built-in wording; the manifest declares only what is
specific to it, and `{{tools}}` and `{{notes}}` are where it lands.

`toolboxPrompt` rewrites that wording per agent. The section is only rendered while
`toolbox.name` is set, so an agent on the host sends nothing here whatever it says — and
setting it to a single space is how an operator whose own `systemPrompt` already covers
the ground stops paying for the preamble twice.

Longer-form tool documentation is the agent's own `systemPrompt`'s job, and each shipped
toolbox's preset carries it there ([Toolboxes](toolboxes.md#agent-presets)). Nothing
reads a file from the toolbox directory into the prompt: what a manifest entry declares
in `use`, `args` and `example` is what reaches the model, and prose repeating it would be
a second copy to keep in step.

## Extending the prompt in code

`ContextContributor` is the seam for content the loop knows nothing about:

```rust
trait ContextContributor {
    fn name(&self) -> &str;
    fn static_section<'a>(
        &'a self,
        context: &'a StaticPromptContext,
    ) -> BoxFuture<'a, Option<String>>;
    fn runtime_section(&self, context: &RuntimePromptContext) -> Option<String>;
}
```

The two halves carry different obligations and are not interchangeable, and the signatures
say so. `static_section` is called once per turn, is async so it **may** do I/O, and must
be stable across the session — a section that changes wherever it likes hands back the
cache benefit the split was built for. `runtime_section` is called on every iteration and
is synchronous, so anything expensive there is paid five or ten times per turn, and an
`await` is not available to hide it in.

Contributor sections are appended after the built-in ones, in the order given, so the
cached prefix grows at the end rather than shifting when a contributor is added.

**[Skills](skills.md) arrive this way**: `SkillsContributor::static_section` reads
`<workspace>/skills/` once per turn and renders the index. It is wired in the composition
root — the loop composes and caches sections and deliberately knows nothing about where one
came from.

**[Memory](memory.md) is the second**, and it works the same way: the workspace's
`memory/` folder is a property of the folder rather than of one message, so it too has only
a static half. It is placed _after_ skills, because sections are appended in order and
memory is the one a turn can rewrite — so it sits where a change invalidates the least of
the cached prefix.

`memoryPrompt` and `skillsPrompt` are therefore the two section templates that travel on
neither of the prompt builder's arguments: `systemPrompt`, `livePrompt` and `wrapUpPrompt`
arrive on `PromptAgent`, and `toolboxPrompt`, `policyPrompt` and `platformPrompt` on
`PromptTools`. Those six fill sections the prompt builder writes; these two are
contributors'. The composition root hands them to `MemoryContributor`
and `SkillsContributor` directly. In the editor that distinction is invisible, and
deliberately — an operator editing their prompt does not care which of the two wrote a
paragraph.

Both sections are gated twice over: on their own tool's permission, and on `toolsEnabled`.
The second is what a reader is most likely to be surprised by — an agent whose model cannot
call tools carries neither section, however its permissions read, because an index of paths
is only useful to something that can open one.

**`skillsPrompt`'s `{{index}}` carries its own leading blank line**, so a template that
places it straight after its prose leaves no gap when there is nothing to list. That is the
convention `{{notes}}` already keeps on the toolbox template. It also cost a rule: the
preamble used to disappear when the index was empty, since it told the model to open the
files below it and there were none. A placeholder cannot express that condition, so the
built-in wording says "a line below" rather than "each line below" and is true either way.

There is no placeholder for a skill's body. A sheet is never templated into the prompt at
all: the agent opens the file the index names, long after this section is rendered.
