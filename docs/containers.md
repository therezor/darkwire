# Containers

Containers are optional execution placement for the built-in `exec` tool. They do not
grant capabilities: an agent's `tools` permission map is the complete authority for
built-in, extension, and MCP tools.

An agent normally runs `exec` on the host. Set `agents.list.<id>.container.name` to run
it in an installed container instead. The container definition fixes the image, mounts,
hardening and lifecycle. It holds no network policy of its own: egress is the agent's
request, and the definition's hardening only decides whether a restricted allow-list can
be enforced around it at all.

## Definitions

Install container definitions under `policy/containers/<name>.yaml`. Each definition
uses `ghostai.container/1` and pins an image digest. `ghostai container list` reports
the installed definitions and their hardening. The sandbox service re-resolves a
definition before use and stops work if it has drifted.

## Agent configuration

```yaml
agents:
  list:
    reviewer:
      tools:
        exec: ask
      container:
        name: dev
        network:
          mode: allowlist
          allow: [10.0.0.0/8]
          hosts: [api.example.test]
          dns: [10.0.0.53]
```

With an empty container name, `exec` runs on the host and network must be `none`.
With a container, `open`, `none`, and `allowlist` are available. An allow-list needs
CIDRs, exact host names, and DNS resolver addresses.

Containers can be shared or private as declared by their definition. The sandbox service
owns their lifecycle and exposes inspection and stop operations through `ghostai sandbox`.
