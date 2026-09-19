# Development

**Who this is for:** anyone changing the code. It is the CI gate, the conventions a
linter cannot enforce, the coverage bars, and how to release. If you only want to _run_
DarkWire, [Getting started](getting-started.md) is the page you want;
[CONTRIBUTING.md](../CONTRIBUTING.md) is the short version of this one.

## Setup

```bash
git clone https://github.com/therezor/darkwire.git
cd DarkWire
pnpm install
pnpm build                                       # the web bundle the binary embeds
cargo build --release -p darkwire                 # → target/release/darkwire
```

The Rust workspace under `crates/` needs `rustup` (the compiler version is pinned in
`rust-toolchain.toml` and installs itself on the first `cargo` command) and three cargo
tools the gate runs:

```bash
cargo install cargo-nextest cargo-llvm-cov cargo-deny --locked
cargo build --workspace
```

Node 22 or newer and pnpm 11 (`corepack enable` — the version is pinned in the root
`package.json`).

**Node is a build dependency now, not a runtime one.** It builds the web bundle and runs
the four remaining TypeScript packages' tests; the binary it produces links no
interpreter and reads no `node_modules`. The one place Node can still appear on a user's
machine is an extension that happens to be written in JavaScript, and that runs as its
own process over JSON-RPC — its interpreter is its business, not the product's. There is
no version floor to state any more: `node:sqlite`, which is where the old 22.13 floor came
from, has been replaced by SQLite compiled into the binary through `rusqlite`.

A debug build is fine for everything except a demo: `cargo build -p darkwire --features
test-hooks` is what the e2e harness looks for by default (`target/debug/darkwire`, unless
`DARKWIRE_BIN` names another), and CI hands it a release build through that variable
because the suite is slow enough already.

**`pnpm build` comes before the first `cargo` command, including `cargo check` and
whatever rust-analyzer runs on open.** `rust-embed` compiles `packages/web/dist` into the
binary, so on a fresh clone — where `dist/` is gitignored and therefore absent —
`crates/server`'s build script stops with a sentence naming the command that fixes it.
That is deliberate: the alternative, which this repository shipped for exactly one commit,
is a build that quietly drops the UI and a binary whose `GET /` is a JSON 404. To work on
the Rust side without building the bundle at all, set `DARKWIRE_HEADLESS_BUILD=1` — it is
what the CI `rust` job does, and it is an editor environment variable as easily as a shell
one.

## The gate

**`pnpm check` is not the CI gate.** It runs `typecheck`, `lint` and `test`; CI runs five
more things in that job alone and three more jobs after it — most local sessions that end
green and then fail CI fail on `format:check`, which `pnpm check` never calls.

CI is [`.github/workflows/ci.yml`](../.github/workflows/ci.yml), and it is four jobs.
Run all of it before calling something done:

```bash
# job: check
pnpm typecheck
pnpm lint
pnpm --filter @darkwire/web exec tsx src/tokens/run-gates.ts   # design token gates
pnpm format:check                                             # ← the usual failure
shellcheck -s sh install.sh                                   # the line the README pipes into a shell
pnpm i18n:check
pnpm protocol:check                                           # emit the zod JSON Schemas, then diff them
pnpm test
pnpm build

# job: coverage — per-package thresholds, stricter than the default
pnpm test:coverage                                            # the crates' bars are in the rust job

# job: e2e — Playwright, both colour schemes, against the real binary
pnpm build                                                    # the SPA the binary embeds
cargo build --release -p darkwire --features test-hooks        # the server under test
DARKWIRE_BIN=target/release/darkwire pnpm --filter @darkwire/e2e test:e2e

# job: rust — the Cargo workspace under crates/
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo deny check                                              # advisories, licences, sources
cargo nextest run --workspace
cargo llvm-cov nextest --workspace --json --output-path coverage.json
node scripts/coverage-gate.mjs coverage.json                  # per-crate bars
```

