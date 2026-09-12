# Security

## Reporting a vulnerability

Report privately through GitHub's
[Report a vulnerability](https://github.com/therezor/GhostAI/security/advisories/new)
form. Please do not open a public issue for anything exploitable.

Include what you need to make it reproducible: the version (`ghostai --version`), the
configuration that matters (with credentials removed — `config.json` never contains any),
and what an attacker gets. A proof of concept helps more than a description.

You should get an acknowledgement within a few days. This is a small project, not a
company with a rota — if a week passes with nothing, assume the mail was lost and ping the
advisory again rather than assuming it was ignored.

## Supported versions

The latest release. There is no long-term support branch: fixes go into the next version
rather than being backported.

## What is in scope

Anything that lets someone do something the operator did not authorise, in particular:

- Escaping the workspace jail with `exec` **disabled**.
- Reaching a host `guardedFetch` should have refused — SSRF, DNS rebinding, a redirect
  that crosses an origin carrying credentials.
- Reading a credential back out of the vault over HTTP, or out of a log.
- Bypassing the approval gate: an `ask` tool that runs without the operator answering.
- Running an unapproved toolbox or extension, or getting an approval to survive a change
  to the bytes it covers.
- Authentication bypass, session fixation, or defeating the login throttle.
- Escaping a toolbox container, or widening its network mode beyond the manifest's
  ceiling.

## What is out of scope, and why

These are stated limits rather than unknown gaps, and each one is documented where the
guard is. Reporting one gets you a link back to this list.

- **The workspace jail where host `exec` is enabled.** A jail clamps the file tools; it
  cannot clamp a process. A spawned command is an ordinary process with your user's
  permissions and does not honour the workspace root. The workspace is an organisational
  boundary there, not a security boundary — which is what
  [toolboxes](docs/toolboxes.md) exist for. `guardExec` _refuses_ outward-shaped argv
  rather than pretending to clamp it.
- **Extensions.** They are not sandboxed, and the approval dialog says so rather than
  implying otherwise: an extension is ordinary JavaScript in the server process at the
  server's trust level. The gate is a digest over every byte plus an operator who
  approved it. "An approved extension did something bad" is the documented behaviour of
  approving code.
- **An agent configured with `allow` on a dangerous tool.** Permission is per tool, per
  agent, and the operator sets it. A model doing what it was permitted to do is not a
  vulnerability.
- **Prompt injection that the model acts on.** Every tool result is fenced with a
  per-turn nonce and the prompt says the contents are inert data; detection raises a
  badge and passes content through byte-for-byte. This raises the cost, and no prompt is
  a guarantee. An injection that **forges the fence** — closing an envelope without the
  nonce — is very much in scope.
- **Anything reachable only because the operator disabled authentication on a loopback
  bind.** A non-loopback bind with auth off already refuses to start.

[Security](docs/security.md) explains each guard, the attack it closes, why the obvious
approach does not work, and where it stops.

## What this project promises

Nothing leaves the machine. There is no telemetry, no analytics, no crash reporting and
no CDN — and that is tested rather than asserted:
[`self-contained.test.ts`](packages/web/test/self-contained.test.ts) fails the build if a
foreign origin appears anywhere in the UI, and
[`offline.spec.ts`](packages/e2e/test/offline.spec.ts) drives the whole app in a real
browser with every foreign origin blocked.

The one thing that talks to the internet is the model provider you configured, and
pointing it at Ollama removes even that.
