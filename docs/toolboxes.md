# Toolboxes and containers

Two manifests, installed separately, that deliberately do not know about each other.

A **toolbox** is the complete set of operations one agent may call. It names reusable
operation definitions and a permission ceiling for each. It holds no image, no
capabilities and no network, so nothing in it can widen a boundary.

A **container** is where command operations run: an image, the hardening around it, its
resource budget, and whether agents share one instance. It holds no tool grants, so
choosing a place to run commands grants no particular command.

An agent selects each independently. The same toolbox runs on the host or in different
containers, and several agents can reuse one shared container in a workspace.

This is the answer to the honest limit stated in [Security](security.md): a workspace is
an organisational boundary, not a security boundary, wherever host `exec` is enabled. A
container is the boundary.

## The policy directory

Three flat directories under `~/.ghostai/policy/`, one file per definition:

```text
policy/
├── toolboxes/coding.yaml                 ghostai.toolbox/1
├── tool-definitions/git-status.yaml      ghostai.tool/1
└── containers/dev.yaml                   ghostai.container/1
```

**Each file is the policy, and where the directory lives is the boundary.** It sits
**beside** the workspace, never inside it. The jail root _is_ the workspace, so a
definition kept in there would be writable by `write_file`, and prompt injection would
become a way to rewrite the policy the agent runs under. Writing one of these files is the
decision, the same way writing `config.yaml` is.

## The toolbox

A grant list, and nothing else:

```yaml
{
  'schema': 'ghostai.toolbox/1',
  'name': 'coding',
  'label': 'Repository inspection',
  'version': '1.0.0',
  'notes': 'Builds are slow. Prefer a targeted test over the whole suite.',
  'tools':
    [
      {
        'name': 'git_status',
        'definition': 'git-status',
        'permission': 'allow',
      },
      { 'name': 'cargo_test', 'definition': 'cargo-test', 'permission': 'ask' },
    ],
}
```

| Field                | Type                   | Default   | Notes                                                    |
| -------------------- | ---------------------- | --------- | -------------------------------------------------------- |
| `schema`             | `'ghostai.toolbox/1'`  | —         | Required.                                                |
| `name`               | slug                   | —         | Also the filename. Lowercase, digits and `-`.            |
| `label`              | string                 | `''`      | Shown in the UI. Empty falls back to the name.           |
| `version`            | string                 | `'0.0.0'` | The manifest's own version.                              |
| `notes`              | string                 | `''`      | Caveats about the set, appended to the prompt section.   |
| `tools[].name`       | string                 | —         | The callable name, local to this toolbox.                |
| `tools[].definition` | slug                   | —         | The definition under `tool-definitions/` it resolves to. |
| `tools[].permission` | `allow \| ask \| deny` | `'ask'`   | A **ceiling**. An agent may only tighten it.             |

`notes` is model guidance, never an authorisation rule.

## The operation definition

One reviewed operation, reusable by every toolbox that names it:

```yaml
{
  'schema': 'ghostai.tool/1',
  'description': 'Show repository status',
  'parameters':
    { 'type': 'object', 'properties': {}, 'additionalProperties': false },
  'implementation':
    {
      'kind': 'command',
      'executable': '/usr/bin/git',
      'argv': ['-c', 'core.fsmonitor=false', 'status', '--porcelain=v1'],
    },
}
```

An operation is a fixed program and a reviewed argument mapping, never a shell string.
`git diff $PATH` is not expressible, because there is nowhere to put it. The JSON Schema
it publishes is the same one its inputs are validated against before a process starts, so
what the model was told it could send and what the sandbox accepts cannot drift apart.

Three implementation kinds:

- **`command`** — an absolute `executable` outside the workspace, plus `argv`. An argv
  element may be a literal string or `{ "input": "path", "workspacePath": true }`, naming
  a required scalar property of the schema. `workspacePath` resolves the value through
  the jail. `argvInput` names a required array of strings appended verbatim, and should
  be reviewed as a broad capability.
