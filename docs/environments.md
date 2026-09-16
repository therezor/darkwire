# Environments

An environment is **where an agent's commands run**. Every agent has one: the default is
the host, which is the machine GhostAI itself runs on, and naming an installed definition
moves `exec` into a container instead.

Environments do not grant capabilities. An agent's `tools` permission map is the complete
authority for built-in, extension and MCP tools; an environment only decides where the
command that was already allowed actually starts.

An environment definition fixes the image, mounts, hardening and lifecycle. It holds no
network policy of its own: egress is the agent's request, and the definition's hardening
only decides whether a restricted allow-list can be enforced around it at all.

## Definitions

Install definitions under `policy/environments/<name>.yaml`. Each uses
`ghostai.environment/1` and pins an image digest. `ghostai environment list` reports the
installed definitions and their hardening, and the **Environments** tab in Settings shows
the same list. The environment service re-resolves a definition before use and stops work
if it has drifted.

```yaml
schema: ghostai.environment/1
kind: container
name: dev
image: sha256:…
prompt: |
  ## Environment

  Node 22, pnpm 9 and cargo 1.83 are installed. The workspace is mounted at /workspace.
```

`kind` has one arm today. It is there so a remote environment is a variant rather than a
second migration, and every other field below it still describes a container.

`prompt` is what the model is told about this place. See [Prompts](prompts.md). It is
optional and has no built-in: an environment that says nothing about itself places no
section, because nobody but the operator knows what is in an image and a wrong guess about
the toolchain costs the model a turn finding out.

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

An agent may override the definition's `prompt` with `environmentPrompt`, on the same
three-state contract as every other template: empty inherits, a single space removes the
section, anything else replaces it.

## Subagents inherit when their caller says so

Each entry in an agent's `subagents` list carries **`inheritEnvironment`**, on by default.
On, the delegation runs where its caller does, and the subagent's own `environment` is not
consulted. Off, it runs in the environment its own entry names, which is the host when it
names none. The switch is in the agent editor, on the subagent's row.

It lives on the reference rather than on the target because being somebody's subagent is a
relationship: the same researcher can inherit from one caller and run on the host for
another.

What is inherited is the _name and network policy_, not a running instance. The subagent
resolves its own placement from its own agent id and a fresh session key, so under a
private definition it gets its own container; only a definition with `shared: true` puts
caller and subagent inside the same one.

This used to be implied by the target naming no environment, which meant "the host" at the
top of a chain and "inherit" below it. One spelling for two answers, and no way to ask for
the host under a containerised caller at all.

## Lifecycle

Environments can be shared or private as declared by their definition. The environment
service owns their lifecycle and exposes inspection and stop operations through
`ghostai sandbox` and the Environments settings tab.

## Migrating from `ghostai.container/1`

Definitions written against the old tag do not load. Three steps:

1. `mv ~/.ghostai/policy/containers ~/.ghostai/policy/environments`
2. Change each definition's `schema:` line to `ghostai.environment/1`.
3. Rename `container:` to `environment:` under each `agents.list.<id>` in `config.yaml`.

`ghostai environment list` says so when it finds the old directory still in place.
