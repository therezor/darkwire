/**
 * Adding and editing a provider, in a browser, against the real stack.
 *
 * The unit suites already prove the pieces: the patch builders round-trip, the
 * key field's rules are pure functions, the routes accept what the screens
 * send. What only a browser can show is that they are wired to each other —
 * that one press on the editor moves *both* halves of an endpoint, the
 * connection into `config.json` and the credential into the vault, on two
 * different routes in the order the vault requires. Before they were merged,
 * "they both work" was two separate facts.
 *
 * Every assertion here is on **durable** state read back through the API: the
 * settings tree, and the `credentialsPresent` boolean that is the only thing
 * the vault ever says out loud. Nothing waits on the fetch-models line — this
 * harness has no `testProvider` at all, deliberately, since a test suite must
 * not dial out — which makes that absence the useful thing to assert around:
 * the endpoint is still created, and the save still lands.
 */

import type { Page } from '@playwright/test';

import { expect, test } from '../src/fixtures.js';
import { INSTANCE_ID } from '../src/harness/server.js';

interface SettingsView {
  readonly config: {
    readonly providers: Record<
      string,
      {
        readonly label?: string;
        readonly apiBase?: string;
        readonly enabled?: boolean;
      }
    >;
  };
  readonly credentialsPresent: Record<string, boolean>;
}

const settingsOf = async (app: Page, url: string): Promise<SettingsView> => {
  const response = await app.request.get(`${url}/api/settings`);
  return (await response.json()) as SettingsView;
};

/**
 * One endpoint's card.
 *
 * Scoped to the Providers list rather than swept off the page: the shell has
 * lists of its own — the sidebar's sessions, any open menu — and a bare
 * `getByRole('listitem')` picks them up too.
 */
const providerRow = (app: Page, name: string) =>
  app
    .getByRole('list', { name: 'Providers' })
    .getByRole('listitem')
    .filter({ hasText: name });

/** One endpoint, so a spec that edits has something to open. */
async function seedOllama(
  app: Page,
  url: string,
  withKey = false,
): Promise<void> {
  await app.request.patch(`${url}/api/settings`, {
    data: { providers: { ollama: { type: 'ollama' } } },
  });
  if (withKey) {
    await app.request.put(`${url}/api/settings/credentials`, {
      data: { namespace: 'providers', key: 'ollama', value: 'lan-token' },
    });
  }
}