- **`registered`** — an installed built-in, MCP or extension tool, pinned by `tool` name
  and the `digest` of its advertised definition. This is how a toolbox grants a built-in
  back.
- **`transcript`** — reads a bounded portion of output from the agent's current container
  instance.

Four rules are enforced when a definition is read and again at every call:

- The schema must be self-contained. `$ref`, `$dynamicRef` and `$recursiveRef` are
  refused, because validation must never fetch anything.
- `type: object` with `additionalProperties: false`, so an extra argument is refused
  rather than ignored.
- The executable is absolute, contains no `..`, and is not under `/workspace` — a file
  `write_file` could replace between resolution and call.
- Every argv input is a **required scalar** property. Optional means the argv has a hole
  at a position the operator counted on being filled; non-scalar means one input becomes
  several arguments.

A tool definition has no digest of its own. It is covered by the digest of every toolbox
that names it: a definition shared by three toolboxes cannot be edited without all three
noticing.

## The container definition

```yaml
{
  'schema': 'ghostai.container/1',
  'name': 'dev',
  'image': 'sha256:…',
  'shared': true,
  'runtime': 'runc',
  'workdir': '/workspace',
  'user': '1000:1000',
  'caps': { 'drop': ['ALL'], 'add': [] },
  'security':
    {
      'noNewPrivileges': true,
      'seccomp': 'default',
      'readOnlyRoot': true,
      'tmpfs': ['/tmp:rw,nosuid,size=256m'],
    },
  'limits': { 'memoryMb': 2048, 'cpus': 2, 'pidsMax': 512, 'shmSizeMb': 256 },
  'env': ['LANG', 'TZ'],
}
```

| Field                        | Type                    | Default        | Notes                                                                      |
| ---------------------------- | ----------------------- | -------------- | -------------------------------------------------------------------------- |
| `schema`                     | `'ghostai.container/1'` | —              | Required.                                                                  |
| `name`                       | slug                    | —              | Also the filename.                                                         |
| `image`                      | string                  | —              | **Must be digest-pinned.** `name@sha256:<64hex>` or a bare local image id. |
| `shared`                     | boolean                 | `false`        | Reuse one instance across agents in a workspace.                           |
| `runtime`                    | `runc \| runsc \| kata` | `'runc'`       | `runsc` is gVisor, `kata` a microVM.                                       |
| `workdir`                    | string                  | `'/workspace'` | Where the workspace is mounted. Absolute, and not `/`.                     |
| `user`                       | string                  | `'1000:1000'`  | `uid:gid` inside the container.                                            |
| `caps.drop`                  | string[]                | `['ALL']`      |                                                                            |
| `caps.add`                   | string[]                | `[]`           | `NET_ADMIN`, `SYS_ADMIN` and `SYS_MODULE` are never grantable.             |
| `security.noNewPrivileges`   | boolean                 | `true`         |                                                                            |
| `security.seccomp`           | `default \| unconfined` | `'default'`    | `unconfined` is surfaced to the operator, not refused.                     |
| `security.readOnlyRoot`      | boolean                 | `true`         |                                                                            |
| `security.tmpfs`, `.devices` | string[]                | `[]`           |                                                                            |
| `limits.memoryMb`            | int                     | `2048`         |                                                                            |
| `limits.cpus`                | number                  | `2`            |                                                                            |
| `limits.pidsMax`             | int                     | `512`          |                                                                            |
| `limits.shmSizeMb`           | int                     | `256`          |                                                                            |
| `env`                        | string[]                | `[]`           | Host variables passed through. Everything else is scrubbed.                |

**The image must be digest-pinned.** A tag is a mutable pointer, so a container installed
once and then repointed runs code nobody chose while every hash still matches. The
pattern is anchored at both ends: an end-only anchor would accept something like
`-v/:/hostfs@sha256:…`, and the image is pushed to the engine as a bare argv token.

**`NET_ADMIN` is never grantable.** The egress gateway's rules live in a network namespace
the container _shares_, and a container holding `NET_ADMIN` can flush them. This is refused
rather than surfaced because it breaks an invariant the rest of the system relies on.

