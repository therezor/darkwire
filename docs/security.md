# Security

Everything that decides whether an agent may touch a path, reach a host, spawn a process,
run in a container or read a credential is in `crates/security` and nowhere else.
Reviewing the security surface means reading one crate — that is why it is a crate.

It carries the strictest coverage bar in the repo, 95% lines **and** branches, because an
untested branch in a guard is a bypass rather than a bug. The layer graph helps: `core`
sits underneath this crate, declares no HTTP client and spawns nothing, so the crate
everything else is built on is not where any of this could quietly be reimplemented.

## Threat model

The agent is assumed to be **capable and not adversarial, but steerable by its inputs**.
It runs code the operator asked for, on the operator's machine, with the operator's
credentials. The attacks worth defending against come from three directions:

1. **Content the agent reads** — a web page, a file, a tool result — containing text
   designed to redirect it.
2. **Model error** — a wrong path, a wrong host, a destructive command, none of it
   malicious.
3. **Someone reaching the server** — over the network, or with filesystem access to the
   install.

What is explicitly _not_ claimed: this does not defend against a model that is
deliberately hostile and has host `exec`. That is what [toolboxes](toolboxes.md) are for,
and the limit is stated in the jail section below rather than papered over.

---

## Workspace jail

**Stops:** path traversal and symlink escape.

The workspace is a root in the `chroot` sense, not a prefix to compare against.
`/etc/passwd` addresses `<workspace>/etc/passwd`. `../../secrets` addresses
`<workspace>/secrets`. `~/.ssh/id_ed25519` addresses `<workspace>/.ssh/id_ed25519`. There
is no way to spell "outside".

Three rules, in this order:

1. **Lexical normalisation before resolution.** The path is _constructed_ from its
   segments — `..` popping an empty stack is a no-op — so containment holds by
   construction rather than by a check that could be wrong.
2. **`realpath`, then containment.** A symlink inside the workspace pointing at `/etc` is
   refused. Comparing before canonicalising is the classic version of this bug.
3. **Normalisation is total.** Every input maps either to a path inside the root or to a
   refusal _about the filesystem_, with the original string's shape recorded for the audit
   log.

