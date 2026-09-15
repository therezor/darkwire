# GhostAI documentation

Everything here describes what is built. Work that is designed but not implemented is not
documented as though it works.

**New here?** [Getting started](getting-started.md) runs from a clone to a first answer,
and everything else on this page is reference you can reach for afterwards.

## Using it

| Page                                  | What it covers                                                                        |
| ------------------------------------- | ------------------------------------------------------------------------------------- |
| [Getting started](getting-started.md) | Install, first run, first conversation, giving it files, letting it run commands.     |
| [CLI](cli.md)                         | Every `ghostai` command and flag, and the slash commands inside the chat prompt.      |
| [Configuration](configuration.md)     | Every key in `config.json`, its type and its default. Env vars. Patch semantics.      |
| [Prompts](prompts.md)                 | The eight editable templates, their placeholders, and the caching split behind them.  |
| [Providers](providers.md)             | The registry, provider instances, resolution order, credentials, resilience.          |
| [Tools & permissions](tools.md)       | The eight built-in tools, and the `allow \| ask \| deny` model that gates them.       |
| [Skills](skills.md)                   | Instruction sheets in `<workspace>/skills/`, indexed or named on a message.           |
| [Memory](memory.md)                   | What an agent remembers between sessions, one file per fact in `<workspace>/memory/`. |
| [Toolboxes](toolboxes.md)             | Approved tool surfaces and independently selected execution containers.               |
| [Sandbox service](sandbox-service.md) | Isolated container service, Compose deployment, lifecycle management.                 |
| [Extensions](extensions.md)           | Third-party code an operator installs and approves, and the five things it may add.   |
| [Web UI](web-ui.md)                   | The screens, and what each one lets you do.                                           |

## Understanding it

| Page                            | What it covers                                                                    |
| ------------------------------- | --------------------------------------------------------------------------------- |
| [Architecture](architecture.md) | The crate graph, a turn end to end, the event stream, subagents, what is on disk. |
| [Security](security.md)         | Each guard, the attack it closes, why the obvious approach fails, and its limits. |
| [API](api.md)                   | The REST surface and the WebSocket protocol.                                      |

## Working on it

| Page                          | What it covers                                                       |
| ----------------------------- | -------------------------------------------------------------------- |
| [Development](development.md) | The CI gate, conventions, coverage bars, the UI loop, the e2e suite. |

## Where the truth lives

The code carries its reasoning in comments, and these pages are written from it rather
than from each other. When a page and the source disagree, the source is right — and the
page is a bug. The highest-value files to read directly:

- `packages/protocol/src/config.ts` — the settings tree, with a paragraph per decision
- `packages/protocol/src/prompt.ts` — the prompt templates and substitution rules
- `crates/agent/src/agent_loop.rs` — the turn, and the invariants it maintains
- `crates/agent/src/dispatch.rs` — the tool half of a turn: authorise, run, answer
- `crates/security/src/` — the guards, each explaining its own threat model
