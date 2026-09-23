import { defineConfig } from 'vitest/config';

/**
 * Per-package coverage gates.
 *
 * Two entries, because two packages are all that carry decision logic of their
 * own now: everything the Rust workspace replaced has its bars in
 * `scripts/coverage-gate.mjs` instead. `protocol` is not listed — it is schema
 * declarations exercised by the whole suite, and a ratio over it measures the
 * consumers rather than the package.
 */
const THRESHOLDS: Record<string, { lines: number; branches: number }> = {
  // The token layer is the UI's `security`: an untested branch in the contrast
  // resolver or a gate regex is a rule that reports "clean" without checking.
  // Only the `.ts` half is measured — the components are `.tsx`, and the
  // Playwright run is what covers those.
  web: { lines: 85, branches: 80 },
  // Above the default because the package is small and almost entirely pure
  // decision logic: `locale.ts` is a negotiation with a fallback chain and
  // `format.ts` is a set of `Intl` wrappers with threshold branches. Both are
  // the kind of code where an untested branch is a language silently resolving
  // to the wrong bundle rather than a crash — and there is no I/O here to make
  // the bar expensive to hold.
  i18n: { lines: 90, branches: 90 },
};

export default defineConfig({
  test: {
    // `examples/*` are workspace packages like any other: the hello extension
    // is what proves an out-of-tree extension still speaks the JSON-RPC
    // handshake, and a suite that does not run is a contract nothing holds.
    //
    // `packages/e2e` is the one exclusion. Its specs are Playwright's, and the
    // two runners' `test`/`expect` are different objects — vitest collecting a
    // `.spec.ts` fails with "Playwright Test did not expect test.describe() to
    // be called here", which reads as a broken suite rather than the wrong
    // runner. It has its own command; see the `e2e` job in CI.
    projects: ['packages/*', '!packages/e2e', 'examples/*'],
    passWithNoTests: true,
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json-summary', 'html'],
      // `src` only, which since the tests and testkits moved to `test/` is the
      // whole of the filter. Fixtures and jsdom stubs are not code under test:
      // measuring a no-op `observe()` that exists because jsdom lacks one says
      // nothing, and — because a testkit runs on every test and so scores near
      // 100% — it inflates the ratio of whichever package holds it. Keeping the
      // directory out of `src` is what keeps it out of the denominator.
      include: ['packages/*/src/**/*.ts'],
      // `target` first. The untested-file search walks the repo from the root
      // before it applies `include`, and a Cargo build directory holds millions
      // of files: without this the run hangs in that walk.
      exclude: ['target/**', '**/*.test.ts', '**/index.ts', '**/*.d.ts'],
      thresholds: Object.fromEntries(
        Object.entries(THRESHOLDS).map(([pkg, t]) => [
          `packages/${pkg}/src/**`,
          { ...t, functions: t.lines, statements: t.lines },
        ]),
      ),
    },
  },
});
