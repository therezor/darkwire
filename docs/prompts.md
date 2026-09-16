# Prompts

An agent's prompt is assembled from editable templates plus per-turn state. Empty template
fields inherit the built-in wording; a single space removes that section.

The stable part contains the system prompt, platform policy, environment section,
tool-output policy, and any memory or skill indexes. The runtime part contains time,
iteration state, and the random tool-output delimiter. Keeping per-turn values out of the
stable part lets providers cache the larger prefix.

`platformPrompt` fills `{{platformPolicy}}` and says _where_ commands run. The built-in
host template describes host `exec`; the container template is used whenever the turn's
environment is confined. Which one applies is decided per turn rather than per agent,
because a subagent that names no environment inherits its caller's.

`environmentPrompt` fills `{{environment}}` and says what is _there_: what is installed,
and what the mounts mean. It is the one section with no built-in: empty inherits the
environment definition's own `prompt`, a single space removes it, and an agent on the host
places no section at all. Nobody but the operator knows what is in an image, and a wrong
guess about the toolchain costs the model a turn finding out.

The supported placeholders are exported by `@ghostwire/protocol`. The editor flags unknown
placeholders before saving, while the renderer leaves unknown placeholders visible rather
than silently deleting text.