Notes that save a cycle:

- **`pnpm format:check` fails, `pnpm format` fixes it.** Prettier is not wired into
  `lint`. When it reports files you did not touch, format only your own.
- **e2e needs both builds first.** The suite spawns `darkwire serve` as a subprocess and
  the binary embeds the SPA, so `pnpm build` comes before `cargo build`;
  a missing binary fails with a sentence naming `cargo build`, and a missing bundle
  fails the Rust build at compile time, naming `pnpm build`. The `rust` job is the one
  place that is not true, and it says so with `DARKWIRE_HEADLESS_BUILD=1`.
- **The fidelity spec skips without a baseline.** `2 skipped` is the healthy result.
- **A green local e2e run is evidence, not proof.** CI runs 2 workers on a shared runner;
  a laptop runs 5 with nothing competing. When CI reports a failure the local suite will
  not reproduce, re-run that spec under load before blaming CI:

  ```bash
  pnpm --filter @darkwire/e2e exec playwright test <spec> --repeat-each=6
  ```

### Scripts

| Command                                 | Does                                                                       |
| --------------------------------------- | -------------------------------------------------------------------------- |
| `pnpm typecheck`                        | `tsc -b` across all project references                                     |
| `pnpm lint` / `lint:fix`                | ESLint with type-aware rules                                               |
| `pnpm format` / `format:check`          | Prettier                                                                   |
| `pnpm test` / `test:watch`              | Vitest                                                                     |
| `pnpm test:coverage`                    | Vitest with the per-package gates enforced (`vitest.config.ts`)            |
| `pnpm build`                            | Turborepo build across the graph                                           |
| `pnpm i18n:extract` / `i18n:check`      | Regenerate the locale bundles / fail if they are out of step               |
| `pnpm --filter @darkwire/web dev`       | Vite dev server, proxying `/api` and `/ws` to a running `darkwire serve`   |
| `pnpm --filter @darkwire/e2e test:e2e`  | Playwright, both colour schemes                                            |
| `pnpm screenshots`                      | Regenerate the documentation's images into `docs/screenshots/`             |
| `pnpm demo`                             | Build, then regenerate the animated terminal cast in the README            |
| `node scripts/gen-packages.mjs`         | Regenerate `packages/protocol`'s manifest; carry the version into the rest |
| `cargo fmt --all --check` / `cargo fmt` | rustfmt, the Rust half of `format:check`                                   |
| `cargo clippy ... -- -D warnings`       | Clippy with the workspace lint table; pedantic is an error in CI           |
| `cargo deny check`                      | Advisories, licences, wildcard versions, git sources (`deny.toml`)         |
| `cargo nextest run --workspace`         | The Rust tests                                                             |
| `node scripts/coverage-gate.mjs`        | Per-crate coverage bars over a `cargo llvm-cov --json` report              |

### Coverage gates

Blocking in CI, and the table is split across two files because the toolchains are.
Default is 70 lines / 65 branches on both sides.

**The crates**, in `scripts/coverage-gate.mjs`, enforced by
`cargo llvm-cov nextest --workspace --json` piped through it:

| Crate                                          | Lines | Branches |
| ---------------------------------------------- | ----- | -------- |
| `security`                                     | 95    | 95       |
| `i18n`                                         | 90    | 90       |
| `core`, `channels`, `tui`                      | 90    | 85       |
| `agent`, `runtime`, `extension-host`, `server` | 85    | 80       |
| `providers`, `tools`, `mcp`                    | 80    | 75       |
| everything else                                | 70    | 65       |

**The two remaining TypeScript packages that hold decision logic**, in
`vitest.config.ts`, enforced by `pnpm test:coverage`: `web` at 85/80 and `i18n` at
90/90. `protocol` is deliberately absent — it is schema declarations exercised by the
whole suite, so a ratio over it would measure the consumers rather than the package.

