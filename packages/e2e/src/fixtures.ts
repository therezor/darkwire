/**
 * One server per test, and a page that has already logged in.
 *
 * Two fixtures and one option. `harnessOptions` is how a spec file says what
 * kind of server it wants — `test.use({ harnessOptions: { config: … } })` — and
 * it is a Playwright option rather than a helper call so that the server is
 * built before the page exists rather than in the middle of a test body.
 *
 * `app` logs in through `page.request`, which shares the browser context's
 * cookie jar: the session cookie the route sets lands where the page's own
 * fetches will send it back. Logging in through the overlay instead would put
 * six interactions in front of every unrelated assertion, and the overlay has
 * its own spec for the same reason.
 */

import { test as base, expect, type Page } from '@playwright/test';

import {
  startHarness,
  PASSWORD,
  USERNAME,
  type Harness,
  type HarnessOptions,
} from './harness/server.js';

export interface Fixtures {
  readonly harness: Harness;
  /** A page on the harness's origin, authenticated, with the shell rendered. */
  readonly app: Page;
}

export interface Options {
  readonly harnessOptions: HarnessOptions;
}

export const test = base.extend<Fixtures & Options>({
  harnessOptions: [{}, { option: true }],

  harness: async ({ harnessOptions }, use) => {
    const harness = await startHarness(harnessOptions);
    await use(harness);
    await harness.close();
  },

  app: async ({ page, harness }, use) => {
    const response = await page.request.post(`${harness.url}/api/auth/login`, {
      data: { username: USERNAME, password: PASSWORD },
    });
    expect(response.ok(), 'the harness login should succeed').toBe(true);

    await page.goto(harness.url);
    // The shell, not the route: every screen has it, and waiting for it is what
    // separates "the bundle loaded" from "React mounted".
    await expect(
      page.getByRole('complementary', { name: 'Sidebar' }),
    ).toBeVisible();
    await use(page);
  },
});

export { expect };
export type { Harness };
