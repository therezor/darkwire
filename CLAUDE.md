# Working in this repo

## Write plain English, and never use an em dash

Applies to code comments, doc pages, commit messages, test names and what you say
back to the user.

- **No em dashes.** Not `—`, not `-` standing in for one. Use a full stop, a comma,
  a colon, or brackets.
- **Say it once.** No restating the request, no summary of what you just wrote.
- **Short words and short sentences.** Split anything past about 25 words.
- **Cut hedging.** Drop "just", "simply", "basically", "essentially", "probably".
- **Comments are minimal.** Say why, not what, and only where the pattern is not
  standard. The code already says what it does.

Older prose in this repo is full of em dashes. Fix the lines you are already
editing and leave the rest alone.

## Run what CI runs before you call a task finished

Every task, before you write the summary. `pnpm check` is the check job, and
`.github/workflows/ci.yml` is the other three. When this and
[Development](docs/development.md#the-gate) disagree, that page wins.

```bash
pnpm check          # typecheck, lint, token gates, format, shellcheck, i18n, protocol, test, build
pnpm test:coverage  # the coverage job

# the e2e and rust jobs: docs/development.md#the-gate
```

Report what you actually ran. If you skipped e2e because the change was server-only,
say so. An unqualified "all tests pass" that meant `pnpm test` is a false claim.

Notes that save a cycle:

- `pnpm format` fixes the `format:check` step of `pnpm check`.
- Filter Rust tests inside the workspace build rather than beside it:
  `cargo nextest run --workspace -E 'package(darkwire-server)'`. A bare `-p` resolves
  features for that package alone and builds a second copy of everything.
- e2e needs both builds, `pnpm build` before `cargo build`.
- The fidelity spec skips without a baseline. `2 skipped` is healthy.
- A visible UI change means `pnpm screenshots`. The images are generated and
  committed, and nothing but a reader catches a stale one.
- `pnpm i18n:check` diffs `web.json` only. `keepRemoved: true`, so it cannot see a
  stale key. `cli.json` is hand-maintained and a missing key there is a compile error.
- Coverage numbers live in `vitest.config.ts` and `scripts/coverage-gate.mjs`. Read
  them there, never copy them here. A new branch in a guard needs a test.
- A green local e2e run is evidence, not proof. CI runs 2 workers on a shared runner.
  Re-run a suspect spec under load:
  `pnpm --filter @darkwire/e2e exec playwright test <spec> --repeat-each=6`

### Never assert a transient state in an e2e test

Assert the durable state a step settles into: the card's `Succeeded`/`Failed` status,
the text in the transcript. Cover the in-between wording in a component test, where
the state holds still (`packages/web/test/chat/approval.test.tsx`). If the only reason
you can see it is that the machine was slow, it does not belong in an `expect`. See
[Development](docs/development.md#never-assert-a-transient-state).

## Rust: rustfmt and clippy own the style

Everything under `crates/` is Rust, one crate per former TypeScript package.
`rustfmt.toml`, `clippy.toml` and `[workspace.lints]` hold the rules; full reasoning
in [Development](docs/development.md#rust-conventions).

- `#![forbid(unsafe_code)]` in every crate, no exceptions.
- `pedantic` warns locally, errors in CI. A new `#[allow]` needs a one-line why.
  Product names that trip `doc_markdown` go in `doc-valid-idents`, not behind an allow.
- Errors are values: `WireError { kind, message, retryable, details }`, `thiserror`
  below the binary, `anyhow` only in `crates/cli/src/main.rs`. Never branch on a
  message substring.
- One cancellation mechanism: a `CancellationToken` threaded from transport to child
  process. No running flags.
- Injected `Clock` and `RandomSource`; `rand::rng` and `SystemTime::now` are
  clippy-denied. No shell, ever: `Command::new(argv[0])`.
- The wire is the contract. `packages/protocol` (zod) is the source of truth because
  the browser parses it; `crates/protocol` mirrors it and a drift test compares the two
  JSON Schemas. `#[serde(default)]` on every field zod `.default()`s client-to-server.
- Port reasoning, not commentary. A comment about Node, tsup, zod inference,
  `node:sqlite`, pino, `AbortSignal`, async generators or a dead TypeScript identifier
  is deleted, not translated. No "ported from `x.ts`" breadcrumbs.
- One version: the root `Cargo.toml` and root `package.json` must agree, and
  `crates/cli/tests/version.rs` fails if they do not.

## TypeScript: the style guide is Google's, and the linter owns it

`eslint.config.js` and `.prettierrc.json` hold the checkable parts of the
[Google TypeScript Style Guide](https://google.github.io/styleguide/tsguide.html), so
`pnpm lint` and `pnpm format:check` are the guide as far as a change is concerned.

- **80 columns, not 100.** The mass reformat is in `.git-blame-ignore-revs`; wire it
  up once per clone with `git config blame.ignoreRevsFile .git-blame-ignore-revs`.
- **No leading or trailing underscores**, including on unused parameters. There is no
  `argsIgnorePattern`. Delete the parameter, or give it an ordinary name.
- **`private`, never `#ident`.** A `private` field is a real enumerable own property,
  so a deep-equality assertion sees it. A private field cannot share a name with a
  public getter: rename the private side (`private itemsByKey` + `get items()`).

Deliberate deviations from the guide, all on purpose:

- PascalCase values are allowed for React components and contexts (JSX reads a
  lowercase identifier as an intrinsic element) and for zod schemas.
- `export default` is allowed in `*.config.ts`, where the tool reading the file
  dictates the shape.
- Object and type _properties_ are exempt from naming rules. They are wire formats,
  header names, route keys, CSS custom properties and i18n keys.
- `ignoreRestSiblings` is on, so `const {password, ...rest} = user` works.
- Tests may assert object literals (`{matches: true} as MediaQueryListEvent`). Only
  that case, only under `test/`, and product code still annotates.

## Where a test goes

**Nothing under `src/` is a test.** Coverage measures `src/` only on both sides.

- **TypeScript**: `packages/<pkg>/test/` mirroring `src/`. The test for
  `packages/web/src/chat/markdown/blocks.ts` is
  `packages/web/test/chat/markdown/blocks.test.ts`.
- **Rust**: `crates/<crate>/tests/` mirroring `src/`. An inline `#[cfg(test)]` module
  is for a private helper with no public path, and carries a line saying why.
- **One test binary per crate.** `autotests = false`, and `tests/main.rs` lists every
  file as a `mod`. A new test file is a new `mod` line there, never a new binary: each
  binary links the whole dependency tree.

Tests reach source through an alias, never a relative path: `#src/…` in `protocol` and
`i18n` (a package.json `imports` entry), `@/…` in web.

- `test/` is its own TypeScript project (`packages/<pkg>/test/tsconfig.json`,
  referenced from the root `tsconfig.json`). A new package needs both references added.
  Web is the exception: its own `tsconfig.json` checks its tests, because `noEmit`
  makes it invalid as a reference target (TS6310).
- A test does not share a program with `src/`, so a module augmentation has to be
  imported by name: `import type {} from '…'`.

Testkits are never measured, and live where the toolchain can export them:

- **TypeScript**: `test/testkit/`, outside `src/`, reached by alias
  (`@testkit/render.js` in web).
- **Rust**: `src/testkit.rs` behind a `testkit` cargo feature, because Cargo cannot
  export an unbuilt directory. Consumers name `features = ["testkit"]`.

## Auth changes touch more places than they look like they do

Four places that do not import each other. See
[Development](docs/development.md#areas-that-touch-more-than-they-look-like-they-do).

- **The wire**: `packages/protocol/src/rest.ts` (every exported `*Schema` also goes in
  `schemas.ts`, enforced by a test) and its mirror `crates/protocol/src/rest.rs`.
- **The server**: `crates/server/src/` `auth_store.rs`, `auth.rs`, `login_throttle.rs`,
  `signing.rs`, `boot.rs`, `routes/auth.rs` and `manifest.rs`, the last because the
  router is built from it and the auth-matrix test iterates the same array.
- **The browser**: `components/login-overlay.tsx`, `setup/setup-overlay.tsx` with
  `setup/setup-steps.ts`, and `settings/account-panel.tsx`, all under `packages/web/src/`.
- **The e2e harness**: `packages/e2e/src/` `harness/server.ts`, `fixtures.ts`,
  `fidelity/capture.ts` and `screenshots/capture.ts`. Only e2e catches this one.
