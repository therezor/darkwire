# Extensions

An extension is a directory of code an operator installed and approved. It adds
tools, channels, providers, prompt sections and slash commands to a running
install, and it runs as a **child process speaking JSON-RPC over its own
stdio** — which is why the approval is a content digest rather than a checkbox:
the manifest names a program, so approving the manifest alone would approve a
pointer.

```
~/.darkwire/extensions/hello/
  darkwire.extension.yaml     ← the manifest
  index.mjs                  ← whatever `command` runs
~/.darkwire/extension-data/hello/   ← what it writes at runtime
```

```bash
darkwire extension list        # what is installed, and what state each is in
darkwire extension approve hello
darkwire extension revoke hello
```

Or Settings → Extensions, which is the same three actions and reloads without a
restart.

## It is an MCP server

The wire is MCP's stdio transport, verbatim: newline-delimited JSON-RPC 2.0, one
JSON object per line, the `initialize` handshake, protocol revision
`2025-06-18`. "Verbatim" is the whole design, and it buys one specific thing —
**a plain MCP server is already a valid tools-only extension.** A server that
has never heard of DarkWire completes the handshake, answers `tools/list` and
`tools/call`, replies `-32601` to everything else, and the host registers its
tools and moves on. Nothing had to be written for us.

Everything past tools is namespaced under `darkwire/`, and the asymmetry in the
set is the security argument. The host asks an extension for **five** things;
an extension may ask the host for **one**.

```text
host  → ext   darkwire/context/static    {agentId}               → {sections}
host  → ext   darkwire/context/runtime   {agentId, sessionKey}   → {sections}
host  → ext   darkwire/commands/list     {}                      → {commands}
host  → ext   darkwire/commands/run      {id, args, sessionKey}  → {message, ok}
host  → ext   darkwire/channels/list     {}                      → {channels}
host  → ext   darkwire/channels/start    {channelId, settings}   → {}
host  → ext   darkwire/channels/send     {channelId, message}    → {}
ext   → host  darkwire/secret            {}                      → {value?}
ext  ~> host  darkwire/channels/publish  {channelId, …}          (notification)
ext  ~> host  darkwire/channels/control  {channelId, frame}      (notification)
```

`darkwire/secret` is the only ext→host _request_, and it takes no arguments on
purpose: an extension asks for "my secret", never for a namespace and a key, so
there is no shape of that call that reads another extension's credential. The
two notifications are the inbound half of a channel, and they are notifications
because nothing the host would answer is useful — a message that could not be
published is a host-side problem the extension cannot act on.

**There is no `darkwire/hello` handshake, because there does not need to be
one.** Everything the host tells an extension about itself travels in
`params._meta` on `initialize`, the field MCP reserves for implementation data:
its id, its settings block, its data directory and the host's version. A server
that ignores `_meta` is not broken — it is the tools-only case.

## The manifest

```yaml
{
  'schema': 'darkwire.extension/2',
  'id': 'hello',
  'version': '2.0.0',
  'label': 'Hello',
  'description': 'A reference extension: one tool, one prompt section, one command.',
  'command': ['node', 'index.mjs'],
  'contributes': ['tools', 'context', 'commands'],
}
```

| Field              | Type                     | Default                           | Notes                                                                         |
| ------------------ | ------------------------ | --------------------------------- | ----------------------------------------------------------------------------- |
| `schema`           | `'darkwire.extension/2'` | —                                 | Required. `/1` parses, and is refused with a sentence — see below.            |
| `id`               | string                   | —                                 | 1–40 lowercase alphanumerics and hyphens. **Must equal the directory name.**  |
| `version`          | string                   | `'0.0.0'`                         |                                                                               |
| `label`            | string                   | `''`                              | Shown in the UI. Empty falls back to the id.                                  |
| `description`      | string                   | `''`                              | One sentence, shown beside the Approve button.                                |
| `command`          | string[]                 | `[]`                              | The argv to spawn. Never a shell line. Empty on a `/2` manifest is a refusal. |
| `env`              | string[]                 | `['PATH','HOME','LANG','TMPDIR']` | Host variable **names** the child may additionally inherit. Never values.     |
| `providers`        | `ProviderSpec[]`         | `[]`                              | Provider types, as data. No code.                                             |
| `contributes`      | string[]                 | `[]`                              | `tools`, `channels`, `providers`, `context`, `commands`.                      |
| `engines.darkwire` | string                   | `''`                              | A semver range. It parses; nothing in this build enforces it.                 |

**The id and the directory name have to agree.** Neither side wins a
disagreement — it is refused — because the approval row is keyed by id and the
directory is how the extension is found, so letting either win would mean the id
an operator approved and the id the host registers under could differ.