**`seccomp: unconfined` is deliberately not refused.** It is genuinely risky and genuinely
required for rootless builds, so it is surfaced in the install review and left to the
operator. The rule of thumb: refuse what silently breaks the machinery, surface what is
merely dangerous.

**The image needs `setsid`.** Every command runs through it so a timeout reaches the whole
process tree rather than the leader alone. On Debian and Ubuntu that is `util-linux`;
BusyBox provides it already.

### tmpfs is memory

`security.tmpfs` and `limits.memoryMb` are not independent budgets. A tmpfs is RAM-backed
and its pages are charged to the container's memory cgroup, so a container with `/tmp` at
512m and `memoryMb` at 1024 has half its memory reachable by writing files.

It is easy to miss, because the default Docker behaviour hides it: without
`--memory-swap`, swap is twice the memory limit, so tmpfs pages get swapped and a write
past the limit succeeds slowly instead of failing. With swap disabled the same write is
OOM-killed. Size `/tmp` for what a job actually spills, and count it against `memoryMb`.

Work belongs in the workspace anyway, a bind mount on real disk outside this budget
entirely. `/tmp` is for what a program does behind your back.

## Binding an agent

```yaml
"toolbox": {
  "name": "recon",
  "tools": { "*": "deny", "nmap": "allow", "dnsx": "allow" }
},
"container": {
  "name": "security-tools",
  "network": { "mode": "allowlist", "hosts": ["deb.debian.org"] }
}
```

`toolbox.tools` narrows the manifest's grants and can only tighten them. `*` stands for
every grant the map does not name, which is what makes "only these two" one line instead
of twenty denials. A `deny` is an **absence**, not a refusal at call time: the tool is
never sent to the model, and the prompt section does not mention it either. That is the
point, because the cost it saves is the schema it would otherwise carry on every request
of every turn.

Leaving `container.name` empty runs command operations on the host, inside the workspace
jail. That still constrains which operations are callable; it does not claim OS isolation.

### The scope is complete

An agent with a toolbox can call its grants and **nothing else**. There is no `exec` to
reach a program the toolbox did not grant, no `read_file`, no `memory`, no `skill`, and no
ambient MCP or extension tool. A toolbox that wants a built-in back grants it explicitly,
as a `registered` operation pinned to that tool's definition digest, so "this agent may
read files" is a line in a reviewed manifest instead of a default nobody chose.

Delegation is the one exception, and it is not an exception to the scope: a subagent's
delegate tool comes from the agent's own `subagents` bindings and is appended after the
scope, so a toolboxed agent can still delegate.

Drift is checked **during** a call, not only before it. A turn can run for minutes, and an
operator who edits a toolbox mid-run means it now. A 250 ms ticker re-resolves the
definition beside the running command and cancels the moment its digest stops matching the
one the call was prepared under.

## Network

Egress is configured in exactly one place: the agent's `container.network`.

| Field   | Type                        | Default  | Notes                                          |
| ------- | --------------------------- | -------- | ---------------------------------------------- |
| `mode`  | `none \| allowlist \| open` | `'none'` |                                                |
| `allow` | string[]                    | `[]`     | CIDR blocks, enforced by the gateway's filter. |
| `hosts` | string[]                    | `[]`     | Exact DNS names, enforced by the egress proxy. |
| `dns`   | string[]                    | `[]`     | Resolvers, as non-loopback IP literals.        |

`allowlist` runs through a separate gateway container whose network namespace the tool
container shares, with default-deny nftables rules. There is no interface in the tool
container to route around, and no host firewall table is touched.

`allow` and `hosts` are **alternatives, not layers**, and naming both is refused. CIDRs are
enforced in the packet filter and are the only thing that works for traffic that is not
HTTP; hosts are enforced by the proxy, which sees the _name_ rather than an address DNS
rebinding chose. A request enforced in two places is enforced in neither.