`security` carries the strictest bar on either side because an untested branch there is a
vulnerability, not just a bug. A new branch in a guard needs a test, or the coverage job
fails while `pnpm test` and `cargo nextest` both pass.

Two mechanical notes. `cargo llvm-cov` has a single threshold for the whole workspace,
which is why the bars go through a script at all; "branches" there is llvm-cov's region
coverage, the closest thing the stable toolchain measures. And only `src/` counts on
either side — `crates/*/src/**` minus `testkit.rs` and `main.rs`, `packages/*/src/**.ts`
— so a testkit, which runs on every test and scores 94–100%, no longer pads whatever
holds it.

## Releasing

One binary, four platforms, attached to a GitHub release. Nothing goes to npm: the
product is `darkwire`, the web bundle is compiled into it, and an install is a download
rather than a dependency tree.

There is still one version, and now it lives in two files that must agree — `Cargo.toml`
is what the binary reports through `CARGO_PKG_VERSION`, and `package.json` is what the
web bundle and the two remaining TypeScript packages carry. `crates/cli/tests/version.rs`
fails when they disagree, and so does the release workflow, so a tag cannot ship a binary
whose `--version` contradicts the release it came from.

Two edits and a tag:

```bash
# 1. the version, in both places
#    "version": "1.1.0"   in package.json
#    version = "1.1.0"    in Cargo.toml, under [workspace.package]
# 2. carry the first into the remaining TypeScript manifests
node scripts/gen-packages.mjs

git commit -am 'Release 1.1.0' && git tag v1.1.0 && git push origin main v1.1.0
```

The hand-edited `VERSION` and `SERVER_VERSION` literals are gone. They existed because a
bundle in `dist/` resolves a relative manifest read differently in the workspace and in a
published tarball, and a silently wrong version is worse than a missing one. A compiled
binary has no such ambiguity: `env!("CARGO_PKG_VERSION")` is fixed at build time and is
what both `darkwire --version` and `GET /api/status` report.

**Name the tag in the push.** `--follow-tags` is the spelling this said for three
releases and it pushes _annotated_ tags only, so a `git tag v1.1.0` goes nowhere: the
commit lands, the release never fires, and nothing says so. Either name the tag as above
or make it annotated with `git tag -a`.

The tag fires [`release.yml`](../.github/workflows/release.yml), which runs the whole
gate again — both halves of it, because a tag can be pushed from a branch CI never saw —
checks the tag against both manifests, builds, and attaches the tarballs and a
`SHA256SUMS` file to the release.

**Each binary is built on its own architecture rather than cross-compiled.** The
credential vault shells out to the platform keychain and the container runner signals
process groups; both are platform code, and a cross-linker is one more thing that can be
subtly wrong in a way only a user discovers. The matrix is `aarch64`/`x86_64` on macOS
and on Linux.

**The runners are pinned, and the Linux pin is a promise to users rather than to us.** A
binary links the glibc it was built against and refuses to start on anything older, so
the runner chooses who can run the release: `ubuntu-latest` is 24.04, whose glibc 2.39
rules out Debian 12, Ubuntu 22.04 and RHEL 9. The matrix builds on 22.04, which puts the
floor at 2.35. On the macOS side `macos-13` was the last free Intel image and was retired
in December 2025; `macos-15-intel` is what keeps `x86_64-apple-darwin` a native build.

**Rehearse before tagging.** `release.yml` also answers `workflow_dispatch`, and a manual
run has no tag, so it builds all four targets, proves each binary serves its UI, uploads
the tarballs to the workflow run and stops — the release job is gated on a tag. This is
the only way to compile three of the four targets at all: `ci.yml` is x86_64 Linux from
top to bottom, so without a rehearsal a macOS-only compile error is something the release
discovers.

**The release notes are the changelog's, not the commit log's.** The release job takes the
`## [x.y.z]` section matching the tag and fails when there is not one, which makes an
unwritten changelog entry a red workflow rather than a published release nobody described.