**`command` is an argv, never a shell line.** Element zero is the program and
the rest are its arguments, exactly as they reach `execve`. `argv[0]` has two
legal shapes, and the split is on whether it contains a separator rather than on
whether it exists:

- **A bare name** — `node`, `python3` — is resolved by the operating system on
  the host `PATH`. It names a program the operator installed, not one the
  extension shipped, so the approval digest has nothing to say about it.
- **Anything with a separator** is a path into the install directory, held to
  exactly the containment rule the old `entry` was: lexical first, then
  `realpath`, because a symlink defeats the lexical half alone. An absolute path
  is refused outright.

A **shell binary** is refused either way, from the same list the exec guard
uses. An `entry` was always interpreted by Node; an argv is interpreted by
whatever `argv[0]` names, and a shell named there turns the rest of the argv
back into a program string somebody can inject into.

**`env` is an allow-list of variable _names_, never values.** A manifest cannot
set a variable, only ask for one the host already has — so a manifest may
request `NODE_EXTRA_CA_CERTS` and cannot invent it. The four defaults are enough
to find a program and behave like one run from a terminal, and none of them
could be a credential; a name the host does not have is simply absent in the
child, rather than an empty string a program reading `TMPDIR` would treat as a
directory. The host adds two of its own, `DARKWIRE_EXTENSION_ID` and
`DARKWIRE_EXTENSION_DATA_DIR`, which are the whole of what an extension knows
about its own installation before `initialize` arrives.

**A `darkwire.extension/1` manifest still parses.** It named an `entry`: a
JavaScript module a host loaded into its own process, and no amount of care
makes that contract into this one. So it is refused — but refused _late_, after
the manifest has produced an id and a label, so the operator gets a row naming
the extension and a sentence saying to rebuild it against `/2`, rather than a
log line naming a file. A manifest version this build cannot run still has to
produce the row that explains the refusal, which is why the two versions parse
into one shape rather than a discriminated union.

## What an extension may add

| Kind        | Contract                                                                                   |
| ----------- | ------------------------------------------------------------------------------------------ |
| `tools`     | MCP tool descriptors from `tools/list`. Bridged, registered under source `extension`.      |
| `channels`  | `darkwire/channels/list`, then `start`, `send` and the two inbound notifications.          |
| `providers` | `providers[]` in the manifest. Data, not code.                                             |
| `context`   | `darkwire/context/static` and `darkwire/context/runtime` — the seam Skills and Memory use. |
| `commands`  | `darkwire/commands/list` and `run`. A slash command, from the composer and the terminal.   |

**Tools go through the MCP bridge, unchanged.** Not a copy of it and not a
variant of it — the same bridge an MCP server's tools go through, with the
prefix `ext` instead of `mcp`. That is the whole of the difference between the
two, and it should stay the whole of it: one bridge, two prefixes. What the
bridge already decides is right here for the same reasons it is right there — a
schema it cannot advertise drops _that tool_ and leaves the rest working, a
remote failure is a tool result rather than a dead turn, and `readOnlyHint` is
the one annotation believed at face value.

**Registering a tool grants nothing.** It joins the registry and every agent
still decides for itself whether it may call it — `agents.list.<id>.tools`,
where an absent name means disabled. There is no permission vocabulary in the
manifest, deliberately: one reachable from a file an extension ships would be a
way to grant something the operator never enabled.

**A provider is manifest data now, and there is no adapter to write.**
Registering a provider used to mean handing the host a wire adapter — a
function — which an out-of-process extension cannot do, and which would route
every generated token through two extra hops if it could. So what an extension
contributes is the table entry and the host supplies the adapter it already
ships: an OpenAI-compatible endpoint needs no code at all, which is the common
case. `wire` is a plain string rather than an enum precisely so that a manifest
naming a wire this build has no adapter for still **installs**, minus the
provider it could not supply — an unknown wire is a warning on the extension's
row, not a manifest that will not load. Either way the result goes through the
resilience wrapper, so an extension's provider inherits retry, backoff and
timeout classification rather than reimplementing them.

**A context contributor is fetched once per turn, in the async half.** The
contributor seam's runtime method is synchronous and runs on every iteration,
so an RPC there would block a worker thread five or ten times a turn. Both
halves are fetched together, under their own caps, and the runtime one is cached
per session.

**A command answers with text, not a resource key.** Its copy ships with the
extension and the translation layer has never seen it. `ok: false` renders the
answer as an error rather than a note.

**`darkwire/channels/list` is not in the obvious design and had to be.** A
channel is registered as a _factory_ keyed by id and the manager builds it
before anything starts, so the host has to know the ids before the first
`start`. `contributes: ["channels"]` says that there are channels, not what
they are called.

## One namespacing rule

