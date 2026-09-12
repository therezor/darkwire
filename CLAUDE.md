# Working in this repo

## Run the tests before you call a task finished — every time

Not "when the change looks risky", not "when it touched tests". Every task, before
reporting it done. The list below is the whole gate; a task is finished when that
list is green, and saying it is finished on anything less is a false claim the user
finds out about in CI.

Two habits that make this cheap rather than a chore:

- Run it **before** writing the summary, not after. A failure found then is part of
  the task; a failure found by CI is a second session.
- Report what you actually ran. If you skipped e2e because the change was
  server-only, say that — an unqualified "all tests pass" that meant `pnpm test`
  is how the last four red CI runs happened.

## Before saying a task is done, run what CI runs

`pnpm check` is **not** the CI gate. It runs `typecheck`, `lint` and `test`, and CI
runs five more things in that job alone — and three more jobs after it — so most
sessions that end "green" and then fail CI fail on `format:check`, which
`pnpm check` never calls.

CI is `.github/workflows/ci.yml`, and it is four jobs. Run all of it. When this
list and [Development](docs/development.md#the-gate) disagree, that page is right —
it is written from the workflow, and this is the copy that goes stale:

```bash
# job: check
pnpm typecheck
pnpm lint
pnpm --filter @ghostwire/web exec tsx src/tokens/run-gates.ts   # design token gates
pnpm format:check                                             # ← the usual failure
shellcheck -s sh install.sh                                   # the line the README pipes into a shell
pnpm i18n:check                                               # extract, then diff the bundles
pnpm protocol:check                                           # emit the zod JSON Schemas, then diff them
pnpm test
pnpm build

# job: coverage — per-package thresholds, stricter than the default 70/65
pnpm test:coverage

# job: e2e — Playwright, both colour schemes, against the real binary
pnpm build                                                    # the SPA the binary embeds
cargo build --release -p ghostai --features test-hooks        # the server under test
GHOSTAI_BIN=target/release/ghostai pnpm --filter @ghostwire/e2e test:e2e

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
  `lint`. When it reports files you did not touch, format only your own and say so —
  do not sweep unrelated files into the diff.
- **e2e needs both builds first.** The suite spawns `ghostai serve` as a subprocess and
  the binary embeds the SPA, so `pnpm build` comes before `cargo build`;
  a missing binary fails with a sentence naming `cargo build`, and a missing bundle
  fails the Rust build at compile time.
- **The fidelity spec skips without a baseline.** `2 skipped` is the healthy result,
  not a problem to fix.
- **A visible UI change means `pnpm screenshots`.** The images in the README and
  `docs/web-ui.md` are generated and committed; two runs produce byte-identical
  files, so a picture in `git status` means the UI moved. Not a CI gate — nothing
  catches a stale screenshot but a reader.
- **`pnpm i18n:check` runs the extractor and then diffs `web.json`.** A
  new `t()` call whose key never reached the bundle fails it; `pnpm i18n:extract`
  fixes it. Note `keepRemoved: true` — the extractor never prunes, so a key going
  stale is _not_ something this gate can see. The diff is scoped to `web.json`
  because `cli.json` is hand-maintained now: the binary embeds it and generates its
  key constants from it, so a missing key is a compile error instead.
- **The raised coverage gates live in two files now**, against a 70/65 default:
  `vitest.config.ts` carries the two TypeScript packages that still hold decision
  logic, and `scripts/coverage-gate.mjs` carries one bar per crate — `security` is
  95/95 there. Do not keep a copy of the numbers here: read them from those two
  files, which are the only places they are true, and see
  [Development](docs/development.md#coverage-gates) for the rationale. A new branch
  in a guard needs a test, or the coverage job fails while `pnpm test` and
  `cargo nextest` both pass.
- **A green local e2e run is evidence, not proof.** CI runs on 2 workers on a shared
  runner; a laptop runs 5 with nothing else competing. When CI reports a failure the
  local suite will not reproduce, re-run just that spec under load before concluding
  it is CI's fault:

  ```bash
  pnpm --filter @ghostwire/e2e exec playwright test <spec> --repeat-each=6
  ```

### Never assert a transient state in an e2e test

Assert the **durable** state a step settles into — the card's `Succeeded`/`Failed`
status, the text in the transcript — and cover the in-between wording in a component
test, where the state can be held still (`packages/web/test/chat/approval.test.tsx`).
The rule of thumb: if the only reason you can see it is that the machine was slow,
it does not belong in an `expect`.

This broke CI four runs in a row while passing locally every time.
[Development](docs/development.md#never-assert-a-transient-state) has the case.

## Rust: rustfmt and clippy own the style

Everything under `crates/` is Rust, one crate per former TypeScript package, same
names and layering. `cargo fmt --check` and `cargo clippy -D warnings` are the style
guide as far as a change is concerned; `rustfmt.toml`, `clippy.toml` and the
`[workspace.lints]` table in the root `Cargo.toml` hold the rules. Read
[Development](docs/development.md#rust-conventions) for the reasoning; the short
version:

- `#![forbid(unsafe_code)]` in every crate, no exceptions.
- `pedantic` warns locally and is an error in CI (`-D warnings`). A new `#[allow]`
  needs a one-line comment saying why. Product names that trip `doc_markdown` go in
  `doc-valid-idents` in `clippy.toml`, not behind an allow.
- **Errors are values**: `GhostError { kind, message, retryable, details }` with the
  same closed fifteen-variant `kind`; `thiserror` below the binary, `anyhow` only in
  `crates/cli/src/main.rs`. Never branch on a message substring.
- **One cancellation mechanism**: a `CancellationToken` threaded from the transport
  to the child process; `child_token()` composes a timeout. No running flags.
- **Injected `Clock` and `RandomSource`**; `rand::rng` and `SystemTime::now` are
  clippy-denied (`clippy.toml`). No shell, ever: `Command::new(argv[0])`.
- **Tests live outside `src/`, mirroring it** — `crates/<crate>/tests/session_store.rs`
  for `src/session_store.rs`. One rule, two spellings; "Where a test goes" below
  states it once for both stacks.
- **The wire is the contract**: `packages/protocol` (zod) stays the source of truth
  because the browser parses it; `crates/protocol` mirrors it and a drift test
  compares the two JSON Schemas. `#[serde(default)]` on every field zod
  `.default()`s in the client-to-server direction, because the browser omits them.
- **Port reasoning, not commentary.** A comment survives the port only if it is still
  true in Rust. Anything about Node, tsup, zod inference, `node:sqlite`, pino,
  `AbortSignal`, async generators, the module registry or a TypeScript identifier
  that no longer exists is deleted, not translated. No "ported from `x.ts`"
  breadcrumbs, at the crate root or anywhere else.
- One version: the root `Cargo.toml` and the root `package.json` must agree, and
  `crates/cli/tests/version.rs` fails if they do not. There is no hand-edited
  `VERSION` literal any more.

## The style guide is Google's, and the linter owns it (TypeScript)

This repo follows the [Google TypeScript Style Guide][gts]. You do not need to
have read it: the parts a machine can check are in `eslint.config.js` and
`.prettierrc.json`, so `pnpm lint` and `pnpm format:check` are the guide as far
as a change is concerned. Read it when you want to know _why_ a rule is there.

[gts]: https://google.github.io/styleguide/tsguide.html

Two things about it that surprise people:

- **80 columns, not 100.** This is the guide's, and it is the reason almost
  every file was touched at once. That reformat is listed in
  `.git-blame-ignore-revs`, so `git blame` can skip it — GitHub reads the file
  by name, and locally it takes one command per clone:

  ```bash
  git config blame.ignoreRevsFile .git-blame-ignore-revs
  ```

- **No leading or trailing underscores, including on unused parameters.**
  There is no `argsIgnorePattern`. A parameter that is not used is deleted; one
  that cannot be deleted because it sits before a parameter that _is_ used just
  gets an ordinary name. `_x` is not available as an escape hatch.

### The deliberate deviations, and why

Everything below is a place where the guide says one thing and this repo does
another on purpose. If you are about to "fix" one of these, this is the
argument you are arguing with.

- **PascalCase is allowed for values, not only for types.** Two kinds of value
  are PascalCase by an external convention and cannot be renamed without
  breaking what reads them: a React component or context (JSX treats a
  lowercase identifier as an intrinsic element), and a zod schema
  (`ChatMessageSchema`), whose name mirrors the type it produces and which is
  re-exported across every package.
- **`export default` is allowed in `*.config.ts`.** vite, vitest, tsup and
  playwright each load their config by taking the module's default export. The
  guide's rule is about our modules; a file whose shape is dictated by the tool
  reading it is not one. Everywhere else the rule is on.
- **Object and type _properties_ are exempt from naming rules.** They are wire
  formats, HTTP header names, route keys, CSS custom properties and i18n keys —
  data whose spelling is fixed outside this repository, not identifiers.
- **`ignoreRestSiblings` is on.** `const {password, ...rest} = user` has to name
  the key it drops. Without this there is no way to omit a field at all, and the
  guide's own advice is to use rest destructuring for exactly this.
- **Tests may assert object literals.** `{matches: true} as MediaQueryListEvent`
  is the point of a fixture: it supplies the one field under test and no other,
  and the annotation the guide asks for instead would be a compile error. Only
  the object-literal case is relaxed, and only under `test/` — `as` is still the
  required syntax, and product code still annotates.

### `#private` is gone; `private` is the spelling

The guide bans `#ident` in favour of TypeScript's `private`, and converting the
repo rewrote a little over 2,000 declarations and references across 30 files.
Two consequences worth knowing before you reintroduce one by habit:

- A `private` field is a real, enumerable own property. `#x` was invisible to
  `Object.keys`, spread, `JSON.stringify` and a deep-equality assertion;
  `private x` is not. If you write a test that deep-compares an instance, it now
  sees the internals.
- A private field can no longer share a name with a public getter. The pattern
  `#items` + `get items()` does not compile, so the _private_ side gets the new
  name — `private itemsByKey` + `get items()`. Fifteen of those were renamed
  during the conversion; the public surface was left alone deliberately, because
  a getter becoming a property is an API change and a rename of a private field
  is not.

## Where a test goes

**Nothing under `src/` is a test.** One rule, spelled twice because the two stacks
spell directories differently:

- **TypeScript** — every test lives in its package's `test/` directory, mirroring
  `src/`. The test for `packages/web/src/chat/markdown/blocks.ts` is
  `packages/web/test/chat/markdown/blocks.test.ts`.
- **Rust** — every test lives in its crate's `tests/` directory, mirroring `src/`.
  The test for `crates/core/src/session_store.rs` is
  `crates/core/tests/session_store.rs`. An inline `#[cfg(test)]` module is for a
  private helper with no public path, and carries a line saying why.

Coverage measures `src/` only on both sides, which is what the rest of this section
and the coverage gates are about.

The TypeScript half of the rule reaches source through an alias, never a relative
path:

- **`#src/…`** in `protocol` and `i18n` — a package.json `imports` entry, so
  `#src/locale.js` resolves the same way for node, tsc and vitest with no extra
  config.
- **`@/…`** in web, which already had the alias and uses it throughout.

Two consequences worth knowing before you fight them:

- **`test/` is its own TypeScript project** (`packages/<pkg>/test/tsconfig.json`,
  referenced from the root `tsconfig.json`). That is what keeps tests out of
  `rootDir` and so out of the emitted `dist`, and it is what ESLint's
  `projectService` finds when it walks up from a test file. A new package needs
  both of its references added at the root, not one.
- **A test does not share a program with the rest of `src/`**, so a module
  augmentation it used to inherit for free has to be imported by name. A test that
  fails to compile on a type some other file declares with `declare module` is
  this, every time: name the module that carries the augmentation in an
  `import type {} from '…'` and the merge happens again.

**Web is the exception, deliberately.** Its tests are checked by
`packages/web/tsconfig.json` itself rather than a separate project, because that
config sets `noEmit` — and `tsc -b` refuses a reference to a project that disables
emit (TS6310), a reference being a promise of declarations. There is no `dist` to
keep tests out of there either, so the split would buy nothing.

### A testkit lives where the toolchain can export it, and never in coverage

The placement is the opposite in the two stacks, and the reason is the toolchain
rather than a change of mind.

- **TypeScript: `test/testkit/`, outside `src/`.** `src/` is runtime code and a
  testkit is not, and nothing has to be built for another package to import one —
  the `exports` map can name TypeScript source directly, and every consumer of a
  testkit is a test runner that reads TypeScript. In web, `@testkit/render.js` is
  the alias beside the existing `@/`; reach one by alias, never a relative path.
- **Rust: `src/testkit.rs`, behind a `testkit` cargo feature.** Cargo has no way to
  export an unbuilt directory. A crate's public surface is its `lib.rs`, so a test
  double another crate can `use` has to be a module of the library — and a feature
  is what keeps it out of a release build: `crates/<crate>/Cargo.toml` declares
  `testkit = []`, and the crates whose tests need it name
  `features = ["testkit"]` on a dev-dependency.

**Neither is measured, and that is the part worth holding on to.** A testkit runs on
every test, so it scores 94–100% and inflates the ratio for whatever holds it.
`vitest.config.ts` includes `packages/*/src/**/*.ts` and so sees nothing under
`test/`; `scripts/coverage-gate.mjs` skips `crates/*/src/testkit.rs` by name. Both
gates measure product code alone — they got stricter when this was fixed, and no
threshold had to move.

## Auth changes touch more places than they look like they do

The credential surface spans, in four places that do not import each other:

- **Both halves of the wire.** `packages/protocol/src/rest.ts` is the DTOs, and
  every exported `*Schema` must also be registered in `schemas.ts`, which a test
  enforces; `crates/protocol/src/rest.rs` mirrors it, and the drift gate compares
  the two JSON Schemas rather than trusting that they were edited together.
- **The server.** `crates/server/src/auth_store.rs` (passwords, session tokens and
  the one-time setup code), `auth.rs` (the cookie and `Bearer` middleware),
  `login_throttle.rs` (the two asymmetric scopes), `signing.rs` (the HMAC media
  URLs, which are a credential of their own), `boot.rs` (the refusal that decides
  whether a socket opens at all), `routes/auth.rs`, and `manifest.rs` — the last
  because the router is built _from_ it and the auth-matrix test iterates the same
  array, so a route whose `RouteAuth` is wrong fails a test rather than shipping.
- **The browser.** `packages/web/src/components/login-overlay.tsx`,
  `packages/web/src/setup/setup-overlay.tsx` (with `setup/setup-steps.ts`, which
  decides the step the wizard opens on), and
  `packages/web/src/settings/account-panel.tsx`.
- **The e2e harness** — `packages/e2e/src/harness/server.ts`,
  `packages/e2e/src/fixtures.ts`, `packages/e2e/src/fidelity/capture.ts` and
  `packages/e2e/src/screenshots/capture.ts`, all four of which sign in. This is the
  one only unit-tests-plus-e2e catches.