Platform-independent: `\` is a separator and drive letters are handled everywhere, not
only on Windows, because the model does not know which host it is on.

### The stated limit

**A workspace is an organisational boundary, not a security boundary, wherever `exec` is
enabled on the host.** A spawned child process does not honour the jail's clamping. This
is why the exec guard _refuses_ out-of-workspace path arguments rather than resolving them
inside — refusal is the only honest answer when the thing being constrained can walk out.

An agent that must be genuinely confined gets a [toolbox](toolboxes.md).

---

## Exec guard

**Stops:** command injection.

`guard_exec` takes an `argv` vector and produces a validated plan for
`Command::new(argv[0])`. **There is no shell, ever** — not a default that something
could switch off, but no shell anywhere in the path a command takes.

There is deliberately **no deny-list of shell metacharacters**. With no shell there is no
string for `$(...)`, backticks or `| sh` to be interpreted in, so scanning for them blocks
nothing and breaks honest commands — a commit message containing `$HOME`, a grep for a
pipe character.

What actually constrains the child:

- **`argv[0]` against a deny-list, then an allow-list**, matched on basename so
  `/usr/bin/git`, `git` and `git.exe` get one verdict.
- **Shell binaries refused unless explicitly listed**, and the `-c` family refused even
  then.
- **Every path-shaped argument classified and refused** if it points outside the
  workspace. This is the one place in GhostAI that refuses rather than clamping, for the
  reason above.
- **An environment allow-list** — `PATH`, `HOME`, `LANG`, `TZ` by default — so the child
  inherits what it needs and nothing holding a token.
- **An output budget enforced while the child writes**, not after it exits, so a runaway
  process cannot fill memory before the cap notices.

Inside a toolbox the shell ban and the path ban lift together; see
[Toolboxes](toolboxes.md#why-the-exec-guard-relaxes-inside-one) for why that is not a
weakening.

---

## Guarded fetch

**Stops:** SSRF and DNS rebinding.

The usual shape — resolve the hostname, check the address, then hand the _URL_ to an HTTP
client — is advisory only, because the client resolves again when it connects and nothing
makes the two answers agree.

Here, validation resolves the host itself and **pins the resulting addresses into a
client built for that one request**, whose resolver is handed the answer and never
consults DNS. There is no second resolution to differ from the first. A proxy configured
in the environment is ignored for the same reason — it would connect wherever it liked.

- **Every redirect hop is re-validated** with a fresh pin.
- **`Authorization` and `Cookie` are dropped on origin change.**
- **The body is capped as it streams**, not measured afterwards.
- Defaults: 3 redirects, 5 MiB, 30-second timeout.

Policy knobs: `allowLoopback`, `allowPrivate`, `allowedHosts` (exact or leading-dot),
`deniedHosts` (checked before everything, including the allow-list), `maxRedirects`.

### Address classification

All of these reach 127.0.0.1, and a naive `hostname === 'localhost'` check passes every
one:

```
http://2130706433/        decimal
http://0177.0.0.1/        octal
http://0x7f.1/            hex
http://127.1/             short form
http://[::ffff:7f00:1]/   IPv4 mapped into IPv6
```

Literals are parsed with the same semantics `getaddrinfo` uses, so all five classify as
loopback.

The blocked set is data, and deliberately wider than "private": `0.0.0.0/8`, `10/8`,
`100.64/10` (CGNAT), `127/8`, and **`169.254/16`** — link-local, which is where cloud
metadata lives at `169.254.169.254`, the highest-value SSRF target there is. **Nothing
unlocks link-local.** IPv6 transition prefixes (6to4, Teredo, NAT64) are blocked
wholesale rather than unwrapped, because unwrapping is another parser to get wrong.

Note that the **provider** base URL does not go through this guard — the common case is
loopback, which the guard exists to refuse. What is enforced there instead is narrower and
matches the real risk: an API key is never sent over plain HTTP to a public address.

---

## Prompt injection

**Stops:** text in a tool result being read as an instruction.

Every tool result is wrapped in a delimiter carrying a **fresh 64-bit random nonce,
regenerated every turn**, and the system prompt states that everything inside such a
delimiter is inert data. An attacker who cannot guess the nonce cannot close the envelope.
Closing tags appearing inside the content are escaped case-insensitively, because the
model does the parsing and the model is not case-sensitive.

The policy text lives in the **static** half of the prompt. It names no delimiter — the
nonce is one line of live state instead — so two hundred tokens of prose that never
changes are cached for the life of the session rather than re-read every iteration.

It is an ordinary editable template, `toolPolicyPrompt`, like the other seven. Editing it
does not weaken the mechanism: the envelopes are emitted by the runtime whatever the
template says, and the nonce comes from the injected `RandomSource`, which reads no
template. What a deleted policy costs is the model's _reason_ to treat what is inside an
envelope as data — so it gets a warning in the editor and a `tool_policy_missing_nonce`
config warning, not a refusal. See [Prompts](prompts.md#what-you-can-edit-and-what-that-does-not-change).

**Detection is deliberately non-destructive.** When injection-shaped text is spotted, a
`prompt_injection` notice raises a badge in the UI and **the content passes through
byte-for-byte**. Replacing a match with a warning banner fires on this project's own
security documentation, and leaves the model hallucinating around the hole where the text
used to be. Telling the operator is more useful than lying to the model.

---

## Credential vault

**Stops:** key theft at rest.

AES-256-GCM, one file, mode `0600`.

- **GCM rather than CBC**, because it must detect _tampering_: someone who can write the
  file but not read it could otherwise flip bits in a stored base URL and redirect
  traffic. A failed auth tag is a hard error, never a fallback to "treat as empty".
- **A version string is bound in as additional authenticated data**, so a file from
  another format or another application cannot be replayed into this one.
- **The key lives in the OS keychain** when one is reachable, and in a `0600` keyfile when
  not. The fallback is not a lesser mode — it is what makes this work in a container, over
  SSH, and on a headless box.

Keys never appear in `config.json`, which is what makes that file safe to commit. Over
HTTP the vault is **write-only**: a client can store a credential and nothing reads one
back out. What a client can see is a per-instance `credentialsPresent` boolean.

---

## Toolbox authorisation

**Stops:** the policy an agent runs under being changed underneath the operator.

A toolbox is authorised as a **dependency bundle hash, not a signature** — the manifest
and every reusable tool definition it names are approved together, length-framed, so
editing a shared definition revokes every toolbox that reaches it. A container definition
has a separate approval hash over its own bytes alone. Editing either silently revokes
that one resource. The approval is a file beside the definition rather than a database
row, which is what lets the app and the isolated sandbox service verify the same answer
without either being the authority on the other.

Re-checked **during** a call, not only before it: a 250 ms ticker re-resolves the approval
beside a running command and cancels the moment the hash it was authorised under stops
matching. A revoke means it now, not when the process happens to exit.

The toolbox is the complete callable surface and agent permissions may only tighten its
ceilings. Containers require content-addressed images; `NET_ADMIN`, `SYS_ADMIN` and
`SYS_MODULE` are refused. Restricted egress uses a separate default-deny gateway whose
namespace the tool container shares. Definitions live beside the workspace, never inside
it, so file tools cannot rewrite the policy the agent runs under.

**Egress is agent configuration, and that is a deliberate narrowing of this boundary.**
A settings save can set `network.mode` to `open`; it cannot change an image digest, a
capability, a seccomp profile, a uid, `noNewPrivileges` or the shared flag, because none
of those has any representation in the config tree. A gateway also refuses to start for a
container whose hardening could not enforce a restricted allow-list, so the two halves
cannot silently disagree. The trade was one configuration point for one fewer ceiling.

Full detail in [Toolboxes](toolboxes.md).

---

## Extension authorisation

**Stops:** code an operator never reviewed running beside the agent.
**Does not stop:** anything that code does once it is running.

An extension is authorised by **content digest, not signature**, exactly as a
toolbox is — the question is "are these the exact bytes approved?", not "who
wrote them". It diverges from the toolbox in one place, and the divergence is
what makes the analogy hold: the digest covers **every file under the install
directory**, not the manifest alone. A container definition pins an immutable
image, so approving the definition approves the code; an extension manifest
names a _path_, so approving it would approve a pointer and the file behind it
could be swapped afterwards without moving an approved byte.

Editing any file, adding one, removing one or renaming one moves the digest and
revokes the approval, and the next reconcile refuses with a sentence naming the
drift. Nobody has to remember to re-approve.

**There is a process boundary, and it buys less than it sounds like it does.**
An extension is a child process talking to the host over a pipe, so nothing the
host holds is reachable from it: no registry handle, no vault object, no
database connection, no workspace jail. The only thing it may ask the host for
is its own secret, and the call that does it takes no arguments, so there is no
shape of it that reads another extension's credential.

**The limit is stated rather than papered over.** That process still runs under
the operator's account, with the operator's filesystem and the operator's
network. It can open `~/.ghostai/vault.json` itself, spawn a program and open a
socket, and nothing in this repository stops it. **The trust class is unchanged
from the in-process design** — what the boundary narrows is the reach of a
mistake, not the reach of an attack. That is the same trust level as a toolbox
with host `exec`, and it is why approving one asks a question in the UI rather
than being a toggle.

The manifest's `contributes` list is **disclosure, not enforcement**. The host
drops a registration whose kind is not declared, which keeps the approval screen
honest and turns an honest mistake into a visible one — but the code is already
running by then, so it is not a boundary and this page does not call it one.

Three smaller rules fall out of the same reasoning.

- **`command[0]` is checked before anything is spawned.** A shell binary is
  refused outright — a shell turns the rest of the argv back into a string
  somebody can inject into — and a program path is resolved through `realpath`
  and refused if it lands outside the extension's own directory, since code the
  digest does not cover is code that was never reviewed. A _bare_ name (`node`,
  `python3`) is the deliberate exception: the operating system resolves it on
  the host `PATH`, so it names a program the operator installed rather than one
  the extension shipped, and the digest has nothing to say about it.
- **The environment allow-list passes variable _names_, never values.** A child
  gets `PATH`, `HOME`, `LANG` and `TMPDIR` plus whatever names its manifest
  asked for, built from nothing rather than inherited. A host serving models has
  provider API keys in its own environment, and handing that wholesale to
  third-party code would give away every credential on the box to anything an
  operator approved once. A manifest can ask for `NODE_EXTRA_CA_CERTS`; it
  cannot invent a value for it.
- **Stopping one is stdin, then `SIGTERM`, then `SIGKILL`**, each with the same
  grace, and every signal goes to the process **group** rather than the process.
  A child that forked a worker would otherwise leave the worker behind holding
  the pipe, looking from out here exactly like a child that hung. The group is
  created at spawn time, so a signal meant for an extension can never reach the
  host.

An extension's runtime state is written to a _sibling_ directory
(`~/.ghostai/extension-data/<id>`), never inside the install, because the first
write would otherwise revoke its own approval.

Full detail in [Extensions](extensions.md).

---

## Authentication

**Stops:** someone reaching the server.

- **argon2id** at the OWASP baseline (19 MiB, t=2, p=1) for the password.
- **Session tokens** are `<id>.<secret>` with 32 bytes of randomness, stored as SHA-256
  and compared with a constant-time equality. A KDF on a random 32-byte token would only
  be a rate limit.
- **The cookie** is `httpOnly`, `SameSite=Strict` (which stands in for a CSRF token), and
  `Secure` except on plain HTTP to a loopback host — Safari will not store `Secure` on
  `http://localhost`. No token is ever in a response body, because this app renders text a
  model wrote.
