# Prompts

An agent's prompt is assembled from editable templates plus per-turn state. Empty template
fields inherit the built-in wording; a single space removes that section.

The stable part contains the system prompt, platform policy, tool-output policy, and any
memory or skill indexes. The runtime part contains time, iteration state, and the random
tool-output delimiter. Keeping per-turn values out of the stable part lets providers cache
the larger prefix.

## Running commands

One section, one field, and one default text.

| What the agent's environment is           | What an empty `platformPrompt` inherits |
| ----------------------------------------- | --------------------------------------- |
| The host                                  | the default below                       |
| A container whose definition is silent    | the default below                       |
| A container whose definition has `prompt` | that prose, under the same heading      |

```markdown
## Running commands

`exec` runs with the workspace root as its working directory, so pass relative
arguments. Paths outside the workspace may be refused.

The file tools always act on the workspace on this machine, whatever `exec`
does. Prefer them where they are simpler or more reliable than a command.
```

**There used to be two defaults, a host one and a container one.** The host one
claimed that commands run on this machine and are _not_ confined to the workspace,
and both are false in a container, so the wording had to be chosen per turn and a
wrong choice told a delegated agent the opposite of the truth. Neither claim is in
the text above, so one default is true wherever the turn lands and there is no
choice left to get wrong.

**It is plain text.** `{{runtime}}` and `{{shellPolicy}}` are still offered and
still filled, so a template that names one keeps working, but the default names
neither. `{{shellPolicy}}` was a paragraph generated in Rust, two bullets on
POSIX and three on Windows; the POSIX pair said little beyond "a shell exists",
and the Windows three are now something an install writes into its agent if it
wants them, where they can be read rather than generated.

**The definition replaces the default, it does not follow it.** An image that
lists what it holds has said the useful half already. Only the image knows what is
installed, so saying it once in the definition beats restating it on every agent
that uses one.

**One field rather than two.** The editor seeds the box with whichever built-in
applies, so an agent granted three of an image's ten tools opens the box on the
image's own list and deletes seven. An override replaces the section, the way an
override replaces any other template here; a single space deletes it.

The built-in carries the heading for the same reason every other section's
template does: a default is the seed an operator's first edit is a diff against,
so a heading added later at render time would double the one they had edited.
`platformTemplate` in `@darkwire/protocol` builds it, which is how the editor
shows exactly what the turn will carry.

In raw mode `{{platformPolicy}}` places the whole section. `{{environment}}` is
not a placeholder and nothing ever filled it; a raw template naming it is
reported, as is an agent still setting the retired `environmentPrompt`.

The supported placeholders are exported by `@darkwire/protocol`. The editor flags unknown
placeholders before saving, while the renderer leaves unknown placeholders visible rather
than silently deleting text.