**Windows is deliberately absent.** The two modules above are POSIX here. The workspace
jail was written platform-independent on purpose — it treats `\` as a separator and
handles drive letters on every platform — so adding Windows later is a port of the
keychain and the process teardown, not a rewrite.

**The web bundle is a build input, not a separate artifact.** `rust-embed` compiles
`packages/web/dist` into the binary, so `pnpm build` runs before `cargo build` in every
matrix job — the whole graph, not `--filter @darkwire/web`, because the web app imports
`@darkwire/protocol` and `@darkwire/i18n` and their `exports` resolve to `dist/`
outside a dev server. Turbo is what knows to build those two first. Without it the build fails at compile time, which is
the intended failure and one step earlier than resolving a path at startup used to give.
For a build with no bundle — a headless server, or a CI job that only wants the tests —
`DARKWIRE_HEADLESS_BUILD=1` skips the embed and `GET /` answers a JSON 404 with a
sentence.

## Screenshots

Every picture in the README and in [Web UI](web-ui.md) is generated. `pnpm screenshots`
builds both halves — the bundle and the binary, because the harness spawns the real
`darkwire serve` — boots that harness over a scripted provider, drives each screen to the
state worth showing, and writes twenty PNGs into `docs/screenshots/`, one per screen per
colour scheme. They are committed, because GitHub cannot run a build step to render a
README.

Regenerate them whenever the UI changes, and commit what comes out. **Two runs on one
machine produce byte-identical files**, so an image that shows up in `git status` means
the UI moved — which makes the diff worth reading rather than noise to skip. Two
_different_ machines will not agree, because font rasterisation is the platform's; there
is no CI gate on these for that reason.

`packages/e2e/src/screenshots/capture.ts` is a sibling of `fidelity/capture.ts`, not a
mode of it — the fidelity tool refuses to run without a reference product, and this one
has to work on any clone. Its header explains the five things that had to be pinned down
to get a stable image, and the surprising one is not the clock or the fonts: it is that
seed rows written in a loop share a millisecond often enough to make a list ordered
`time DESC, id ASC` reshuffle between runs.

### What a pty cannot tell you about a terminal

Everything the chat TUI draws is asserted from the bytes a completed paint emitted, in
`crates/tui/tests/renderer.rs` against an in-memory terminal. That catches what the
program wrote. It cannot catch what an emulator does with it, and two of those behaviours
decide whether the screen is right:

- **What `\x1b[2J` does with the rows it erases.** iTerm2, Terminal.app, VTE, WezTerm and
  kitty move them into the scrollback; xterm and Alacritty drop them. That split is why
  nothing here may emit it: on most terminals a repaint left a copy of whatever was on
  screen in the history, and a young session's screen is the welcome banner. The test that
  holds the line is `nothing_it_ever_writes_clears_the_screen`.
- **Whether a resize rewraps rows that were hard terminated.** The repaint walks up to the
  strip's first row by counting how many rows each drawn line needs at the new width. A
  terminal that does not rewrap is overcounted, which erases rows of conversation from the
  visible screen rather than stranding a fragment of the strip on it. The conversation is
  still in the history, so it is the harmless direction to be wrong in.

A pty is not an emulator, so neither is testable here. After a change to
`crates/tui/src/renderer.rs`, run the binary in iTerm2, Terminal.app, Ghostty, kitty,
WezTerm and Alacritty, and in each one: narrow the window a column at a time eight times,
press `ctrl-l` three times, scroll up, then select and copy a line of the conversation.
One banner, no stranded fragments, no duplicated conversation, the composer on the last
row throughout.

What a pty _does_ check is the sequences themselves. `scripts/ptyrec.py` records a real
session, and the cast is a plain list of the bytes the binary wrote:

```bash
python3 scripts/ptyrec.py 92 24 /tmp/chat.cast /tmp/keys.json -- ./target/release/darkwire chat
```

### The terminal cast

`pnpm demo` regenerates `docs/screenshots/demo.svg`, the animated recording at the top of
the README. `scripts/demo-provider.mjs` stands up a mock `openai-chat` endpoint,
`scripts/ptyrec.py` records **bash** on a real pty — typing `darkwire chat`, waiting for the
TUI, asking a question — and `svg-term` renders the cast to a self-contained SVG. Every
byte on screen came back through the pty from the real binary; the keystroke schedule is
authored so the run reproduces.

Three things that are not obvious:

- **A pipe is not a terminal.** Piping `darkwire chat` gets the plain stream it writes for a
  machine — no session header, no composer, no status bar, no spinner. The child has to
  believe it is on a tty, and `script(1)` needs a controlling terminal that tooling does
  not always have. Python's `pty` is stdlib and needs nothing.
- **The mock must be its own process.** `demo-cast.mjs` drives the recorder with
  `execFileSync`, which blocks its event loop for the length of the take. A server in that
  process accepts nothing, and the recording is ten seconds of `thinking…`.
- **The cast ends at its last byte**, and the answer lands milliseconds after the question
  is sent. Without the trailing hold in `ptyrec.py` the loop is all typing and a flash of
  the reply.

The first cast had a doubled status bar and unrenderable glyphs, and both were the same
bug — the recorder decoded each `os.read` chunk on its own, so a multi-byte character
split across a read boundary became two U+FFFD, and a split _escape sequence_ corrupted
the repaint that erases the footer. `ptyrec.py` now holds partial sequences in an
incremental decoder. If either artifact comes back, that is where to look — not at
svg-term.

## TypeScript conventions

Four packages are still TypeScript — `protocol`, `i18n`, `web` and `e2e` — and these are
theirs. The rules that used to live here about the runtime (cancellation, exec, injected
clocks) moved with the code; see [Rust conventions](#rust-conventions) below.

- **ESM only.** `"type": "module"` everywhere, `.js` extensions in relative imports
  (NodeNext resolution).
- **`isolatedDeclarations` is on** everywhere except `protocol`. Every exported function
  needs an explicit return type; this keeps declaration emit fast and makes the public API
  surface reviewable in diffs. `protocol` is the exception because a Zod schema export
  _is_ an inference result, and annotating it by hand would recreate the drift the schemas
  exist to prevent.
- **`tsup` owns the JavaScript, `tsc -b` owns the types.** `emitDeclarationOnly` keeps
  `tsc` from overwriting the bundle, `clean: false` keeps `tsup` from deleting the
  declarations. `pnpm build` runs both in that order. Delete `dist` to force a full
  rebuild. (`web` is a Vite app and does neither; `e2e` is never built at all.)
- **Zod is the single source of truth** for the wire — config, messages and tool
  parameters. Types come from `z.infer`, JSON Schema from `z.toJSONSchema`. Never
  hand-write a type a schema could produce, and every exported `*Schema` must be
  registered in `schemas.ts`, which a test enforces. `crates/protocol` mirrors the result
  and a drift test compares the two JSON Schema documents, so a schema edited on one side
  only fails CI rather than reaching a browser.
- **Errors are values, not strings.** Never branch on a substring of an error message.
  Return a typed discriminated union with a `kind`.
- **No `Math.random()`.** Inject a generator so tests are deterministic; use `node:crypto`
  for anything security-relevant. Also lint-enforced.
- **Injected `Clock` and `fetch`.** Tests use fake timers and a mock dispatcher; nothing
  sleeps and nothing touches the network.

## Rust conventions

Everything under `crates/` is one crate per former TypeScript package, same names and
the same layering as the diagram below. Cargo `[dependencies]` are the mechanical
enforcement: a crate that does not list `darkwire-server` cannot `use` it.

- **rustfmt and clippy own the style.** `rustfmt.toml` is defaults plus a 100-column
  width (rustfmt's own default; the 80-column rule is Google's TypeScript guide).
  `[workspace.lints]` in the root `Cargo.toml` denies `clippy::all` and warns on
  `pedantic`, which CI promotes to an error with `-D warnings`. A new `#[allow]` needs a
  one-line comment saying why. `clippy.toml` holds the `doc_markdown` allow-list of
  product names and the denied methods.
- **`#![forbid(unsafe_code)]`** in every crate. Anything needing unsafe goes through a
  dependency.
- **Errors are values.** `darkwire_core::WireError { kind, message, retryable, details }`
  with the closed fifteen-variant `ErrorKind` and its per-kind `retryable` defaults;
  `thiserror` below the binary, `anyhow` only in `crates/cli/src/main.rs`. Never branch
  on a message substring.
- **One cancellation mechanism.** `tokio_util::sync::CancellationToken`, threaded from
  the transport through the hub, the loop, the provider request, the tool and the
  child process; `child_token()` is how a timeout composes. No running flags.
- **Streams, not generators.** A turn is a stream of events plus a completion; dropping
  the stream cancels the token, which is the "abandoning the iterator unwinds the turn"
  property in Rust.
- **Injected `Clock` and `RandomSource`.** `rand::rng`, `rand::random` and
  `SystemTime::now` are clippy-denied. Tests pause tokio's clock.
- **No shell, ever.** `Command::new(argv[0]).args(&argv[1..])`.
- **Serialisation is the wire contract.** `#[serde(rename_all = "camelCase")]`,
  `#[serde(default)]` on every field zod `.default()`s in the client-to-server
  direction, `deny_unknown_fields` only where zod uses `strictObject`,
  `schemars::JsonSchema` on every mirrored type.
- **Tests live in `crates/<crate>/tests/` mirroring `src/`**, the same rule as
  `packages/<pkg>/test/`; coverage measures `src/` only. Inline `#[cfg(test)]` is for a
  private helper with no public path, with a line saying why. Property tests use
  `proptest`. A testkit is `src/testkit.rs` behind a `testkit` cargo feature.
- **Port reasoning, not commentary.** A comment survives the port only if it is still
  true in Rust. Anything about Node, tsup, zod inference, `node:sqlite`, pino,
  `AbortSignal`, async generators, the module registry or a TypeScript identifier that
  no longer exists is deleted, not translated. No "ported from `x.ts`" breadcrumbs.
- **One version.** The root `Cargo.toml` and the root `package.json` must agree;
  `crates/cli/tests/version.rs` fails when they do not, and `env!("CARGO_PKG_VERSION")`
  is what `darkwire --version` and `GET /api/status` report.
- **Dependencies are pinned exact** in `[workspace.dependencies]` and `cargo deny` is the
  analogue of pnpm's release-age policy: no git sources, no wildcards, an allow-listed
  licence set. Cargo has no release-age hold, so `cargo update` is reviewed rather than
  delayed.

## Layering

```
{ protocol, i18n } → core → security → { providers, tools } → { mcp, agent } ─┬→ runtime → server ┐
{ protocol, i18n } → web                                                      │                   │
             core → channels ────────────────→ extension-host ────────────────┘                   ├→ cli
                    tui                                                                           ┘
```

Every name on that diagram except `web` is a crate under `crates/`, and Cargo is the
enforcement: a crate that does not list `darkwire-server` in `[dependencies]` cannot `use`
it, which is a compile error rather than a lint. `web` is the TypeScript half, and pnpm's
isolated `node_modules` does the same job for it — an undeclared `@darkwire/x` import
fails to _resolve_.

The agent must never reach back into the HTTP server.
[Architecture](architecture.md#layering) explains why `tui` sits beside the roots.

## Working on the UI

There is no CSS framework and there are three token gates. The full picture is in
[Web UI](web-ui.md#design-tokens); the short version:

- `styles/tokens.css` is the only file allowed a raw colour or a `px` literal.
- `pnpm --filter @darkwire/web lint` runs ESLint _and_ the gates.
- `/tokens` in the running app renders every token and primitive on one page.
- A contrast test resolves the sheet in both themes and holds every text-on-surface
  pairing to WCAG AA, so a seed edit that darkens text past the line fails the suite.

**A UI build does not reach a running binary.** `rust-embed` compiles the bundle in, so
the server is serving the copy that existed when it was _built_ — rebuilding
`packages/web/dist` underneath it changes nothing at all until `cargo build` runs again.
For an edit-reload loop use the Vite dev server, which proxies `/api` and `/ws` to a
running `darkwire serve`; to test a bundle against the real server without a recompile,
`darkwire serve --ui packages/web/dist` reads it from disk instead, which is what the e2e
harness does.

## End-to-end tests

```bash
pnpm build
cargo build -p darkwire --features test-hooks
pnpm --filter @darkwire/e2e exec playwright install chromium   # once
pnpm --filter @darkwire/e2e test:e2e
```

Every spec spawns its own `darkwire serve` against a scripted model, so nothing reaches the
network and nothing shares state. **The colour scheme is a Playwright project**, which
means every assertion runs twice.

The binary is the real one, which is the point of the rebuild: `test-hooks` is a cargo
feature rather than a code path, so what the suite drives differs from a release build
only in the seams the harness needs to reach. `DARKWIRE_BIN` names the binary when it is
not `target/debug/darkwire`, and a missing one fails with a sentence naming `cargo build`
rather than as twenty timed-out specs.

### Never assert a transient state

This is what broke CI four runs in a row. A spec waited for the line an approval card shows
_between_ the operator answering and the tool result arriving. The scripted provider
answers inside a frame, so whether that line is ever painted depends on how the runner
interleaves the re-render with the socket message. It passed locally every time and failed
CI every time.

Assert the **durable** state a step settles into — the card's `Succeeded`/`Failed` status,
the text in the transcript — and cover the in-between wording in a component test, where
the state can be held still. The rule of thumb: **if the only reason you can see it is
that the machine was slow, it does not belong in an `expect`.**

### The fidelity gate

`fidelity.spec.ts` compares the shell's geometry and colour ramps against a checkout of a
reference build. That checkout is not in this repository and is not required — point
`DARKWIRE_FIDELITY_ORIGINAL` at one to run the gate, and
`pnpm --filter @darkwire/e2e baseline` to write the side-by-side captures. Without it the
gate skips.

## Translations

Two CI gates catch opposite halves of the problem, and neither substitutes for the other:

- **`pnpm i18n:check`** runs the extractor and fails if the locale bundles moved — this
  finds a key that was used in code and never reached the bundle a translator reads.
- **`untranslated.test.ts`** sweeps the source for English prose in JSX text and in
  `aria-label`, `placeholder`, `title`, `alt` and friends — this finds copy that never
  became a key at all.

The JSON bundle _is_ the type: keys are walked into a dotted-path union, so a typo is a
compile error rather than a string that renders as itself.

## Areas that touch more than they look like they do

**Auth.** The credential surface spans both halves of the wire
(`packages/protocol/src/rest.ts`, where every exported `*Schema` must also be registered
in `schemas.ts`, and its mirror `crates/protocol/src/rest.rs`), `crates/server/src/`
(`auth_store.rs`, `auth.rs`, `login_throttle.rs`, `signing.rs`, `boot.rs`,
`routes/auth.rs` and `manifest.rs`, the last because the auth-matrix test iterates it),
the two web overlays, the Account settings panel, **and the e2e harness** — the last of
which only unit tests plus e2e catches.

**Config.** A new key means the schema, the patch merge rules if it is a record, the
settings panel, and this documentation. A key that parses but is never read should say so
in its doc comment, so the next person does not go looking for the consumer.
