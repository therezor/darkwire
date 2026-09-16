# Prompts

An agent's prompt is assembled from editable templates plus per-turn state. Empty template
fields inherit the built-in wording; a single space removes that section.

The stable part contains the system prompt, platform policy, tool-output policy, and any
memory or skill indexes. The runtime part contains time, iteration state, and the random
tool-output delimiter. Keeping per-turn values out of the stable part lets providers cache
the larger prefix.

`platformPrompt` fills `{{platformPolicy}}` and is the one section about placement. It
says both _where_ commands run and what is _there_. The built-in host template describes
host `exec`; the container template is used whenever the turn's environment is confined,
and tells the model to check for a tool rather than assume it. Which one applies is
decided per turn rather than per agent, because a delegated turn runs where its caller
does unless the agent pins itself.

There used to be a second section, `environmentPrompt`, whose wording came from the
environment definition's own `prompt`. It was the same topic split by who happened to
author each half, and the heading came from operator data rather than from this repo.
Anything an image needs said goes in `platformPrompt` now. A config still setting
`environmentPrompt` is reported as a warning rather than read.

The supported placeholders are exported by `@ghostwire/protocol`. The editor flags unknown
placeholders before saving, while the renderer leaves unknown placeholders visible rather
than silently deleting text.
