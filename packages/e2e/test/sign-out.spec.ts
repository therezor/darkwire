/**
 * Signing out reaches the tabs that are already open.
 *
 * The socket is authenticated once, at the upgrade. Without the server closing
 * it when the login behind it is revoked, a tab left open would keep driving an
 * agent after its owner signed out somewhere else. So the assertion is on the
 * other tab: nothing is done to it, and it still ends up at the login overlay.
 */

import type { Page, WebSocket } from '@playwright/test';

import { PASSWORD } from '../src/harness/server.js';
import { expect, test } from '../src/fixtures.js';

/** The page's next socket that has received a frame from the server. */
function nextLiveSocket(page: Page): Promise<WebSocket> {
  return new Promise((resolve) => {
    page.on('websocket', (socket) => {
      socket.once('framereceived', () => {
        resolve(socket);
      });
    });
  });
}

test('logging out in one tab signs another tab out', async ({
  app,
  harness,
}) => {
  const other = await app.context().newPage();
  const firstSocket = nextLiveSocket(other);
  await other.goto(harness.url);
  await expect(
    other.getByRole('complementary', { name: 'Sidebar' }),
  ).toBeVisible();
  const socket = await firstSocket;
  const closed = socket.waitForEvent('close');

  // The same browser, so the same cookie: this is the first tab signing out.
  const response = await app.request.post(`${harness.url}/api/auth/logout`);
  expect(response.status()).toBe(204);

  await closed;
  await expect(other.getByRole('heading', { name: 'Sign in' })).toBeVisible();

  // Signing back in dials again, on its own.
  const secondSocket = nextLiveSocket(other);
  await other.getByLabel('Password', { exact: true }).fill(PASSWORD);
  await other.getByRole('button', { name: 'Sign in' }).click();
  await expect(other.getByRole('heading', { name: 'Sign in' })).toBeHidden();
  await secondSocket;
});
