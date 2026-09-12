# Changelog

The sections are [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)'s — Added,
Changed, Fixed, Removed — without its release dates, which the tags carry. This project
uses [semantic versioning](https://semver.org/spec/v2.0.0.html). There is one version for
the whole repository, carried by the root `Cargo.toml` and the root `package.json`, which
a test holds equal; what you install is one binary, `ghostai`.

## [0.9.0]

**GhostAI is a Rust program now, shipped as one binary.** Twelve TypeScript
packages — the core, the security guards, the providers, the tools, the MCP
client, the agent loop, the channels, the extension host, the runtime, the
server, the terminal UI and the command line — were rewritten as crates under
`crates/` and then deleted: 484 files and about 127,000 lines. What a person
installs is a single executable with the browser UI compiled into it, rather than
a package manager's dependency tree.

Four packages stayed TypeScript, and the reason is the same one in each case: a
browser has to read them. `protocol` is the zod schemas the web app validates
against, so it remains the source of truth and Rust mirrors it; `i18n` is the
translation layer the UI loads; `web` is the UI; `e2e` is the Playwright suite,
which now drives the real binary as a subprocess instead of a server it had built
in-process. The port is covered by 3,956 Rust tests, 1,887 vitest tests and 314
Playwright specs across two colour schemes.

**If you have an extension written against `ghostai.extension/1`, it will not
load.** That is the one hard break in this release; see below.

### Changed

- **Extensions are separate processes speaking JSON-RPC, and the wire is MCP's.**
  A v1 extension was an ES module the host `import`ed into its own process: it
  exported `activate`, received a context object with five `register*` methods,
  and shared a heap — and an address space — with the agent, the vault and the
  server. A v2 extension is an argv the host spawns, talking JSON-RPC 2.0 over its
  own stdio, one object per line. That is exactly MCP's stdio transport, which has
  a consequence worth stating plainly: **a plain MCP server, one that has never
  heard of GhostAI, is already a valid tools-only extension.** The `ghostai/`
  methods that carry channels, providers, prompt sections and commands are
  additions on top of a handshake it already speaks, and a server that answers
  `-32601` to all of them still works.

  The manifest changes with it — `"schema": "ghostai.extension/2"`, and `entry`
  (a module path) becomes `command` (an argv). **A v1 bundle lands on its row as
  `failed`, with a sentence saying why**, rather than being adapted: the
  difference between the two is a process boundary, and there is no version of
  "load this module" a host that spawns can honour. Both versions still parse, so
  the refusal has an id, a label and a `contributes` list to render — a union
  would have made it a parse error with nothing to hang a row on. The only
  migration is to rewrite the entry point as a program, and
  `examples/hello-extension` is that rewrite: about two hundred lines of
  dependency-free Node, and the reference for what the contract now is. The
  trust class is unchanged — the
  child still has full access to the operating system, and approval is still a
  digest over every byte — but a crash is now a process dying rather than an
  exception in the agent's stack, and `failed` on an extension's row means its
  process died.

- **Installing is a download.** Each release attaches four tarballs —
  `aarch64`/`x86_64` × macOS/Linux — and a `SHA256SUMS` file. Each binary is built
  on its own architecture rather than cross-compiled, because the credential vault
  talks to the platform keychain and the container runner signals process groups;
  both are platform code, and a cross-linker is one more thing that can be subtly
  wrong in a way only a user discovers. Windows is still absent for the same two
  reasons.

- **SQLite is compiled in rather than borrowed from the host.** `node:sqlite` set
  the old Node 22.13 floor; `rusqlite` with the bundled library removes the
  question entirely. The database format did not move — an existing `ghost.db`,
  `config.json`, `vault.json` and `vault.key` are read as they are, which is
  asserted against files the deleted TypeScript wrote (see `fixtures/`).

- **Node is a build dependency, not a runtime one.** It builds the web bundle and
  runs the four remaining packages' tests. The only way it reaches a user's
  machine now is an extension that happens to be written in JavaScript, and that
  brings its own interpreter as a child process.

### Added

- **`install.sh`, and a verification step nobody has to remember.** The install
  was four lines to copy, one of which — `shasum -c` against the release's
  `SHA256SUMS` — is the one that distinguishes "the release" from "whatever
  arrived", and is therefore the one most likely to be skipped. The script picks
  the build for the machine it is running on, refuses to extract anything until
  the hash matches, and reaches for `sudo` only when the install directory is not
  the caller's, after saying so. `--dir` and `--version` are there for a
  different directory and an older release. The manual four lines are still
  documented, under a fold, because piping a script from the internet into a
  shell is a thing to be able to decline.

- **A Rust workspace and a fourth CI job.** `rust-toolchain.toml` pins the
  compiler, the root `Cargo.toml` pins every dependency exact and carries the lint
  table (`clippy::all` denied, `pedantic` an error in CI, `#![forbid(unsafe_code)]`
  in every crate), and `deny.toml` is the supply-chain policy — advisories,
  licences, no git sources, no wildcards. The job runs format, clippy, deny, the
  tests and per-crate coverage bars through `scripts/coverage-gate.mjs`, which
  exists because `cargo llvm-cov` has one threshold for a whole workspace and
  `security` has to be held to 95/95 while a cosmetic crate is held to 70/65.

- **`fixtures/` — a parity oracle, in a form both languages read.** Fifteen
  families of cases were generated from the TypeScript implementation before it
  was deleted, and the Rust tests assert against the same bytes. Twelve of them
  are frozen now, which is the correct end state: the specification they record no
  longer exists as code, so nothing can quietly re-derive them to match a port
  that disagrees.

- **A drift gate over the wire contract.** `packages/protocol` emits one JSON
  Schema document per registered schema and `crates/protocol` emits its own; CI
  diffs them. A schema edited on one side only fails the build rather than
  reaching a browser.

### Fixed

- **A history fetch could delete an answer that was already on screen.** This one
  was live in the shipped product, not something the port introduced. The socket
  mints a session key on the first message, the URL gains it, and the history
  request that fires on that change is in flight while the turn is still
  streaming. Against a server on the same machine it lands in a millisecond and
  holds everything; against one a network away — or a busy one — it lands holding
  the rows that existed when it was _asked_, which is a turn missing its last
  answer. That was taken as the base of the merge, so text the tab had already
  displayed was deleted, and no later fetch put it back: the invalidation that
  would have refetched fires while the request is still open and is deduplicated
  into it. The turn stayed truncated until something forced a reload. A stored
  rebuild that knows _less_ about a turn than the socket does no longer replaces
  it.

The other three never reached a user: they were found in the port, in the port's
own code, before the first binary shipped. They are recorded because the
counterfactual is the interesting part — had that binary gone out carrying them,
each would have failed on first use, on every install:

- **`PATH` reached no tool's child process.** The crates take an owned
  environment map rather than reading the process, so that a test does not have to
  mutate global state — and the list of names copied into that map did not include
  `PATH`. `exec`'s own allow-list then filtered an environment that had none, so
  every command named without a leading slash failed with "No such file or
  directory". `node --version` is the shape of it: a command an operator would
  call ordinary.

- **`POST /api/automation/jobs/:id/run` answered "this build has no scheduler".**
  A knot that had genuinely come untied: the scheduler is built over the job
  store, the job store is created while the server is built, and the server's
  routes need the scheduler — so the server was handed `None` and nothing ever
  handed it anything else. Both `Run now` and every route that asks the engine to
  re-read what is due were affected.

- **An install with a provider but no model reported no provider at all**, so the
  first-run wizard asked for a provider when it should have asked for a model.
  The status route read the endpoint off the agent's _loop_, and an agent with no
  model has no loop — which made "configured an endpoint, did not pick a model"
  indistinguishable from "configured nothing". It reads the resolved instance when
  there is no loop now; `configured` remains the flag to branch on for "can this
  take a turn".

### Removed

- **npm publishing.** Nothing in this repository is published to a registry any
  more. The trusted-publishing job, the `pnpm pack` loop, the per-package
  `publishConfig` and the `files` negations that existed to keep source maps out
  of a tarball are all gone with it, and the remaining manifests are `private`.

- **The twelve TypeScript packages the crates replace**, their tsup configs, their
  root `tsconfig.json` references, their coverage thresholds and
  `examples/loopback-channel` — which is now a conformance test in
  `crates/channels/tests`.

- **`@ghostwire/i18n/cli`.** The terminal's i18next instance had no consumer left:
  the Rust CLI embeds `locales/en/cli.json` with `include_str!` and generates a
  typed constant per key in `build.rs`, so the JSON is still the type and a key
  that is not in the bundle is a compile error. The bundle itself and the
  `./locales/*` export are unchanged, because that is what both sides read.

- **The hand-edited `VERSION` and `SERVER_VERSION` literals.** They existed
  because a bundle in `dist/` resolves a relative manifest read differently in a
  workspace and in a published tarball, and a silently wrong version is worse than
  a missing one. A compiled binary has no such ambiguity:
  `env!("CARGO_PKG_VERSION")` is fixed at build time and is what both
  `ghostai --version` and `GET /api/status` report.

## [0.8.1]

The 0.8.0 config change, and what it cost a browser tab that was already open.
`/model` said it had saved a model it had not, in the one way a person can
neither see nor act on — and while chasing it, three smaller disagreements about
which agent a conversation runs on.

### Changed

- **Adjacent read-only tool calls run together.** A model that asks for six
  files in one message had them fetched one after another, for no reason but the
  shape of the loop. Grouping is _adjacent_ runs only, which is the safety
  property and not a simplification: `read, read, write, read` becomes
  `[read‖read]`, `write`, `read`, so a write is never reordered past a read. A
  call that would prompt for approval and a delegation to a subagent are both
  excluded, so nothing about who is asked what has changed — and results are
  reported as they land rather than gathered at the end, so a fast read no
  longer waits on the slowest member of its group. Eight at once, which is a
  bound on open file handles rather than a knob to tune.

### Fixed

- **A settings patch naming a section this build does not have is refused
  instead of silently discarded.** `ConfigPatchSchema` was a stripping object,
  so a client sending a shape the server no longer knows got a 200 and a save
  that changed nothing. That is not hypothetical: removing `agents.defaults` in
  0.8.0 left every already-loaded browser tab still sending
  `{agents: {defaults: …}}` for `/model`, and the answer was "the agent now runs
  it. Saved." over a config the request never touched. It answers 422 naming the
  key now — a refusal rather than a warning field, because the clients this
  catches are old clients, and an old client does not read a new field but does
  surface a 4xx. **If `/model` has been saying "Saved" without switching
  anything, reload the page**: the fix is on both sides, and the client half only
  reaches a tab that has fetched it.
- **The agent switcher follows the conversation you open.** It was documented to
  and never did: the only thing that moved it was a move you had just made by
  hand, so arriving at a conversation bound to another agent any other way left
  the control naming a different one — and a new conversation started from there
  inherited the wrong agent. A binding whose agent has been deleted is still not
  adopted, which is the case the control marks rather than follows.
- **The welcome card names the model that will answer**, not the one this
  browser last picked. An empty transcript is not an unbound conversation:
  `/clear` leaves the binding in place and so does a branch nobody has spoken
  in, and on either of those the one line whose whole job is to say what is about
  to answer named another agent's model.
- **A session whose stored `agentId` is empty is treated as unbound**, which is
  what the server has always done with it. The picker offered to _move_ a
  conversation that had never been bound, and `/model` edited an agent no turn on
  that session would have used.

## [0.8.0]

`agents.defaults` is gone, and with it the inheritance layer above an agent. Every agent
now states its own settings; a field an entry does not name is filled by the schema, not
by another agent's answer. This is a breaking change to the config format.

### Removed

- **`agents.defaults`.** It was one settings block above every agent, and an agent that
  named no `model` took whichever one it held. That made "what does this agent run on"
  unanswerable from the agent — the agent editor had already stopped expressing
  inheritance and worked around the format by writing values down on save, and the
  composer's `/model` reintroduced it every time somebody cleared a field. `AgentEntry`
  is a complete schema now, so `{"label": "Coder", "model": "qwen3:8b"}` is still a whole
  agent: the brevity came from the defaults, not from the indirection.

  **An existing `config.json` is not migrated.** The section is dropped on load, and any
  agent that was relying on it for a `model` comes up unconfigured: listed, editable, and
  refused a turn with a message saying so. Set a model per agent, in Settings → Agents or
  by hand. Everything else — budgets, timeouts, the two capability switches — comes from
  the schema at the same values `agents.defaults` used to hold, so nothing else changes.

### Changed

- **`workspace` moved to the root of the config.** It was the one field in
  `agents.defaults` that is not a turn setting: an agent _works in_ a workspace and does
  not own one, which is why `AgentEntry` could never name it. `{"workspace": "…"}` now
  sits beside `providers` and `server`.
- **`/model`, `/effort` and `/temperature` edit the agent the session runs on**, in the
  browser's composer and at the terminal's prompt, and both save. `/model` on a session
  bound to an agent with its own model moves that agent, which is the one thing it could
  not do before.
- **`reasoningEffort` and `temperature` are per agent, and absent means the provider
  decides.** There is nothing above an agent for a cleared field to fall back to, so the
  word for that state is `default` — `/effort default`, `/temperature default`.
- **The agent editor is one form.** The model and budget boxes used to write a different
  subtree for the default agent than for every other; there is one subtree now, so
  `maxToolIterations` and the turn timeout are editable on every agent rather than only
  on `default`.
- **`ghostai agent install` writes a model into the preset it installs**, copied from the
  default agent. A preset ships none on purpose — one naming a model would break on every
  machine that lacks it — and it used to inherit one.

### Fixed

- **The e2e harness ran every turn on the default agent's loop**, whatever agent the
  session was bound to, so the suite could not see a per-agent model, prompt or toolset
  at all.

## [0.7.3]

The terminal UI, on the terminals it had been quietly failing on. Everything
secondary was drawn with an attribute a good many emulators do not implement,
colour was forced on whatever the environment said, and the frame only took the
window once you resized it.

### Fixed

- **Secondary text is bright black rather than faint.** `dim` was SGR 2, which
  is _optional_ in ECMA-48 — the Linux kernel console and PuTTY do colour
  perfectly well without implementing it, so every hint, header label and status
  row drew at the weight of ordinary prose there. SGR 90 is a colour, and a
  terminal that ignores it renders plain text, which is what those terminals were
  already doing. Bound once, in `paletteFor`, rather than at forty call sites.
- **`NO_COLOR`, `FORCE_COLOR`, `TERM=dumb` and a redirected stdout are honoured
  again.** `--no-color` is a negated flag, and commander gives one of those the
  value `true` at registration — so every ordinary run passed an explicit "yes,
  colour" and picocolors' own detection was never consulted. `ghostai chat > log`
  had been writing escape codes into the log. Colour in a pipe now needs
  `FORCE_COLOR`; `CI` still keeps it on, so CI output is unchanged.
- **`ghostai chat` takes the window on the way in**, instead of on the first
  resize. The first frame was printed wherever the shell left the cursor, and a
  width change was the first thing that cleared and homed it. It clears the
  screen, not the scrollback behind it — a resize still drops the scrollback,
  because a rewrap can strand fragments of a frame we drew up there, and on the
  way in there is nothing of ours to strand.

## [0.7.2]

Two screens stopped lying about a turn that is still running: a reload during a
delegation lost the run, and the context bar billed text the model never saw.

### Added

- **A reload mid-turn comes back to the whole turn**, nested subagent steps
  included. The server keeps the running turn beside the replay ring, and a
  resume past the ring gets both the stored tail and that turn's frames.
- **`server.turnLogMaxBytes`** (default 16 MiB) bounds that retention. Only a
  session with an open turn holds one; `0` turns it off.
- **The context bar moves while a turn runs**, on a new `context.usage` frame
  emitted at the end of every tool iteration.
- **Time to first token** in the turn-info popover, beside elapsed.

### Changed

- **Tokens/s divides by generation time, not the turn's wall clock**, so a cold
  model load or a slow tool no longer reports the model as slow. Older turns
  keep the wall-clock figure.
- **The context inspector prices the request, not the stored records.** The
  model's reasoning and a tool's `risk`/`source` are no longer counted or shown;
  both halves of the prompt are priced inside the messages they are sent in.
  Expect the figure to move on the same conversation.
- The retry ladder sizes history the same way, so a context-length retry cuts
  what it means to.
- `GET /api/sessions/:key/context` no longer returns `reasoning`. The transcript
  endpoints still do.

### Fixed

- **`node:sqlite`'s experimental warning no longer prints** on every command
  that touches a session. Only that one message is dropped.
- A subagent whose delegating call was never seen — the usual outcome of
  reloading mid-delegation — renders as a card instead of being dropped.
- A _reconnect_ past the replay ring no longer deletes the answer on screen.

## [0.7.1]

Skill sheets learn who they are for. A sheet was in every agent's catalogue or in
none, which is the wrong granularity for a workspace a coder, a researcher and a
lead all work in: every sheet cost every agent prompt on every turn, and the only
way to narrow it was not to write the sheet.

### Added

- **`agents:` in a sheet's frontmatter**, narrowing it to the agents it names —
  `agents: coder, team-lead`. A sheet without one is in every agent's catalogue, so
  nothing written before this changes. Scope decides which sheets an agent is _told
  about_, not which it may open: `read_file` and the `skill` tool still reach a sheet
  that was never advertised, and that is not a hole — a skill is prose, and the jail
  and the exec guard have never read a word of the prompt.
- **It fails open.** A line that yields no usable id leaves the sheet visible to
  every agent and logs a warning. The two ways of being wrong are not symmetric: a
  sheet shown too widely costs prompt that `/skills` will show you, and a sheet
  hidden from everybody is one that silently stopped working with nothing anywhere
  to find.
- **`skills` on an agent preset** — sheet directories under the catalogue's
  `skills/`, copied into the workspace when the agent installs. A copy, byte for
  byte, rather than an install: no hash, no approval gate, and nothing recorded
  afterwards. A toolbox manifest earns one because it names a container's boundary;
  a sheet is prose, and the preset's own `systemPrompt` — same catalogue, same
  network — already set that bar.
- **`-W, --workspace-id <id>` on `agent install` and `preset install`**, saying which
  workspace those sheets land in. It defaults to `default`, and a named workspace has
  to exist already. Spelled `--workspace-id` because `--workspace` already means a
  _directory_ on `chat` and `serve`, and one flag meaning two things is how somebody
  ends up passing a path to it.
- **`/skills` marks sheets scoped to other agents** rather than dropping them.
  Somebody runs it precisely when a sheet is not working, and a listing that hid it
  would leave nowhere to find out why.

### Changed

- **Nothing about a sheet refuses.** One that is missing, symlinked or over the
  copier's bounds costs that sheet and a line in the report, and the agent installs
  regardless. A missing _toolbox_ still refuses, because an agent without one cannot
  run at all; an agent with one fewer index line can.
- **A sheet already in the workspace is left alone** unless `--force` is passed,
  which is the rule the `agents.list` entry already followed and for the same reason:
  it may carry your edits. `--force` overwrites file by file rather than emptying the
  directory, so anything you added inside a sheet folder survives.
- **`MAX_SKILLS` is applied before scope**, deliberately: the cap bounds the per-turn
  read, and applying it after would mean opening a thousand directories to find the
  twelve one agent sees. Past a hundred, which sheets an agent sees follows
  alphabetical order rather than who they are for.
- `ghostai agent install` copies a preset's sheets too when a catalogue is already on
  the machine. It never fetches one, so on a box that has never run
  `ghostai preset update` every sheet a preset names is reported missing, and the
  agent installs anyway.

### Removed

- **`~/.ghostai/agents/<id>/`, and `agentDirFor` from `@ghostwire/core`.** The
  directory was reserved so per-agent state could sit outside the jail, where prompt
  injection could not rewrite what an agent believes. Nothing ever wrote to it:
  memory declined it, skills declined it, and per-agent scope declined it too — each
  time because a sheet is meant to be read, reviewed and committed beside the project
  it describes, and it is none of those if it lives somewhere the agent cannot list.
  After a third pass the reservation was removed rather than kept for a fourth. An
  agent id is now a key in `agents.list`, a session column and a tool-name segment,
  and never a directory; it keeps a workspace id's character rules regardless, since
  two rule sets that agree today are two that drift apart in the case nobody tested.

### The catalogue moves on its own

Sheets a preset brings need a catalogue that carries them, and
`@ghostwire/presets@1.0.0` has no `skills/` and no preset naming one. Nothing waits
on the other: a preset that names no sheet copies none, and the half of this that
reads sheets already in a workspace works today with no catalogue at all.

The CLI asks npm for `@ghostwire/presets@^1.0.0` — any 1.x, with `--no-save` and
`--no-package-lock`, so nothing pins a resolved version. A catalogue that adds
`skills/` therefore reaches an existing install on the next `ghostai preset update`,
with no upgrade of GhostAI itself. The two release on their own cadences by design;
this entry describes what the app can do, not what today's catalogue asks it to.

## [0.7.0]

The first release. Everything below is what it ships with rather than what changed.

**0.7.0 rather than 1.0.0, deliberately.** An earlier build went out as 1.0.0 on the
argument that the surface was what mattered — the config schema, the REST and WebSocket
protocol, the tool contract, the extension contract and the eight prompt templates are
what other people build against, and breaking one should cost a major version. That
argument still holds and those interfaces have not moved. What was wrong was the
confidence: within two days the agent presets and toolboxes had been extracted into a
repository of their own, the command that installs them was rewritten around a question
the old one never asked, and the packages moved scope. A surface that reshapes itself
that often is a 0.x whatever its interfaces promise.

**The scope is `@ghostwire` and the command is `ghostai`.** Both were `@ghostbot` and
`ghost` in the withdrawn builds. `ghost` is a common enough binary name to collide on a
shared machine, and `@ghostbot/cli` named the layer rather than the product — what you
install is the thing you type.

The known limits at the bottom of this entry are missing features, not unfinished
interfaces.

### The agent

- A tool loop as an async generator, with one `AbortSignal` threading from the HTTP
  request through the provider fetch, the tool and any child process.
- Mid-turn steering: what you type while a turn runs reaches that turn.
- Subagents — an agent delegates to another as an `ask_<id>` tool, each run a real turn on
  a real loop in its own linked session.
- Eight built-in tools: `read_file`, `write_file`, `edit_file`, `list_dir`, `exec`,
  `memory`, `skill`, `automation`.
- Per-tool, per-agent permission — `allow | ask | deny`, and a tool absent from the map is
  not enabled at all.
- Memory and skills as plain markdown in the workspace, one file per fact and one folder
  per sheet.

### Four ways in

- A web UI: streaming answers, tool cards, approval prompts, a file browser and editor,
  multiple workspaces, an agent editor, and a context inspector.
- A CLI: `ghostai chat` as a one-shot, a pipe target or a prompt with slash commands.
- REST and WebSocket on the same port, with an OpenAPI 3.1 document generated from the
  same Zod schemas the server validates against.
- A Telegram bot over long polling, answering only the ids you list.

### Models

- Local first: Ollama, LM Studio, llama.cpp and vLLM, with loopback defaults and live
  model listing.
- Cloud, opt-in: OpenAI, Gemini, OpenRouter, DeepSeek, Groq, xAI, and `custom` for any
  OpenAI-compatible endpoint.
- A prompt split around the provider's cache prefix, with tool definitions and the
  per-turn nonce computed once per turn.
- All eight prompt templates editable, replacing the built-in text rather than being
  appended to a hidden preamble.

### Security

- A workspace jail that rebuilds paths rather than inspecting them, then `realpath`s and
  checks containment.
- Argv-only `exec` — `execFile` with `shell: false`, and a lint rule that fails the build
  on `shell: true`.
- Toolboxes: `exec` inside a digest-pinned container, authorised by manifest hash, caps
  dropped, root read-only, network mode capped by the manifest.
- `guardedFetch`, which pins resolved addresses into the dispatcher so there is no second
  DNS lookup to differ from the first.
- Per-turn nonce fencing on every tool result, with non-destructive injection detection.
- An AES-256-GCM credential vault with its key in the OS keychain.
- argon2id passwords with two asymmetric throttle scopes, and a refusal to start on a
  non-loopback bind with authentication off.

### Agents you install

- **Agent presets.** A JSON file — prompts, tool permissions, a toolbox reference, a
  delegation roster — merged into `agents.list`. The shape is a strict subset of an agent
  entry, so a preset can express nothing a settings save could not: no model, no
  provider, and nothing from the toolbox manifest's side of the security boundary. One
  kind of preset and one lookup, whether or not the agent works in a container.
- **`ghostai preset install`** lists what the catalogue offers and installs the ones you
  tick, building only the containers those particular agents named. Choosing agents that
  need no container is how an install with no Docker finishes. Approval is settled in the
  same run, because approving is what unblocks the agents: `--approve` and `--no-approve`
  answer it outright, and with neither it prints each toolbox's policy and asks — before
  the question, so a `y` is informed. A run with nobody to ask approves nothing.
- **`ghostai agent install <id>`** is the scriptable single-shot beside it, with `--force`
  to overwrite an entry that may carry your own edits, and `ghostai agent list` /
  `ghostai preset list` to see what exists.
- **The catalogue is a separate repository**,
  [`GhostAI-presets`](https://github.com/therezor/GhostAI-presets), published as
  `@ghostwire/presets` and versioned on its own cadence. It is fetched on demand into
  `~/.ghostai/catalogue`; `--from` reads a checkout instead, for anyone writing a preset,
  and is never fetched over.
- **A preset can take part of a box rather than all of it.** `toolbox.tools` maps a
  program to `allow`, `ask` or `deny`, and `"*"` sets the default for every program the
  manifest declares and the map does not name — so `{"*": "deny", "nmap": "allow"}` is
  "only nmap" in one line. A denied program is never sent to the model and is left out of
  the prompt section too, which is the point: a two-dozen-program box costs 60–80 tokens
  per entry on every request. These are defaults an agent's own `tools` map still
  overrides, not a boundary — `exec` reaches the program either way, and the container is
  what contains it.

### Extending it

- MCP servers over stdio, Streamable HTTP or SSE, with OAuth where a server wants it.
- Extensions: a directory that adds tools, channels, providers, prompt sections and slash
  commands, approved by a digest over every byte it holds.
- Scheduled jobs, cron and one-shot, where a heartbeat is a job rather than a second
  system.

### Known limits

Stated here for the same reason they are stated in the README: a feature list that omits
them reads as more finished than it is.

- **`openai-chat` is the only wire adapter that ships.** The `anthropic` registry entry
  names `anthropic-messages` and is refused at construction rather than falling back;
  reaching it means an endpoint that speaks `openai-chat` or an extension contributing the
  wire.
- **English is the only shipped locale.** The translation layer is complete — typed
  bundles, negotiation, errors carrying keys across packages, two CI gates — and adding a
  language is a folder plus a line.
- **A heartbeat's `targets` do not reach a channel yet.** The decide/run/evaluate triad
  ships as a scheduled job's payload; delivery does not.
- **Session search is by title and filter, not by message content.**

[0.7.3]: https://github.com/therezor/GhostAI/releases/tag/v0.7.3
[0.7.2]: https://github.com/therezor/GhostAI/releases/tag/v0.7.2
[0.7.1]: https://github.com/therezor/GhostAI/releases/tag/v0.7.1
[0.7.0]: https://github.com/therezor/GhostAI/releases/tag/v0.7.0