Every id an extension contributes is `<extensionId>` or
`<extensionId>-<suffix>`. A channel id becomes a session-key prefix, a provider
id becomes a `providers.<id>.type`, and a command id becomes what an operator
types after a slash — one character class across three registries, so two
extensions cannot silently fight over a name and an operator reading any of the
three can tell whose it is.

**A tool is the exception in spelling only.** Tool names have their own
character class, so `greet` is rewritten to `ext_hello_greet` on the way in —
the same 64-character cap and digest tail an MCP server's `mcp_<server>_<tool>`
gets, from the same flattener.

A registration that breaks the rule, or one whose kind `contributes` never
declared, is **dropped with a warning on the extension's row** rather than
failing the extension. An extension whose fifth tool is misnamed should install
the other four and say so.

The warning runs the other way too. Every list method is probed, not only the
declared ones, so a kind the manifest declares and the extension answers
`-32601` to earns "declares X but does not implement it" on the same row. Four
extra round trips at load time is the whole cost.

## What the host holds

`initialize` and the list methods fill a bag, and the host applies it
afterwards. That decision predates the process boundary and survived it
unchanged, because the three things it buys were never about being in-process:

- **Unload is exact.** The host holds what the extension turned out to
  contribute, so removing it removes exactly that. Nothing has to be diffed.
- **A partial activation installs nothing.** An extension whose `tools/list`
  answers and whose `darkwire/commands/list` dies leaves no trace — the bag is
  discarded whole. Registering each kind as it arrives leaves four tools
  registered by an extension that is not running.
- **Nothing an extension holds outlives it.** There is no handle to take back,
  because none was ever given out.

### Settings

`config.extensions.settings.<id>` reaches the extension in `initialize`,
**unparsed**, because the config schema cannot know its shape — the same
arrangement `config.channels.<id>` has. Parse it yourself in whatever language
the extension is written in, and refuse to finish `initialize` on a bad block:
that lands on the extension's row as `failed` with the message, which is where
an operator who mistyped it should read about it.

**Credentials do not go there.** Put a secret in the vault under the
`extensions` namespace keyed by extension id, and read it with
`darkwire/secret` — the same arrangement a channel's bot token gets, for the same
reason: `config.yaml` is a plain file that backups, dotfile repositories and
screen shares all reach.

## Writing one

`examples/hello-extension` is the whole contract, with no dependencies and
nothing to build: a manifest, an `index.mjs`, and `node:readline`. Five things a
first-time reader gets wrong, each of which the example shows rather than
describes:

- **stdout is the wire. Never print to it.** A stray `console.log` is a protocol
  error. Diagnostics go to stderr, which the host drains into its own log under
  a byte budget — and past the budget the logging stops while the _draining does
  not_, because a child whose stderr pipe fills up blocks on its next write and
  looks, from out here, exactly like one that hung.
- **Everything the host tells you arrives in `initialize`**, under
  `params._meta.darkwire`. There is no config file to find and no environment to
  read beyond the two variables the host sets.
- **`contributes` has to match what you answer**, in both directions.
- **Every id is namespaced.** The command is `hello-time`, not `time`.
- **Exit when stdin closes.** That is how the host asks a child to stop; it
  follows with `SIGTERM` and then `SIGKILL`, so a process that ignores the
  closed pipe is killed rather than waited for.

**There is no build step, and no bundler settings to get wrong.** The child's
working directory is its install directory, so an extension written in Node
resolves its own files and its own `node_modules` the way any program does. An
install directory may carry a dependency tree — what bounds it is the digest,
not a rule about bundling: every regular file under the directory is hashed, and
the walk is capped at 4,096 files and 64 MB. A tree that blows through that is
refused with a message saying so, which is the difference between "this is not
shaped the way extensions are shaped" and a boot that takes ninety seconds for a
reason nobody can see.

One last thing worth copying from the example: nothing in it writes to the data
directory on startup. An extension that does fails on an install where that
directory does not exist yet, so state is written lazily or not at all.

## The five states

| State        | Means                                                |
| ------------ | ---------------------------------------------------- |
| `ready`      | Running, handshake complete, contributions applied.  |
| `unapproved` | Discovered, never approved.                          |
| `drifted`    | Approved once; the bytes on disk have changed since. |
| `disabled`   | Named in `extensions.disabled`.                      |
| `failed`     | Approved and enabled, and its process is not up.     |

Four of the five are reasons it is _not_ running, and each is distinct because
each has a different fix: approve it, re-approve it, enable it, or repair it.
Collapsing them into one `failed` would put an operator back to reading logs,
which is the state the row exists to replace.

`failed` covers three ways a process can fail to be up, each with its own
sentence on the row: it could not be spawned at all, it did not finish
`initialize` within ten seconds, or it died — carrying the exit status or the
signal that killed it. A crash while `ready` lands here too: the bag is dropped
whole and the tool set is announced as moved.