test.describe('providers', () => {
  test('creating one asks for the type on the form that edits it', async ({
    app,
    harness,
  }) => {
    // The same shape as "New agent": one form for create and edit, with the one
    // question the editor cannot ask afterwards — the type is fixed for the
    // life of an instance — asked here and only here.
    await app.goto(`${harness.url}/settings?panel=providers`);

    await app.getByRole('link', { name: 'New provider' }).click();
    await app.getByRole('combobox', { name: 'Type' }).click();
    await app.getByRole('option', { name: 'Ollama' }).click();
    await app.getByLabel('Name').fill('GPU box');

    // Nothing has been written yet — the dialog this replaced had already
    // created the endpoint by now.
    expect(
      (await settingsOf(app, harness.url)).config.providers.ollama,
    ).toBeUndefined();

    await app.getByRole('button', { name: 'Save changes' }).click();

    // Landed in the editor, on the endpoint that was just made.
    await expect(app).toHaveURL(/\/settings\/providers\/ollama$/u);
    await expect(app.getByRole('heading', { level: 1 })).toHaveText('GPU box');

    await expect
      .poll(
        async () =>
          (await settingsOf(app, harness.url)).config.providers.ollama?.label,
      )
      .toBe('GPU box');
  });

  test('one press on the editor saves the connection and the key together', async ({
    app,
    harness,
  }) => {
    await seedOllama(app, harness.url);
    await app.goto(`${harness.url}/settings/providers/ollama`);

    await app.getByLabel('Name').fill('Workstation');
    await app.getByLabel('API base').fill('http://gpu.lan:11434/v1');
    await app.getByLabel('API token (optional)').fill('lan-token');
    await app.getByRole('button', { name: 'Save changes' }).click();

    await expect
      .poll(async () => await settingsOf(app, harness.url))
      .toMatchObject({
        config: {
          providers: {
            ollama: {
              label: 'Workstation',
              apiBase: 'http://gpu.lan:11434/v1',
            },
          },
        },
        credentialsPresent: { ollama: true },
      });
  });

  test('is saved even though the check cannot reach anything', async ({
    app,
    harness,
  }) => {
    // The rule the whole design turns on: an endpoint that is not answering yet
    // is the ordinary case on this screen. Refusing to store it would break the
    // screen for exactly the situation it exists to fix.
    await seedOllama(app, harness.url);
    await app.goto(`${harness.url}/settings/providers/ollama`);

    await app.getByLabel('API base').fill('http://127.0.0.1:1/v1');
    await app.getByRole('button', { name: 'Save changes' }).click();

    await expect
      .poll(
        async () =>
          (await settingsOf(app, harness.url)).config.providers.ollama?.apiBase,
      )
      .toBe('http://127.0.0.1:1/v1');
  });

  test('clearing the key field and saving removes the stored key', async ({
    app,
    harness,
  }) => {
    await seedOllama(app, harness.url, true);
    await app.goto(`${harness.url}/settings/providers/ollama`);

    // The field is not blank for an endpoint that has a key — that is what lets
    // clearing it mean something — so this is a real erasure, not a no-op.
    await app.getByLabel('API token (optional)').clear();
    await app.getByRole('button', { name: 'Save changes' }).click();

    await expect
      .poll(
        async () =>
          (await settingsOf(app, harness.url)).credentialsPresent.ollama ??
          false,
      )
      .toBe(false);
  });

  test('a rename leaves a key that was never typed at alone', async ({
    app,
    harness,
  }) => {
    // The accident the placeholder exists to prevent, and one only a real round
    // trip can rule out: the field is filled from a *boolean*, and a save that
    // never went near it must not reach the vault.
    await seedOllama(app, harness.url, true);
    await app.goto(`${harness.url}/settings/providers/ollama`);

    await app.getByLabel('Name').fill('Workstation');
    await app.getByRole('button', { name: 'Save changes' }).click();

    await expect
      .poll(async () => await settingsOf(app, harness.url))
      .toMatchObject({
        config: { providers: { ollama: { label: 'Workstation' } } },
        credentialsPresent: { ollama: true },
      });
  });

  test('the list switches one off without opening it', async ({
    app,
    harness,
  }) => {
    await seedOllama(app, harness.url);
    await app.goto(`${harness.url}/settings?panel=providers`);

    await expect(providerRow(app, 'Ollama')).toContainText('Enabled');

    await app.getByRole('button', { name: 'Actions for Ollama' }).click();
    await app.getByRole('menuitem', { name: 'Disable' }).click();

    await expect
      .poll(
        async () =>
          (await settingsOf(app, harness.url)).config.providers.ollama?.enabled,
      )
      .toBe(false);
    // And the list says so rather than dropping the row.
    await expect(providerRow(app, 'Ollama')).toContainText('Disabled');
  });

  test('deleting asks, takes the key with it, and returns to the list', async ({
    app,
    harness,
  }) => {
    await seedOllama(app, harness.url, true);
    await app.goto(`${harness.url}/settings/providers/ollama`);

    await app.getByRole('button', { name: 'Actions for Ollama' }).click();
    await app.getByRole('menuitem', { name: 'Delete this provider' }).click();
    await app
      .getByRole('dialog')
      .getByRole('button', { name: 'Delete' })
      .click();

    await expect(app).toHaveURL(/\/settings/u);
    await expect
      .poll(async () => {
        const view = await settingsOf(app, harness.url);
        return {
          // Everything but the harness's own endpoint, which every install the
          // suite starts has and which this test did not make: the server is a
          // real process now, and a real process needs a model to answer with.
          ids: Object.keys(view.config.providers).filter(
            (id) => id !== INSTANCE_ID,
          ),
          key: view.credentialsPresent.ollama ?? false,
        };
      })
      // The config entry and the vault entry: an endpoint is both, and leaving
      // the credential behind would hand it to whatever reuses the id.
      .toEqual({ ids: [], key: false });
  });

  test('fetching a catalogue is per endpoint, not for all of them at once', async ({
    app,
    harness,
  }) => {
    await seedOllama(app, harness.url);

    // Gone from the list: one closed laptop made the all-at-once version look
    // broken, and fetching a catalogue is a thing you do *to* an endpoint.
    await app.goto(`${harness.url}/settings?panel=providers`);
    await expect(
      app.getByRole('button', { name: /Refresh model lists/ }),
    ).toHaveCount(0);

    // And present on the one endpoint it belongs to, where it is also the
    // connection test — one button, one round trip.
    await app.goto(`${harness.url}/settings/providers/ollama`);
    await expect(
      app.getByRole('button', { name: 'Fetch models' }),
    ).toBeVisible();
    await expect(
      app.getByRole('button', { name: 'Test connection' }),
    ).toHaveCount(0);
  });
});
