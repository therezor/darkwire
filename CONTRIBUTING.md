# Contributing

## Setup

```bash
git clone https://github.com/therezor/GhostAI.git
cd GhostAI
pnpm install
pnpm build                                   # the web bundle the binary embeds
cargo build --release -p ghostai             # → target/release/ghostai
```

pnpm 11 (`corepack enable`) and Node 22 or newer for the bundle; `rustup` for
everything else, with the compiler version pinned in `rust-toolchain.toml` and
installed on the first `cargo` command. The gate also wants three cargo tools:

```bash
cargo install cargo-nextest cargo-llvm-cov cargo-deny --locked
```

**Node is a build dependency, not a runtime one.** GhostAI is one binary with the
browser UI compiled into it, so nothing on a user's machine needs Node — unless
they install an extension that happens to be written in JavaScript, which runs as
its own process and brings its own interpreter.

`pnpm build` is not optional even for a server-only change: `rust-embed` compiles
`packages/web/dist` into the binary, so a missing bundle fails the Rust build at
compile time. `GHOSTAI_HEADLESS_BUILD=1` skips the embed when you genuinely want a
binary without a UI.

## Before you open a pull request

**`pnpm check` is not the gate.** It runs `typecheck`, `lint` and `test`; CI is four
jobs, and the one that catches most people is `format:check`, which `pnpm check`
never calls.

```bash
pnpm typecheck
pnpm lint
pnpm --filter @ghostwire/web exec tsx src/tokens/run-gates.ts   # design token gates
pnpm format:check                                             # ← the usual failure
pnpm i18n:check
pnpm protocol:check                                           # zod schema dump the Rust drift test reads
pnpm test
pnpm build
pnpm test:coverage                                            # stricter than pnpm test

cargo fmt --all --check                                       # the Rust workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo deny check
cargo nextest run --workspace
cargo llvm-cov nextest --workspace --json --output-path coverage.json
node scripts/coverage-gate.mjs coverage.json

# Playwright, both colour schemes, against the real binary
cargo build --release -p ghostai --features test-hooks
GHOSTAI_BIN=target/release/ghostai pnpm --filter @ghostwire/e2e test:e2e
```

[Development](docs/development.md) is the full walkthrough — what each gate catches, the
coverage bars, the e2e suite and the UI loop. Read it once before your first change.

Say what you actually ran. An unqualified "all tests pass" that meant `pnpm test` is how
most red CI runs happen; "typecheck, lint, test and format, not e2e — the change is
server-only" is a useful sentence and takes no longer to write.

## The rules a linter cannot enforce

Two style guides, one per language, and a linter owns each. For TypeScript it is
[Google's][gts], with `eslint.config.js` plus `.prettierrc.json` as the
machine-checkable half — 80 columns, no leading or trailing underscores, `private`
rather than `#private`. For Rust it is `rustfmt.toml`, `clippy.toml` and the
`[workspace.lints]` table in the root `Cargo.toml`, with `pedantic` warning locally
and an error in CI. Run the linters and you have complied. The rest:

- **Tests mirror `src/` from outside it.** `packages/web/src/chat/markdown/blocks.ts`
  is tested by `packages/web/test/chat/markdown/blocks.test.ts`;
  `crates/core/src/session_store.rs` by `crates/core/tests/session_store.rs`.
  Nothing under `src/` is a test. In TypeScript reach source through the `#src/…`
  alias (`@/…` in web), never a relative path; in Rust a testkit is
  `src/testkit.rs` behind a `testkit` cargo feature, and neither is measured by
  coverage.
- **`#![forbid(unsafe_code)]` in every crate**, no exceptions. Anything that needs
  unsafe goes through a dependency.
- **Never assert a transient state in an e2e test.** Assert what a step _settles_ into —
  a card's final status, the text in the transcript. If the only reason you can see
  something is that the machine was slow, it does not belong in an `expect`; cover the
  in-between wording in a component test where the state can be held still.
- **Errors are values.** Return a typed union with a `kind`; never branch on a substring
  of a message.
- **No shell, ever.** `exec` takes an argv vector and spawns it directly —
  `Command::new(argv[0]).args(&argv[1..])`. There is no string for a metacharacter
  to be interpreted in, which is why there is no metacharacter deny-list either.
- **Comments say why, not what — and say it once.** This codebase carries its reasoning
  in the source, and the docs are written from it rather than from each other. If you
  close a subtle hole, the comment explaining which hole is the more valuable half of the
  change. But a module header is a statement of the constraints a reader must not break,
  not an essay about how the design was reached: write the invariant, not the story of
  arriving at it, and never narrate a past state the code no longer has.

[gts]: https://google.github.io/styleguide/tsguide.html

## Changes that touch more than they look like they do

- **Anything about credentials or auth** spans both halves of the wire
  (`packages/protocol/src/rest.ts`, where every exported `*Schema` must be registered
  in `schemas.ts`, and its mirror `crates/protocol/src/rest.rs`), five modules in
  `crates/server/src/` (`auth_store.rs`, `auth.rs`, `login_throttle.rs`,
  `signing.rs`, `boot.rs`) plus `routes/auth.rs` and `manifest.rs`, both web
  overlays and the Account settings panel, **and the e2e harness** — the last of
  which only unit tests plus e2e catch.
- **Any string a person reads** needs a translation key, and there are two CI gates
  pulling in opposite directions: one finds copy that never became a key, the other finds
  keys that never reached the bundle. `pnpm i18n:extract` fixes the second.
- **Anything under `packages/web`** is subject to the design token gates: no `px` outside
  `tokens.css`, no raw hex/rgb/oklch outside it, no `--color-accent` in a text or border
  position. If the UI changed visibly, regenerate the screenshots with `pnpm screenshots`
  and commit them.
- **Adding a crate** needs a `members` entry and a `[workspace.dependencies]` row in
  the root `Cargo.toml`, and a bar in `scripts/coverage-gate.mjs` — a crate with no
  entry there is held to the 70/65 default, which is rarely what a new crate should
  be held to.
- **Adding a TypeScript package** needs both of its `tsconfig.json` references added
  at the root, and an entry in `scripts/gen-packages.mjs` if the manifest is to be
  generated — `packages/protocol` is, and editing a generated manifest by hand is
  reverted by the next run.

## Reporting a bug

Include the version (`ghostai --version`), what you expected, what happened, and enough to
reproduce it. `--verbose` and `GHOSTAI_DEBUG=1` (which prints stack traces rather than the
operator sentence) usually turn a vague report into a fixable one.

**Security issues do not go in an issue.** See [SECURITY.md](SECURITY.md).

## Licence

By contributing you agree that your work is licensed under the [MIT licence](LICENSE),
the same as the rest of the project.
