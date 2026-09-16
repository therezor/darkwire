import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { defineConfig, devices } from '@playwright/test';

import { VIEWPORT } from './src/viewport.js';

/**
 * Two projects, one suite.
 *
 * The colour scheme is a project rather than a parameter inside a handful of
 * specs, so *every* assertion runs twice — once in each theme. That is the only
 * arrangement in which "this component only works in dark" is a failing test
 * rather than something someone notices in review: a light-mode bug that lives
 * in a screen no light-mode spec happened to visit is a light-mode bug that
 * ships. Reviewing only in dark is how a light theme ships broken.
 *
 * There is no `webServer`, and the reason is stronger now than it was. The
 * harness starts `darkwire serve` itself, per test, over a temporary home, on a
 * port the OS picks — see `src/harness/server.ts`. A single shared server would
 * put every spec's settings saves and sessions in one another's way, and the
 * approval matrix in particular is a setting two specs want opposite answers
 * from. Playwright's `webServer` can only offer the shared one.
 *
 * The binary is resolved here as well as in the harness, so a missing build is
 * one sentence before the first test rather than the same sentence inside every
 * one of them. Against the repository root rather than the working directory,
 * because `pnpm --filter` runs this from `packages/e2e` and CI hands over the
 * relative `target/release/darkwire`.
 */

/**
 * The binary under test, checked once so a missing build fails early.
 *
 * `DARKWIRE_BIN` is what CI exports after `cargo build --release`; a laptop that
 * exports nothing gets the debug build. Nothing is thrown here — a config that
 * refused to load would hide the message inside Playwright's own error — the
 * harness raises it per test with the same text.
 */
const ROOT = fileURLToPath(new URL('../..', import.meta.url));
const BIN = resolve(
  ROOT,
  process.env['DARKWIRE_BIN'] ?? 'target/debug/darkwire',
);
if (!existsSync(BIN)) {
  process.stderr.write(
    `No darkwire binary at ${BIN}.\n` +
      'Run `cargo build -p darkwire --features test-hooks`, or set DARKWIRE_BIN.\n',
  );
}
export default defineConfig({
  testDir: './test',
  fullyParallel: true,
  // A `.only` left in a spec silently reduces CI to that one test.
  forbidOnly: process.env['CI'] === 'true',
  retries: process.env['CI'] === 'true' ? 1 : 0,
  reporter: process.env['CI'] === 'true' ? [['github'], ['list']] : [['list']],
  outputDir: './artifacts/output',
  // Raised from 30 s because every test now pays for a process: a temporary
  // home, a spawn, a bind, a login. That is a second or two on a release build
  // and rather more on a debug one, and it is time the assertions never see.
  timeout: 60_000,
  // Raised with the test timeout and for the same reason. Every assertion here
  // waits on a state the app *settles into*, so a longer wait cannot make a
  // wrong test pass — it can only stop a right one failing because five servers
  // and five browsers were competing for the machine when it looked.
  expect: { timeout: 15_000 },
  use: {
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    // Every locator in this suite matches on English text, and the browser's
    // `Accept-Language` is what the pre-paint script resolves from when nobody
    // has chosen yet. Left to the runner, the suite would pass on CI and fail on
    // a laptop set to German — so it is pinned here rather than discovered.
    // Both projects inherit it; a per-project `use` merges over this one.
    locale: 'en-US',
  },
  projects: [
    // The viewport is set *after* the device spread in each project, not once
    // at the top: `devices['Desktop Chrome']` carries a viewport of its own,
    // and a top-level `use.viewport` loses to it. The size matters — the shell
    // has a breakpoint at `md`, and one that straddled it would make "is the
    // sidebar inline or in a drawer" depend on the runner's defaults.
    {
      name: 'dark',
      use: {
        ...devices['Desktop Chrome'],
        colorScheme: 'dark',
        viewport: VIEWPORT,
      },
    },
    {
      name: 'light',
      use: {
        ...devices['Desktop Chrome'],
        colorScheme: 'light',
        viewport: VIEWPORT,
      },
    },
  ],
});