A CIDR allow-list needs at least one `dns` entry, and it must not be loopback: an engine's
embedded resolver answers on an address inside the shared namespace, so a rule naming it
would match traffic this filter never sees. A host allow-list needs no resolver, because
the proxy resolves on the container's behalf.

Four things about a container decide whether a restricted allow-list can be enforced in it
at all, and each is refused at save time rather than at the first command:

- a root or non-numeric `user`, because the gateway filters by the socket's owning uid;
- `user` 65532, which the egress proxy reserves for itself;
- `noNewPrivileges` off, because a process that can gain privileges can become the uid the
  gateway trusts;
- `NET_RAW`, `SETUID` or `SETGID`, which forge packets or change uid past a filter that
  matches on either.

None of these is refused at _install_: a root uid is how a rootless builder works, and
`NET_RAW` is what `nmap -sS` needs. They are legitimate for a container that reaches
nothing. `ghostai container list` and the settings screen report the sentence in advance,
so the choice is visible before a save fails.

**Egress is agent configuration, so a settings save can set `open`.** That is a deliberate
change from having it capped by the manifest: one configuration point was worth more than
a second ceiling. The hardening that remains operator-only is everything in the
container definition — the image digest, the capabilities, the seccomp profile, the uid,
`noNewPrivileges` and the shared flag — and a gateway refuses to start for a container
whose hardening cannot support restricted egress.

## Sharing and instances

With `shared: true`, agents and sessions in one workspace that ask for the **same
egress** reuse one serialized instance. The effective network is part of the instance key,
so two agents asking for different egress from one shared definition get two instances
rather than one of them silently getting the other's reach. Different workspaces never
share. With `shared: false` the instance is private to the agent, workspace and session.

They share workspace state, not permissions: every operation is re-authorised against the
calling toolbox immediately before dispatch.

Instances are pooled — at most four live at once, reaped after ten minutes idle. When
every live instance is busy, a new one is **refused** rather than made room for: evicting
a container with a command still running in it would kill work somebody is waiting on.

An instance the daemon has lost — restarted underneath, removed by hand — is rebuilt once
and the command retried, which is safe because a command that could not find its container
never started. An instance an operator **stopped** is not rebuilt: the two are told apart
by a counter the stop increments and a daemon restart does not.

**A container that fails to start is a refusal, never a downgrade to the host.** An agent
configured to run in one does not quietly get a shell on your machine because Docker was
not running.

## Inside the container

- Only the workspace is mounted from the host, read-write, at `workdir`. Everything else
  in the filesystem disappears when the instance is reaped.
- Output too large to return inline is kept in full under `/run/ghost-runs/<container>/`,
  read-only and outside the workspace. It is scoped to that one instance, so a shared
  container does not hand one agent another's transcripts. When there is nothing to mount,
  an empty read-only tmpfs takes the path instead, so nothing inside can populate a
  directory and read it back as though the host had written it.
- On Docker, `/sys/firmware` and `/sys/class/block` are masked with read-only tmpfs, so a
  command cannot read the host's firmware tables, its disk models or its filesystem
  UUIDs. Those two are masked and no others because every masked path has to exist on
  every architecture: a tmpfs over one that does not makes the runtime try to create the
  mountpoint inside a read-only `/sys`, and the container never starts. `/sys/class/dmi`
  is x86-only for exactly that reason. Podman copies a sysfs directory up into the tmpfs
  laid over it, which needs a capability this container does not hold, so the masking is
  asked for only where it works.
- The file tools, when a toolbox grants them, always act on the workspace on this machine
  through the jail. What they call `notes/todo.md` is `<workdir>/notes/todo.md` to a
  command.

## Digests, and what they are for

There is no second file recording consent. What each definition carries is a **digest**,
and it is identity rather than consent:

```bash
ghostai toolbox list
ghostai container list
```

A toolbox's digest covers the manifest **and every definition it names**, length-framed so
two definitions whose bytes could be split differently cannot hash alike. A container's
digest covers its definition alone. The two are independent: editing a container does not
move a toolbox's digest.

