```
██████╗  █████╗ ██████╗ ██╗  ██╗██╗    ██╗██╗██████╗ ███████╗
██╔══██╗██╔══██╗██╔══██╗██║ ██╔╝██║    ██║██║██╔══██╗██╔════╝
██║  ██║███████║██████╔╝█████╔╝ ██║ █╗ ██║██║██████╔╝█████╗
██║  ██║██╔══██║██╔══██╗██╔═██╗ ██║███╗██║██║██╔══██╗██╔══╝
██████╔╝██║  ██║██║  ██║██║  ██╗╚███╔███╔╝██║██║  ██║███████╗
╚═════╝ ╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝ ╚══╝╚══╝ ╚═╝╚═╝  ╚═╝╚══════╝
```

<div align="center">

# DarkWire

### Your models. Your machine. Your wire.

An agent with a real shell and real file tools, running local models on your own
hardware — Ollama, LM Studio, llama.cpp, vLLM.<br>
Cloud providers are opt-in, not assumed.

[![licence](https://img.shields.io/github/license/therezor/darkwire?style=flat-square&color=00a600&labelColor=0d1117)](LICENSE)
[![install](https://img.shields.io/badge/install-one%20binary-00a600?style=flat-square&labelColor=0d1117)](docs/getting-started.md)
[![build](https://img.shields.io/github/actions/workflow/status/therezor/darkwire/ci.yml?style=flat-square&color=00a600&labelColor=0d1117)](.github/workflows/ci.yml)
[![telemetry](https://img.shields.io/badge/telemetry-zero-00a600?style=flat-square&labelColor=0d1117)](packages/web/test/self-contained.test.ts)
[![runs](https://img.shields.io/badge/runs-offline-00a600?style=flat-square&labelColor=0d1117)](packages/e2e/test/offline.spec.ts)
[![release](https://img.shields.io/github/v/release/therezor/darkwire?style=flat-square&color=00a600&labelColor=0d1117)](https://github.com/therezor/darkwire/releases/latest)

</div>

```bash
curl -fsSL https://raw.githubusercontent.com/therezor/darkwire/main/install.sh | sh
darkwire serve
```

<div align="center">

Prints a URL and a one-time code. Open it, pick a model — done.<br>
One binary, browser UI compiled in. No runtime, no package manager, nothing to
install alongside it.

Want more than one agent? Make them in **Agents**: a system prompt, the tools each may
call, the container each runs in, and which of the others it may hand work to.

**[Get started](docs/getting-started.md)** · [Docs](docs/) ·
[Security](docs/security.md)

</div>

<div align="center">
  <img alt="A terminal recording: darkwire chat is launched, asked what is in the workspace, calls the ls tool, and streams back an answer with a code block — finishing in 2 steps." src="docs/screenshots/demo.svg" width="100%">
</div>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/chat.light.png">
  <img alt="DarkWire's chat view: a streaming answer with a highlighted code block, the session sidebar, and the context budget under the composer." src="docs/screenshots/chat.dark.png">
</picture>

---

## What it does

|                  |                                                                           |
| ---------------- | ------------------------------------------------------------------------- |
| **Acts**         | Reads files, edits them, runs commands, browses the web from a sandbox.   |
| **Four ways in** | Browser, terminal, REST API, Telegram. Same conversations, same database. |
| **Remembers**    | Plain markdown in your project, which you read, edit and commit.          |

Built for long, tool-heavy work on infrastructure you control — recon and pen-test runs,
codebase surgery, research sweeps, scheduled chores.

## Why DarkWire

- **You hold every key.** Permission is per tool, per agent — `allow | ask | deny` — and a
  tool absent from the map is not refused at call time, it is **never offered to the model
  at all**. Nothing is granted implicitly, ever.
- **A small prompt is a fast prompt.** On a local model the prompt is not a bill, it is
  wall-clock: whatever changed since the last step gets re-processed before the first
  token comes back, and one tool-using turn is five or ten requests over the same history.
  So the volatile half of the prompt is kept tiny, tool registry entries are rebuilt once per
  _turn_ rather than once per step, and `exec` reaches every program in a container from one
  **~40 token** schema where a tool per program costs **60–80 each, per request**.
- **Nothing leaves the box.** A repo-wide grep for
  `telemetry|analytics|posthog|sentry|mixpanel` returns **exactly one hit** — the test that
  forbids them. Binds `127.0.0.1`; a public bind with auth off **refuses to start**.
- **No hidden preamble.** All eight prompt templates are yours to edit, and `systemPrompt`
  _replaces_ the built-in text rather than being bolted underneath something you cannot see.
- **Security you can audit in an afternoon.** Every guard lives in one package behind a 95%
  coverage gate, each explaining the attack it closes and why the obvious approach fails.

## The competition

- **More private than ChatGPT** — nothing leaves the box. Zero telemetry, and fully
  offline against Ollama.
- **More contained than Claude Code** — `exec` runs in a digest-pinned container, caps
  dropped, egress default-deny unless the agent asks for more.
- **Leaner on tokens than OpenClaw** — one `exec` schema reaches every program in the
  container, in ~40 tokens, where a tool per program costs 60–80 **each, per request**.

---

## See it

<table>
<tr><td width="50%">

<b>A tool call, expanded</b>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/chat-tool-call.light.png">
  <img alt="A ls tool card, expanded to show the files it returned." src="docs/screenshots/chat-tool-call.dark.png">
</picture>

</td><td width="50%">

<b>An approval, before anything runs</b>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/chat-approval.light.png">
  <img alt="An approval prompt for exec, showing the argv it would run, with Once, This session and Deny." src="docs/screenshots/chat-approval.dark.png">
</picture>

</td></tr>
<tr><td width="50%">

<b>Where the context window went</b>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/context.light.png">
  <img alt="The context inspector, breaking a session's token usage into system prompt, tool registry entries, session and live state." src="docs/screenshots/context.dark.png">
</picture>

</td><td width="50%">

<b>The workspace, browsable and editable</b>

<picture>
  <source media="(prefers-color-scheme: light)" srcset="docs/screenshots/files.light.png">
  <img alt="The file browser, listing the workspace tree." src="docs/screenshots/files.dark.png">
</picture>

</td></tr>
</table>

More in [Web UI](docs/web-ui.md). Generated from the real app, not staged.

## Loadout

|                                                        |                                                                                                                                                                       |
| ------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [**Skills**](docs/skills.md)                           | A folder in the workspace. The agent opens the sheet when it applies; ~20 tokens to index.                                                                            |
| [**Memory**](docs/memory.md)                           | One markdown file per topic, read and written by key, committed beside the project.                                                                                   |
| [**Environments**](docs/environments.md)               | Bound the blast radius. `exec` runs in a digest-pinned container with caps dropped and root read-only, and no image, capability or uid an agent's config could reach. |
| [**MCP servers**](docs/tools.md#mcp-servers)           | stdio, Streamable HTTP or SSE, with OAuth. Each agent picks which of their tools it may call.                                                                         |
| [**Extensions**](docs/extensions.md)                   | Tools, channels, providers, prompt sections, commands. Approval is a digest over every byte.                                                                          |
| [**Subagents**](docs/architecture.md#subagents)        | One agent hands work to another as an `ask_<id>` tool.                                                                                                                |
| [**Telegram**](docs/configuration.md#channelstelegram) | The same sessions from a phone. Answers only the ids you list.                                                                                                        |
| [**Scheduled jobs**](docs/configuration.md#scheduler)  | Cron and one-shot. A heartbeat is a job, not a second system.                                                                                                         |
| [**REST + WebSocket**](docs/api.md)                    | One port, with an OpenAPI 3.1 doc generated from the schemas the server validates against.                                                                            |

## Security

> **Assume the model is compromised.**

It reads web pages, command output and files an attacker may have written. Everything it
asks for is an untrusted request.

| Guard                   | What it stops                                                                                                  |
| ----------------------- | -------------------------------------------------------------------------------------------------------------- |
| **Workspace jail**      | Path traversal. `/etc/passwd` addresses `<workspace>/etc/passwd`; paths are rebuilt, then `realpath`'d.        |
| **Argv-only exec**      | Command injection. The argv vector is spawned directly — no shell, so no string to interpret.                  |
| **Environments**        | Blast radius. `exec` runs in a digest-pinned container whose definition fixes caps, user and network.          |
| **Per-tool permission** | An agent doing what you did not enable. Absent means not enabled; `ask` shows you the arguments first.         |
| **`guardedFetch`**      | SSRF and DNS rebinding. Resolved addresses are pinned into the dispatcher — no second lookup to differ.        |
| **Nonce fencing**       | Prompt injection. Every tool result is fenced with a fresh per-turn nonce, and the model is told it is data.   |
| **Credential vault**    | Key theft at rest. AES-256-GCM, `0600`, key in the OS keychain. Nothing reads a credential back out over HTTP. |
| **Auth**                | Guessing. argon2id, and two asymmetric throttle scopes so a botnet cannot spread its attempts out.             |

[Security](docs/security.md) states each guard's limits — including the one that matters: a
workspace is an organisational boundary, not a security boundary, wherever host `exec` is
enabled. That is what containers are for. [SECURITY.md](SECURITY.md) is how to report a
vulnerability.

## What is not built yet

The schemas and seams ship; the implementations do not, and nothing in the UI advertises
them — a settings screen naming a feature you cannot open is one an operator checks
twice.

| Feature                | Ships today                                                 | Missing                          |
| ---------------------- | ----------------------------------------------------------- | -------------------------------- |
| **Heartbeat delivery** | The decide/run/evaluate triad, as a scheduled job's payload | `targets` reaching a channel     |
| **Session search**     | Keyset pagination and filters                               | Text search over message content |

Two smaller ones, so nothing here reads as more finished than it is: only the
`openai-chat` wire adapter ships, so the one registry entry naming another wire —
`anthropic` — is refused at construction rather than falling back, and reaching it today
means an endpoint that speaks `openai-chat` or an extension that contributes the wire; and
the translation layer is complete while **English is the only shipped locale** — adding
one is a folder plus a line.

---

## Docs

**[Getting started](docs/getting-started.md)** · [CLI](docs/cli.md) ·
[Configuration](docs/configuration.md) · [Prompts](docs/prompts.md) ·
[Providers](docs/providers.md) · [Tools & permissions](docs/tools.md) ·
[Skills](docs/skills.md) · [Memory](docs/memory.md) · [Environments](docs/environments.md) ·
[Extensions](docs/extensions.md) · [Web UI](docs/web-ui.md) · [API](docs/api.md) ·
[Architecture](docs/architecture.md) · [Security](docs/security.md) ·
[Development](docs/development.md)

## Contributing

`pnpm check` runs the first CI job. [CONTRIBUTING.md](CONTRIBUTING.md) has the other
three, [Development](docs/development.md) the reasoning behind them.

<details>
<summary>Running from source</summary>

```bash
git clone https://github.com/therezor/darkwire.git
cd DarkWire
pnpm install
pnpm build                                  # the web bundle the binary embeds
cargo build --release -p darkwire            # → target/release/darkwire
```

Needs pnpm 11 (`corepack enable`) and `rustup`; the compiler version is pinned in
`rust-toolchain.toml`. `pnpm build` is not optional — `rust-embed` compiles
`packages/web/dist` into the binary, so a missing bundle is a compile error rather
than a server that comes up without a UI.

</details>

## License

[MIT](LICENSE)
