# Sandbox service

The sandbox service owns Docker or Podman. The DarkWire app talks to it through a
versioned, bounded Unix socket and never needs the container runtime socket. The service
accepts only registered workspace IDs, environment names and an argv array; a client cannot
submit an image, a mount, a raw daemon argument or a shell command. The container
definition and the placement it implies are checked independently on every request,
against the policy directory rather than the caller's word.

**A single-binary install needs none of this.** `darkwire serve` starts the service as a
task of its own on a socket under the install root when no deployed one answers, and
talks to it exactly as it would talk to one beside it. One code path, one set of approval
checks, and the engine still owned by one component. It registers every workspace in the
registry at boot, so a workspace created afterwards needs a `serve` restart before a
container in it will run; nothing is started when `DARKWIRE_SANDBOX_SOCKET` names a service
or a socket is already listening, because two services over one state directory would each
try to reap the other's containers.

## Upgrading a deployed service

**The app and the service must be the same version.** The socket protocol is a private
wire between two artefacts that are built separately, so a deployed service left on an
older image fails the first `exec` with a parse error rather than a sentence. Rebuild the
`darkwire-environment` image whenever you upgrade the app.

0.10 renames two fields on that wire: the `exec` and `start` operations now carry
`environment` rather than `container`, and a workspace registration lists `environments`
rather than `containers`. `DARKWIRE_SANDBOX_CONTAINERS` is `DARKWIRE_ENVIRONMENTS`. A
single-binary install is unaffected, because `darkwire serve` builds both halves from the
same binary.

## Container deployment

Copy `deploy/sandbox/examples` into an absolute data directory as `policies`, replace the
example image placeholder with a digest or local image ID, then install each environment
definition from an operator-controlled installation. Build the gateway image when any
agent asks for `allowlist` egress:

```bash
docker build -f deploy/sandbox/Dockerfile.gateway -t darkwire-gateway:local .
export DARKWIRE_DATA_DIR=/srv/darkwire
export DARKWIRE_GATEWAY_IMAGE=darkwire-gateway:local
export DARKWIRE_PASSWORD='replace-me'
docker compose -f deploy/sandbox/compose.yaml up --build
```

The Compose file mounts the runtime socket only into `sandbox`. Policy files are read-only
in both services; app state, sandbox state and the registered workspace have separate
mounts. `DARKWIRE_DATA_DIR` is also the absolute host path the Docker daemon sees. If the
daemon runs elsewhere, use a JSON service config and set each `daemonPath` explicitly.

The default web listener is bound to `127.0.0.1:3000`. Put TLS/authentication in front of
it before exposing it beyond the machine.

## The three entry modes

`darkwire-environment` reads its first argument:

- **`proxy`** — runs the egress proxy inside the gateway container, on loopback port 3128
  as uid 65532. Started by the service when an agent scopes egress by host name; not run
  by hand.
- **`serve-env`** — the mode `deploy/sandbox/compose.yaml` uses. Every path is the fixed
  one inside the image, and `DARKWIRE_DATA_DIR` supplies the absolute host path the daemon
  sees for the same directories. `DARKWIRE_GATEWAY_IMAGE` and
  `DARKWIRE_ENVIRONMENTS` fill in the rest; the latter defaults to `dev`. It
  registers one workspace, `default`.
- **a path** — the config file below, which is the only mode that can register more than
  one workspace or point at a daemon whose paths differ from the service's own.

## Direct service configuration

Run `darkwire-environment /absolute/path/service.json` with:

```json
{
  "socket": "/run/darkwire/sandbox.sock",
  "policyRoot": "/srv/darkwire/policies",
  "stateRoot": "/srv/darkwire/sandbox-state",
  "daemonStateRoot": "/srv/darkwire/sandbox-state",
  "engine": "docker",
  "gatewayImage": "darkwire-gateway:local",
  "workspaces": {
    "default": {
      "path": "/srv/darkwire/workspaces/default",
      "daemonPath": "/srv/darkwire/workspaces/default",
      "environments": ["dev"]
    }
  }
}
```

Service-visible paths must be absolute and must not overlap policy or control state.
Socket permissions are `0660`; use a dedicated OS group for app access. Startup reaps
containers from an earlier boot of the same installation while leaving other DarkWire
installations alone. Per-command output is bounded; the full transcript stays under the
sandbox state directory, which is mounted read-only into the container so the agent can
read its own output back with `exec`.

## Management

The CLI and Settings → Environments use the same management protocol:

```bash
darkwire sandbox health
darkwire sandbox list
darkwire sandbox start --environment dev --workspace default
darkwire sandbox stop INSTANCE
darkwire sandbox restart INSTANCE
```

Stop and restart do not refuse a busy instance. The container goes away under whatever it
was running, queued work comes back aborted, and the next command starts a fresh container
from the same definition. A container is not released when one session ends; idle reaping
and explicit lifecycle operations manage it.

Any installed definition can be warmed, in the CLI and in Settings → Environments. An
instance is keyed on the workspace, the definition and the network, none of which is
particular to one turn, so the one warmed ahead of time is the one a turn goes on to use.
