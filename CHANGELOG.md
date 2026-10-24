# Changelog

The sections are [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)'s: Added,
Changed, Fixed and Removed, without its release dates, which the tags carry. This project
uses [semantic versioning](https://semver.org/spec/v2.0.0.html). There is one version for
the whole repository, carried by the root `Cargo.toml` and the root `package.json`, which
a test holds equal; what you install is one binary, `darkwire`.

## [0.6.1]

`darkwire update` installs a newer release, and the web UI and the terminal show the
version that is running.

### Added

- **`darkwire update` replaces the binary with a newer release.** Like `install.sh`, it
  checks the download against the release's `SHA256SUMS` before it extracts anything.
  It then runs the new binary with `--version` and replaces the old file only if that
  prints the expected version. `--check` reports whether a newer release exists and
  installs nothing. A tag such as `v0.5.2` installs that release, even an older one.
  This command is the only part of DarkWire that contacts GitHub, and nothing checks for
  updates in the background.
- **The web UI and the terminal show the version.** The web UI shows it in the
  connection menu. The chat header shows it beside the wordmark, and the
  `darkwire serve` banner has a Version row.

### Changed

- **tokio is 1.53.3 and ctutils is 0.4.5**, which fixes RUSTSEC-2026-0386 and
  RUSTSEC-2026-0385.

## [0.6.0]

Each model call is now a message of its own, the context meter reads the provider's own
token count, and a first load of the web UI fetches 268 KB instead of 960 KB.

### Added

- **Every message has its own line.** Each model call in a turn is a message. Under each
  one is a line with how long it took, how many tokens it wrote and how fast, and Copy,
  Regenerate, Branch and Info. Regenerate works on any message, and Branch works while a
  turn is still running.
- **Typing while a turn runs steers it.** The loop reads the text before its next message.
  A steer it never reads runs as the next turn, so nothing typed is lost. Telegram and the
  other channels steer the same way.
- **The context meter shows the provider's own count.** After a reply the total is the
  number the provider reported, and only what came after it is estimated. The history
  trimmer budgets on the same figure, so the meter and the request agree.
- **`/resume` in the terminal** is another name for `/session`.
- **`GET /api/sessions/:key/context?summary=true`** returns the totals without the system
  prompt, the tool definitions or the messages. The context strip under the composer uses
  it.

### Changed

- **The server sends the web UI compressed and cached.** The build writes a Brotli and a
  gzip copy of each script and stylesheet, and the server sends the one the browser accepts.
  The browser keeps hashed files for a year and checks the page itself with an ETag. A
  first load over plain HTTP fetches 268 KB instead of 960 KB, and a second load fetches
  no scripts or styles at all.
- **Streaming renders in batches.** The browser applies the tokens that arrive within 16
  ms as one update, where it used to render once per token. Finished messages and tool
  lists no longer render again while the next message streams, and the browser skips
  layout for messages off screen.
- **The first download is smaller.** The setup wizard, the sessions page, the context
  inspector and the form controls load when first used. The terminal's strings and
  schemas the browser never reads are out of the bundle.
- **The browser keeps less in memory.** It drops a conversation's cached history and
  context 30 seconds after you leave it, where it used to keep them for 5 minutes. The
  code highlighting cache has a size limit, and at most five toasts show at once.
- **The server holds less per connection.** It reads at most 16 frames from a socket ahead
  of the hub, each at most 8 MiB, and a steer counts against the same queue of 8 as a
  message. Lines from MCP servers and extensions, and provider error bodies, have a size
  limit too.
- **Token counts carry no `~`.** The browser and the terminal show `5.12k` whether or not
  the provider has counted it yet. A screen reader still hears "about" for an estimate.
- **The binary no longer carries source maps**, which makes it 8 MB smaller.
  `darkwire serve --ui <dir>` still serves them.
- **Protocol changes.** `user.message` carries `workspaceId`. An `error` that refuses a
  message carries its `clientMessageId`. `connected` and `session.status` leave out
  `workspaceId` for a session that has no row yet, and the greeting includes
  `session.status` while a turn runs. See [API](docs/api.md).
- **rustls-webpki is 0.103.16 and webpki-root-certs is 1.0.10**, which fixes
  RUSTSEC-2026-0382 and RUSTSEC-2026-0383.

### Fixed

- **A reload in the middle of a turn shows the turn.** The page used to show the stored
  part as finished, with no Stop button, and drop everything after it. A tab that opens a
  conversation mid-turn now gets its status from the server and keeps growing the turn.
- **The browser re-sends a message a dead connection lost.** It pings a quiet connection,
  reconnects when the ping goes unanswered, and sends again any message the server has not
  acknowledged.
- **A refused message says so.** When the queue is full the bubble shows "Not sent" and
  the reason, where it used to say "Sending…" forever.
- **The browser refuses a message larger than 8 MiB** with a toast and keeps the text in
  the box. Such a message used to close the connection.
- **A new conversation starts in the workspace you picked.** The switcher no longer
  jumps back to `default`, and a fresh tab sends its choice with the first message.
- **A draft stays with its conversation.** Text typed in one session no longer appears in
  the next one you open.
- **A crash shows an error screen** with a Reload button, where it used to leave a blank
  page.
- **A tab left open across an upgrade gets a 404 for a missing file**, so the error names
  the file rather than reporting a syntax error.
- **The approval countdown stops** at the deadline and once the prompt is answered.
- **The task panel shows the newest plan.** When a turn wrote its list twice while the
  panel's first request was still on its way, the older list stayed on screen.
- **The scheduler starts a job once.** "Run now" racing the timer, or a restart during a
  run, could start it twice.
- **An approval answers only its own session's call.** Two sessions whose models numbered
  their calls the same way could deny or approve each other's prompts.
- **DarkWire retries a stream cut off in the middle of a tool call**, where it used to run
  the call with half its arguments. It retries a response with an empty body too.
- **Stop interrupts a slow error response** from a provider.
- **The agent loop and the scheduler read the database and files off the async threads**,
  so one slow read no longer stalls other sessions' streams.
- **A stored token calibration of zero no longer breaks the meter.**
- **About 50 messages and labels in the browser can be translated.** They were English in
  the code.

## [0.5.2]

The browser's slash-command list fits on screen however many models it offers.

### Fixed

- **A long `/model` list scrolls.** It ran off the top of the window. The list now stops
  at 20rem or half the window, whichever is less, and the arrow keys keep the highlighted
  row in view.

## [0.5.1]

Fixes for five places where memory or a connection could grow or loop without end, and
less work per token in the browser, the server and the agent loop.

### Fixed

- **A reload during a long turn keeps its connection.** A replay larger than 4 MiB closed
  the socket with code 1013, and the reconnect asked for the same replay again. Replay now
  has its own budget on each connection, with room for one whole replay.
- **Attachments stay inside the request's inline budget.** A file repeated through the
  history was inlined in full every time after the first, past the 16 MiB cap.
- **Expired logins are deleted.** They stayed in `darkwire.db` until someone presented the
  same token again. They now go at startup and on every login.
- **Notifications stop piling up.** The table keeps the newest 1,000. A job on an
  interval used to add a row per run with no limit.
- **An MCP server that never answers counts as failed.** It stayed "connecting" with no
  retry. The handshake now times out after a minute, and reaching the host after 15
  seconds.
- **Regenerate works on a long turn.** It read the last 200 messages to find the question,
  so a turn with more tool traffic than that had nothing to regenerate.

### Changed

- **Streaming does less work per token.** The browser re-renders the transcript alone,
  re-reads only the markdown block being written, and no longer downloads the whole
  conversation again when a turn ends. The server encodes each frame once for all open
  tabs.
- **The first page load is half the size.** Every screen but the chat and the sessions
  list loads when it is first opened, which takes the first bundle from 1.03 MB to 490 KB.
- **A long conversation costs less per agent step.** Each stored message is priced once a
  turn rather than twice a step, and the history is read off the async threads.
- **Writing a message updates the session row once instead of three times**, and a
  filtered session list uses its indexes.
- **`grep` stops walking the tree when it reaches its limit.**
- **A slow container start no longer holds up commands in containers already running.**
- **YAML is parsed by serde-saphyr.** `config.yaml`, environment definitions and extension
  manifests go through a pure-Rust parser with no `unsafe` code and limits on alias
  expansion. The serde_yaml_ng it replaces runs on an archived translation of libyaml.
- **rustls is 0.23.46**, which fixes RUSTSEC-2026-0346 to RUSTSEC-2026-0349.

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

[0.6.1]: https://github.com/therezor/darkwire/releases/tag/v0.6.1
[0.6.0]: https://github.com/therezor/darkwire/releases/tag/v0.6.0
[0.5.2]: https://github.com/therezor/darkwire/releases/tag/v0.5.2
[0.5.1]: https://github.com/therezor/darkwire/releases/tag/v0.5.1
[0.5.0]: https://github.com/therezor/darkwire/releases/tag/v0.5.0
