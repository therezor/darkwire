# Changelog

The sections are [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)'s: Added,
Changed, Fixed and Removed, without its release dates, which the tags carry. This project
uses [semantic versioning](https://semver.org/spec/v2.0.0.html). There is one version for
the whole repository, carried by the root `Cargo.toml` and the root `package.json`, which
a test holds equal; what you install is one binary, `darkwire`.

## [0.5.0]

The first release. DarkWire is an AI agent you run yourself: one Rust binary with the
browser UI compiled into it, pointed at a model on your own hardware.

### Added

- **One binary, four ways in.** A browser UI, a terminal REPL, a REST + WebSocket API and
  a Telegram channel, all over one port and one `darkwire.db`. A session started in one is
  a row the others list.
- **Local models first.** Ollama, LM Studio, llama.cpp and vLLM are reached through the
  `openai-chat` wire adapter with no key and no account. Cloud endpoints are configured
  the same way and are opt-in rather than assumed.
- **Real tools.** Read, write and edit files, list directories, run commands, fetch pages,
  plus memory and skills. Every path is rebuilt inside a workspace jail and then resolved,
  so `/etc/passwd` addresses `<workspace>/etc/passwd`.
- **Environments.** `exec` runs in a digest-pinned container with capabilities dropped and
  root read-only. The definition file under `policy/` is the policy, it sits outside the
  workspace jail, and it is authored in Settings rather than by an agent.
- **Agents and subagents.** An agent is a system prompt, a tool permission set, an
  optional environment and a roster of other agents it may delegate to, each reachable as
  an `ask_<id>` tool.
- **MCP servers** over stdio, Streamable HTTP or SSE, with OAuth. Each agent picks which
  of their tools it may call.
- **Extensions as separate processes**, speaking JSON-RPC 2.0 over their own stdio. That
  is MCP's stdio transport, so a plain MCP server is already a valid tools-only extension.
  Approval is a digest over every byte.
- **Scheduled jobs.** Cron and one-shot, with a heartbeat as an ordinary job rather than a
  second system.
- **Skills and memory as files.** A skill is a folder in the workspace and costs about 20
  tokens to index; memory is one plain markdown file per topic, read, saved and deleted by
  key, committed beside the project.
- **Approvals everywhere a turn can run.** A tool set to `ask` stops and shows the
  command it would run, in the browser, the terminal and a Telegram chat, a subagent's
  included. Each takes once, this session, a standing command rule for `exec`, or no. A
  run with nobody to ask refuses, and `darkwire chat --yes` runs them unasked.
- **Command rules for `exec`.** Argv patterns with an action, the most specific winning,
  and a separate cap for shells, so an agent can run `cargo test *` freely and never
  `git push`.
- **A task list per session.** The `todo` tool replaces the whole list on each call, the
  list is read back into the prompt on every iteration, and it is drawn in the terminal,
  above the composer in the browser and by `/tasks` in a Telegram chat. It is stamped with
  the point in the conversation it was written at, so re-running the turn that wrote it
  forgets it. A subagent runs on its own list.
- **An OpenAPI 3.1 document** generated from the same schemas the server validates
  against, and a protocol drift gate that compares the zod and Rust JSON Schemas rather
  than trusting that both halves were edited together.
- **No telemetry, and an offline test that proves it.** Every asset is local. Nothing
  reaches a CDN.

### Known limits

Stated here for the same reason they are stated in the README: a feature list that omits
them reads as more finished than it is.

- **`openai-chat` is the only wire adapter that ships.** The `anthropic` registry entry
  names `anthropic-messages` and is refused at construction rather than falling back;
  reaching it means an endpoint that speaks `openai-chat` or an extension contributing the
  wire.
- **There is no agent catalogue.** Agents are made in the web UI, one at a time. A preset
  system is planned and is deliberately absent rather than half-built.
- **English is the only shipped locale.** The translation layer is complete: typed
  bundles, negotiation, errors carrying keys across packages, two CI gates. Adding a
  language is a folder plus a line.
- **A heartbeat's `targets` do not reach a channel yet.** The decide/run/evaluate triad
  ships as a scheduled job's payload; delivery does not.
- **Session search is by title and filter, not by message content.**

[0.5.0]: https://github.com/therezor/darkwire/releases/tag/v0.5.0