**A crashed extension is restarted once**, five seconds later, and then left
alone until its digest moves or an operator reconciles. A process that dies on
startup would otherwise be restarted forever.

**A broken extension never stops a boot.** A manifest that will not parse, one
nobody approved, a handshake that dies on the first frame — each is a row with a
sentence beside it. An install that refuses to start because one extension of
five is broken is a worse outcome than one that runs with four.

**A newly discovered extension has no row for a moment.** A reconcile returns
before a child has finished its handshake, which is deliberate: no row at all is
better than a misleading one, and an extension that was already running keeps
the row it had until the new answer arrives. The ten-second handshake cap is
what bounds the gap.

## Approval

**The digest covers every byte of the install directory**, not the manifest. A
environment definition pins an immutable image, so hashing the definition hashes
the code; an extension manifest names a _path_, so hashing it would approve a
pointer. Editing any file — including the one `command` runs, adding one,
removing one, renaming one — moves the digest and revokes the approval
automatically. Nobody has to remember to re-approve, because they cannot avoid
it.

Bounded at 4,096 files and 64 MB, refused above with a message saying so.

**Reloading code does not need a restart.** An edited extension reads `drifted`,
which kills the process and holds the row; approving the new bytes and
reconciling starts it again from them. There is no module cache left to defeat,
which is also why a `failed` row is retried rather than held until its digest
moves. The live-lock that guard used to prevent is stopped instead by only
announcing a row that actually changed — which is why a failure sentence never carries a
pid or a timestamp. It would differ on every pass and announce a change that did
not happen.

**Drift is noticed at the next reconcile, and the two surfaces say so
differently.** `darkwire extension list` reads the directory every time it runs,
so it reports `DRIFTED` the moment a file changes. `GET /api/extensions` and the
Settings panel report what the _server has loaded_, which is still the old copy
until something reconciles — a settings save, an approve or revoke, or a
restart. That is the same split `GET /api/settings` and `GET /api/mcp` make, one
layer along: the CLI is answering "what is on disk" and the panel is answering
"what is running", and both are true.

### What this does and does not buy

It answers **"are these the exact bytes the operator reviewed?"** and nothing
more.

The process boundary is real and worth being precise about. Nothing the host
holds crosses the pipe: there is no registry handle, no vault object, no
database connection and no jail on the other side, and the only thing an
extension may ask the host for is its own secret. But the process it runs in is
an ordinary process under the operator's account, with the operator's
filesystem and the operator's network — it can open `~/.darkwire/vault.json`
itself, spawn a program and open a socket, and nothing here stops it. **The
trust class is unchanged from the in-process design**; what changed is the reach
of a mistake, not the reach of an attack. That is the same trust level as an
agent with host `exec`, and
[Security](security.md#extension-authorisation) states it rather than papering
over it.

`contributes` is **disclosure, not enforcement**. It is what the approval screen
shows, and the host drops a registration whose kind is not listed — which keeps
the declaration honest and stops an honest mistake becoming an invisible one. It
is not a boundary, because the code is already running.

## Configuration

See [Configuration](configuration.md#extensions). The short version:

```yaml
{
  'extensions':
    {
      'load': ['/opt/corp-extensions/audit'],
      'disabled': ['hello'],
      'settings': { 'hello': { 'greeting': 'Ahoy' } },
    },
}
```

`load` takes a **path**, never a package spec. Nothing here fetches: an extension
is a directory an operator put on the box, which is what keeps an air-gapped
install air-gapped. An explicit path wins over an installed extension of the
same id, because it is the more specific statement — a scan is what happens to
be there and a `load` entry is something an operator wrote down.

## Checking one

The conformance suite runs the real host against a real child:

```bash
cargo run -p darkwire-extension-host --example check --features testkit -- \
  ~/.darkwire/extensions/hello --tools 1 --commands 1 --context 1
```

It catches what an extension's own tests structurally cannot, because three of
the four rules are only visible from the host's side of the pipe: that the
handshake completes and completes within the cap, that every kind the manifest
**declares** answers its list method, that every kind the extension **answers**
is one the manifest declared, and that every id it hands back is namespaced. A
red check in your own repository is a better place to find that than a warning
on somebody's settings panel.

It copies the directory into a temporary tree before running, because the
approval digest covers every byte under an install and a working checkout has
build output, editor state and dependency trees in it that a shipped extension
would not.

`examples/hello-extension/test/conformance.test.ts` is one assertion that shells
out to exactly the command above, and skips when `cargo` is not installed — a
JavaScript author editing `index.mjs` should not be required to have a Rust
toolchain, and CI has one.
