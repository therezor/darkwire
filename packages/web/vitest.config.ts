import { fileURLToPath } from 'node:url';

import { defineConfig } from 'vitest/config';

/**
 * Deliberately not `vite.config.ts`: nothing here needs the React
 * plugins, and loading them would put a CSS compile in front of a test that
 * reads `tokens.css` as text. The `@/` alias is duplicated from there for the
 * same reason — it is resolution, not bundling, and both configs need it.
 *
 * `@testkit/` is this config's alone: it points at fixtures the app never
 * imports, so `vite.config.ts` has no business resolving it. It is listed first
 * only for legibility — `@` cannot swallow it, since an alias match has to end
 * at a path separator.
 *
 * `jsdom` everywhere rather than per file. The token gates and the contrast
 * assertion are file readers that do not need a DOM, but they still run on
 * node, so a jsdom environment costs them a few hundred milliseconds and buys
 * one rule instead of a docblock on two thirds of the suite — and a component
 * test that forgot the docblock fails with `document is not defined`, which
 * reads like a bug in the component.
 */
export default defineConfig({
  resolve: {
    alias: {
      '@testkit': fileURLToPath(new URL('./test/testkit', import.meta.url)),
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  test: {
    name: 'web',
    environment: 'jsdom',
    // The default 5s, measured against the slowest component test
    // (`agents.test.tsx > subagents > saves the ref…`), leaves no margin: it
    // runs in ~0.8s bare, ~1.8s under v8 coverage instrumentation, and a CI
    // runner is another ~3x slower again — which is how it timed out at 5111ms
    // in the coverage job while the uninstrumented `check` job passed.
    testTimeout: 15_000,
    include: ['test/**/*.test.{ts,tsx}'],
    setupFiles: ['./test/testkit/setup.ts'],
  },
});