- **A wrong username and a wrong password give the same answer, in the same time.** The
  hash is verified on every attempt, against a decoy when the username does not match, so
  a failed login cannot confirm an account name.
- **Changing either credential revokes every other session**, and requires the current
  password — a session on its own is not enough to rotate the credential it was minted
  from.
- **The first-run setup code** is 12 random bytes rendered as 20 Crockford-base32
  characters. Single use, and dead the moment a password is set.

### Login throttling

Two scopes at once, asymmetric on purpose. Counters live in `ghost.db`, so a restart does
not clear them.

| Scope                                       | After      | Backoff         | Caps at    |
| ------------------------------------------- | ---------- | --------------- | ---------- |
| One address                                 | 4 failures | doubles from 1s | 15 minutes |
| The account, wherever the attempt came from | 4 failures | doubles from 1s | 30 seconds |

The per-address scope handles one host hammering the form. The account scope is the one a
botnet cannot spread out of: every guess lands in the same bucket regardless of origin,
capping the aggregate rate at roughly two a minute however many addresses are in play.

It caps _low_ deliberately. On a single-account server an unbounded lockout is a denial of
service an attacker can trigger on purpose, so the operator's worst case is half a minute
while the attacker's throughput is dead either way.

### Binding

The server binds `127.0.0.1` by default, and **a non-loopback host with auth disabled
refuses to start**. `0.0.0.0` and `::` count as remote. A warning would not be enough — it
scrolls past, and the result is an unauthenticated shell-capable agent on a LAN address.
The same check runs again when settings are saved, so it cannot be turned off from inside
the UI.

