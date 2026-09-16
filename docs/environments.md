# Environments

An environment is **where an agent's commands run**. Every agent has one: the default is
the host, which is the machine DarkWire itself runs on, and naming an installed definition
moves `exec` into a container instead.

Environments do not grant capabilities. An agent's `tools` permission map is the complete
authority for built-in, extension and MCP tools; an environment only decides where the
command that was already allowed actually starts.

An environment definition fixes the image, mounts, hardening and lifecycle. It holds no
network policy of its own: egress is the agent's request, and the definition's hardening
only decides whether a restricted allow-list can be enforced around it at all.

## Definitions

Definitions live under `policy/environments/<name>.yaml`. Each uses
`darkwire.environment/1` and pins an image digest. `darkwire environment list` reports the
installed definitions and their hardening; the **Environments** tab in Settings shows the
same list and is where one is authored, edited or removed. The environment service
re-resolves a definition before use and stops work if it has drifted.

The policy directory sits beside the workspace, never inside it, so nothing a tool can
write reaches a definition. Settings is a door for the operator, not for the agent, and a
save there clears exactly the checks a hand-written file clears: an image that is not
digest-pinned and a capability that is never grantable are refused either way.

Two writes are refused because the next start would refuse them: removing an environment
an enabled agent names, and saving one that stops being able to enforce an allow-list an
agent already asked for. Both name the agents in the refusal.

A save re-emits the file from the definition, so comments and key order in a hand-written
one are lost the first time it is saved from Settings. That also moves the digest, which
is correct: the digest is identity, and an edited definition is a different one.

```yaml
schema: darkwire.environment/1
kind: container
name: dev
image: sha256:…
limits:
  memoryMb: 512
  cpus: 1
```

Three fields are required: the tag, a name, and a digest-pinned image. Everything else has
a default, and the defaults are sized for a small board because that is what this runs on.

`kind` has one arm today. It is there so a remote environment is a variant rather than a
second migration, and every other field below it still describes a container.

**A read-only root gets a writable `/tmp` by default.** `exec` records its pid there so a
timeout or a cancel has something to signal; a definition with neither would keep running
commands that nothing could stop, and would fail any build. Anything else the image needs
to write wants its own `security.tmpfs` entry, `$HOME` most often, because only the image
knows where that is.

## Agent configuration

```yaml
agents:
  list:
    reviewer:
      tools:
        exec: ask
      environment:
        name: dev
        network:
          mode: allowlist
          allow: [10.0.0.0/8]
          hosts: [api.example.test]
          dns: [10.0.0.53]
```

With an empty name the agent runs on the host and network must be `none`. There is no
gateway on the host to enforce anything, so a request there would mean nothing. With a
named environment, `open`, `none` and `allowlist` are available. An allow-list needs
CIDRs, exact host names, and DNS resolver addresses.

**A definition carries no prose.** What the model is told about where its commands run,
and about what is installed there, is one section on the agent: `platformPrompt`. See
[Prompts](prompts.md). A definition still setting `prompt` is reported on its row and read
by nothing.

## A delegated turn runs where its caller does

**Work handed down stays inside the boundary the operator chose**, rather than falling
back to the host halfway down a chain. At the top of a chain there is no caller, so the
agent runs in the environment it names.

An agent overrides that with **`environment.alwaysUseOwn`**, off by default, which pins it
to its own environment whoever called. A web-search agent with a browser in its image is
the case: it is useless anywhere else, and the caller cannot be expected to know that.
Pinning the host is expressible too, which it was not before this field.

The switch is in the agent editor, under the environment picker it qualifies. It is a
property of the agent rather than of one delegation because an agent that needs its own
toolchain needs it from every caller; asking once per roster lets two rosters disagree
about one agent.

What is inherited is the _name and network policy_, and in practice it is also the
container: an instance is keyed on the workspace, the definition and the network, with
neither the agent nor the session in it, so a caller and its subagents work in one
container rather than one each.

This used to be implied by the target naming no environment, which meant "the host" at the
top of a chain and "inherit" below it. One spelling for two answers, and no way to ask for
the host under a containerised caller at all.

## Lifecycle

**An environment is a place, and one place is one container.** Everything in a workspace
asking for the same definition and the same egress lands in the same instance. What that
costs is the container's own ephemeral filesystem, since two commands may write `/tmp` and
`$HOME` at once; the workspace itself was already shared across containers by bind mount.
Commands are not queued behind each other.

The environment service owns the lifecycle and exposes inspection and stop operations
through `darkwire sandbox` and the Environments settings tab. Containers are reaped when
they go idle and on reconfigure.

## Migrating from `darkwire.container/1`

Definitions written against the old tag do not load. Three steps:

1. `mv ~/.darkwire/policy/containers ~/.darkwire/policy/environments`
2. Change each definition's `schema:` line to `darkwire.environment/1`.
3. Rename `container:` to `environment:` under each `agents.list.<id>` in `config.yaml`.

`darkwire environment list` says so when it finds the old directory still in place.
