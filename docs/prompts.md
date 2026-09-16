# Prompts

An agent's prompt is assembled from editable templates plus per-turn state. Empty template
fields inherit the built-in wording; a single space removes that section.

The stable part contains the system prompt, platform policy, tool-output policy, and any
memory or skill indexes. The runtime part contains time, iteration state, and the random
tool-output delimiter. Keeping per-turn values out of the stable part lets providers cache
the larger prefix.

`platformPrompt` fills `{{platformPolicy}}`. The built-in host template describes host
`exec`; the container template is used whenever `container.name` is set.

The supported placeholders are exported by `@ghostwire/protocol`. The editor flags unknown
placeholders before saving, while the renderer leaves unknown placeholders visible rather
than silently deleting text.