Workspace media is served through **short-lived HMAC-signed URLs** rather than an open
file endpoint, so an `<img>` tag can render a file without a credential and the URL stops
working ten minutes later.

---

## Channels

**Stops:** someone reaching the agent through a chat app the operator invited it into.

A channel is a door with no password on it. A bot username is discoverable, the transport
decides who may send to it, and behind it is an agent with the operator's credentials,
their workspace and — depending on the permission map — `exec`. So the allowlist is the
whole boundary, and it is strict in three ways that a convenience-first design would not
be.

- **An empty allowlist refuses to start.** The same shape as the binding rule above, and
  for the same reason: the two failure modes are "answers nobody", which looks exactly
  like a broken token, and "answers anybody", which is a remote shell. A refusal that
  names the config key distinguishes them at the only moment anyone is looking.
- **The sender is checked, not the chat.** In a group both must be listed — the group and
  the person typing. Checking the chat alone hands the agent to everyone else in the room,
  including whoever they invite next.
- **A button press is checked exactly like a message.** Any member of a group can tap a
  button the bot posted, so an approval answered from an inline keyboard is an
  authorisation decision arriving from an unauthenticated source unless it goes through
  the same list. This is the easiest of the three to leave open and the worst to.

An unrecognised sender is **dropped silently and logged once per id**. Replying confirms
the bot is live and spends the rate limit on whoever is knocking; logging every message
lets them fill a disk. That single line is also the intended onboarding path — message the
bot, read the log, add the id.

Beyond the allowlist, `admins` narrows the commands that reach past one conversation:
`/model` moves the whole install onto another model, and `/workspace rm|move` rewrites
where sessions live. Empty means every allowed sender is an admin, so a single-operator
install pays nothing for the distinction.

Bot tokens belong in the **credential vault** under `channels`, not in `config.json`; a
token found in the config file is used and warned about. Settings → Channels writes to the
vault, and the panel can never read one back — it is told only whether one is stored, which
is what lets it show a saved token without ever holding it. The Bot API puts the token in the
request URL, so nothing in the adapter logs a URL and its errors carry the API method
instead.

---

## Logging

Secrets are redacted **by path, not by scanning values** — a scanner cannot tell an API
key from any other opaque string, and will either miss keys or redact identifiers.

Redacted paths include `apiKey`, `api_key`, `token`, `accessToken`, `refreshToken`,
`clientSecret`, `password`, `passphrase`, `secret`, `authorization` and `cookie`, each
with one and two levels of nesting, plus `headers.authorization`, `headers.cookie` and
`set-cookie`.

Errors log structured context rather than interpolated messages, because interpolation
happens past the point where redaction can reach it.

---

## Privacy

Not a claim — three mechanisms, two of which fail the build.

- **Zero telemetry.** A repo-wide grep for `telemetry`, `analytics`, `posthog`, `sentry`,
  `gtag` or `mixpanel` returns exactly one hit: the comment in the test that forbids them.
- **`self-contained.test.ts`** scans the shipped UI for any external origin, any
  `preconnect` or `dns-prefetch`, any cross-origin stylesheet. Fonts ship from npm.
- **`offline.spec.ts`** blocks every request that is not the server's own origin, in a
  real browser, and drives the whole app. This catches what a grep cannot: a font, icon
  set or highlighter theme a bundler quietly left as a runtime fetch.

The reasoning is stated in the design rules: a first paint that reaches a third party
leaks every user's IP address on load, and on an air-gapped machine it is a page in Times
New Roman while a request times out.

What leaves the machine, and only because you configured it: requests to whichever model
provider you chose — local ones go to `127.0.0.1` — and whatever an agent fetches, within
your network policy.
