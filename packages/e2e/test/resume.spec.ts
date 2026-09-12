/**
 * A tab that reloads while a turn is in flight.
 *
 * This is the assertion the replay buffer exists for, and the one no unit test
 * can make: storage holds completed messages, so a turn that has not finished
 * is by definition absent from it. The only thing that can put a half-written
 * turn back on screen is the ring, reached through `session.resume { lastSeq }`
 * on a socket that has just been opened by a *different* page load.
 *
 * `e2e_wait` is what makes "in flight" a state the test can stand still in. A
 * reload timed against a streaming answer would be a race between the browser
 * and the model, and the loser would be whoever was reading the failure.
 */

import { expect, test } from '../src/fixtures.js';

test('a reload rebuilds an in-flight turn from the replay buffer', async ({
  app,
}) => {
  await app.getByRole('textbox', { name: 'Message' }).fill('wait for me');
  await app.getByRole('button', { name: 'Send' }).click();

  const card = app.getByRole('region', { name: 'Tool call: e2e_wait' });
  await expect(card).toBeVisible();
  await expect(card.getByLabel('Running')).toBeVisible();

  // The URL caught up with the session the server minted, which is what makes
  // the reload land on the same session rather than on a fresh one.
  await expect(app).toHaveURL(/session=/);

  await app.reload();

  // Rebuilt: the user's message, the tool card, and the fact that a turn is
  // still running — none of which is in the database yet.
  await expect(
    app.getByTestId('transcript').getByText('wait for me'),
  ).toBeVisible();
  const rebuilt = app.getByRole('region', { name: 'Tool call: e2e_wait' });
  await expect(rebuilt).toBeVisible();
  await expect(rebuilt.getByLabel('Running')).toBeVisible();
  await expect(
    app.getByRole('button', { name: 'Stop the current turn' }),
  ).toBeVisible();

  // And the rebuilt page can still drive the turn it did not start.
  await app.getByRole('button', { name: 'Stop the current turn' }).click();
  await expect(app.getByRole('button', { name: 'Send' })).toBeVisible();
});

/**
 * The reload in the status menu, which is two reloads: the server's settings
 * and then the page.
 *
 * Here rather than in a component test because the join is the part that can
 * break — `api.reloadSettings()` names a URL, the manifest names a URL, and
 * nothing but a real request compares them. A stub answers whatever it is
 * asked for.
 *
 * What is asserted is durable on both sides: the response the server actually
 * sent, and an app that is usable after the navigation. "Reloading" is not a
 * state this waits for — it lasts exactly as long as one fetch.
 */
test('reloads the server from the status indicator, then the page', async ({
  app,
}) => {
  const reloaded = app.waitForResponse((response) =>
    response.url().endsWith('/api/settings/reload'),
  );

  await app
    .getByRole('button', { name: /Connected|Connecting|Reconnecting|Offline/ })
    .click();
  await app.getByRole('menuitem', { name: 'Reload app' }).click();

  expect((await reloaded).status()).toBe(200);

  // The page came back and the socket is up again, which is what makes the
  // press useful rather than just eventful.
  await expect(app.getByRole('textbox', { name: 'Message' })).toBeVisible();
  await expect(app.getByRole('status')).toHaveText('Connected');
});

test.describe('a completed session', () => {
  test.use({
    harnessOptions: {
      sessions: [
        {
          key: 'seeded',
          title: 'A session from before',
          turns: [
            'What did we decide?',
            'To ship the gate before the feature.',
          ],
        },
      ],
    },
  });

  test('is fetched from storage rather than replayed', async ({
    app,
    harness,
  }) => {
    await app.goto(`${harness.url}/?session=seeded`);

    await expect(
      app.getByTestId('transcript').getByText('What did we decide?'),
    ).toBeVisible();
    await expect(
      app
        .getByTestId('transcript')
        .getByText('To ship the gate before the feature.'),
    ).toBeVisible();
    // Nothing is running, so the composer is in its resting state.
    await expect(app.getByRole('button', { name: 'Send' })).toBeVisible();
  });
});
