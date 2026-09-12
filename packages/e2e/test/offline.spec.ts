/**
 * The app renders with no network access.
 *
 * `self-contained.test.ts` in `@ghostwire/web` already fails on any external
 * origin appearing in the shipped source, which is the cheap half of this and
 * runs on every commit. The expensive half is the one that cannot be read off
 * the source at all: a font, an icon set or a highlighter theme that a bundler
 * quietly left as a runtime fetch shows up nowhere in a grep and everywhere in
 * an air-gapped install.
 *
 * So this blocks every request that is not the server's own origin, and then
 * uses the app. A self-hosted, privacy-first product that phones a CDN leaks
 * every user's IP to a third party on page load; one that *needs* the CDN
 * simply does not run behind a firewall.
 */

import { expect, test } from '../src/fixtures.js';

test.describe('with every foreign origin blocked', () => {
  test('the whole app still works', async ({ page, harness }) => {
    const blocked: string[] = [];

    await page.route('**/*', async (route) => {
      const url = new URL(route.request().url());
      if (url.origin === new URL(harness.url).origin) {
        await route.continue();
        return;
      }
      blocked.push(url.href);
      await route.abort('blockedbyclient');
    });

    const response = await page.request.post(`${harness.url}/api/auth/login`, {
      data: { username: 'ghost', password: 'e2e-password' },
    });
    expect(response.ok()).toBe(true);

    await page.goto(harness.url);
    await expect(
      page.getByRole('complementary', { name: 'Sidebar' }),
    ).toBeVisible();

    // Typography is the usual offender: a webfont served from a CDN falls back
    // silently, so the assertion is that the *self-hosted* family is the one in
    // use rather than a system stack standing in for it.
    const family = await page.evaluate(
      () => getComputedStyle(document.body).fontFamily,
    );
    expect(family).toContain('Inter');
    const loaded = await page.evaluate(() =>
      document.fonts.check('1rem Inter'),
    );
    expect(
      loaded,
      'the bundled Inter should have loaded from this origin',
    ).toBe(true);

    // A full turn, including the syntax highlighter — 38 lazy chunks that are
    // all local, and would be the second thing to reach for a CDN.
    await page
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await page.getByRole('button', { name: 'Send' }).click();
    await expect(
      page.getByTestId('transcript').locator('pre code'),
    ).toContainText('const note =');

    // Every other screen, because a route that fetches an icon font on mount
    // would only fail on the screen that mounts it.
    for (const path of ['/files', '/notifications', '/settings', '/tokens']) {
      await page.goto(`${harness.url}${path}`);
      await expect(
        page.getByRole('complementary', { name: 'Sidebar' }),
      ).toBeVisible();
    }

    expect(
      blocked,
      'nothing should have been requested from another origin',
    ).toEqual([]);
  });
});