That digest does three jobs. It is part of a container instance's identity, so two
definitions that differ never share a warm instance. It travels with each call, so a
definition edited while a command is running cancels that command with a sentence naming
the drift. And it is what the idle sweep compares, so a warm container whose definition
moved is torn down rather than reused.

Two failure modes get two different sentences, because "not installed" and "installed but
it does not parse" are different things to do next.

The definitions are files rather than database rows so the sandbox service can reach the
same answer. It owns the container engine and the app does not; both read this directory
and neither writes the other's state. A row in the app's database would have to be told to
the service over the socket, which would make the app the authority on what the service is
allowed to run. Mount the policy directory read-only into both processes.

A definition that changes under a running call cancels it. Stopped calls are never
replayed.

## Building and installing

Definitions are built from the
[`GhostAI-presets`](https://github.com/therezor/GhostAI-presets) repository. Ordinarily you
never do this by hand — picking an agent builds what it needs:

```bash
ghostai preset install
```

A catalogue entry carries `containers/<name>/{Dockerfile, container.yaml}`, the toolbox at
`toolboxes/<name>.yaml`, and every `tool-definitions/<def>.yaml` the toolbox references.
Installing runs `docker build --iidfile`, checks the result is a real `sha256:` image id,
substitutes that id into the definition, and writes it to `containers/<name>.yaml`. The
toolbox and its definitions are copied verbatim — their bytes are what the digest covers,
so rewriting them would move it.

**The image is referenced by its image ID, not a registry digest.** An image ID is the
content hash `docker build` produces: a content address, exactly as unrepointable as a
registry digest, and available on a machine with no internet. That is what makes this work
on an air-gapped install.

Installing is the decision. The run prints what each definition asks for — its network
ceiling, its limits, any hardening it switches off — so the manifest is visible as it
lands, and `ghostai toolbox list` and `ghostai container list` print it again afterwards.
A second install of an unchanged definition rebuilds nothing, because rebuilding would
move the image id and so the digest, restarting every warm instance of it for no reason;
`--force` rebuilds anyway.

## Agent presets

A toolbox is a capability surface; the agent that uses it is config. That config is a
preset, a YAML file named for the agent id it installs, from `~/.ghostai/presets/` or the
catalogue's `agents/`:

```bash
ghostai preset install researcher   # builds what it needs
ghostai agent install researcher    # config merge only; definitions must be installed
```

Every preset lives in that one directory whether or not it names a container, because an
agent that works in one is not a different kind of agent — it is an agent whose
`toolbox.name` is set.

A preset carries the agent's `systemPrompt`, its tool permissions, its toolbox reference
and its container reference. It deliberately cannot carry a model, a provider, or anything
from a manifest's side of the boundary: the shape is a strict subset of an `agents.list`
entry, so a preset can express nothing a settings save could not. See
[CLI](cli.md#ghost-agent) for the full resolution order.

Install refuses a preset whose toolbox does not resolve, because the server would refuse
to boot on the result, and refuses to overwrite an existing agent without `--force`.

## Why the exec guard relaxes inside a container

On the host, `guard_exec` refuses shell binaries and refuses path arguments pointing
outside the workspace. Inside a container both restrictions lift, and they lift
**together**, which is the point.

Both exist to enforce by inspection what a container enforces by construction. A container
that mounts only the workspace has nothing else to point at, so refusing `../etc/passwd`
protects nothing; and with the filesystem already bounded, a shell is just the ergonomics
of running two commands. Lifting one without the other would be a real weakening; lifting
both, given the mount set, is not.

Everything else still applies: argv is still argv, the environment allow-list still holds,
the output budget is still enforced as the process writes, and the call is still gated by
the agent's tool permissions.

## Running it

See [Sandbox service](sandbox-service.md) for deployment and instance management:

```bash
ghostai sandbox health
ghostai sandbox list
ghostai sandbox start --container dev --workspace default
ghostai sandbox stop <instance>
ghostai sandbox restart <instance>
```

Ready examples live under `deploy/sandbox/examples/`.
