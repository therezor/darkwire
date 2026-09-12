/**
 * The provider editor, driven through the real router.
 *
 * Three things decide whether this screen is right, and none of them is a
 * rendering question:
 *
 *  - **One press moves both halves of an endpoint.** Its connection is config
 *    and its key is a vault entry, on two routes with two different rules, and
 *    from the operator's side they are one Save. The assertion is on what went
 *    over the wire and in what order.
 *  - **A key reaches the vault and nothing else.** It goes out once, as a `PUT`,
 *    and appears in no other request and nowhere in the DOM afterwards.
 *  - **Fetching the catalogue checks the connection being edited**, not the one
 *    on disk: the URL in the box, the key about to be saved, and the headers the
 *    instance already carries. Sending anything else reports on an endpoint no
 *    turn would ever make.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { ConfigSchema, type ConfigPatch } from '@ghostwire/protocol';

import { Providers } from '@/app/providers.js';
import { createAppRouter } from '@/app/router.js';
import {
  stubApi,
  testQueryClient,
  type RecordedRequest,
  type StubRoute,
} from '@testkit/render.js';
import { STATUS } from '@testkit/fixtures.js';
import { KEY_PLACEHOLDER } from '@/settings/provider-form.js';

const CONFIG = ConfigSchema.parse({
  agents: { defaults: { model: 'llama3', provider: 'ollama' } },
  providers: {
    ollama: {
      type: 'ollama',
      apiBase: 'http://127.0.0.1:11434/v1',
      models: ['llama3'],
      extraHeaders: { 'X-Title': 'GhostAI' },
    },
    openai: { type: 'openai' },
  },
});

const SETTINGS = {
  config: CONFIG,
  credentialsPresent: { ollama: false, openai: true },
};

const PROVIDERS = {
  types: [],
  instances: [
    {
      id: 'ollama',
      type: 'ollama',
      displayName: 'Ollama',
      apiBase: 'http://127.0.0.1:11434/v1',
      isLocal: true,
      isGateway: false,
      isOAuth: false,
      enabled: true,
      supportsModelListing: true,
      credentialsPresent: false,
    },
    {
      id: 'openai',
      type: 'openai',
      displayName: 'OpenAI',
      apiBase: 'https://api.openai.com/v1',
      isLocal: false,
      isGateway: false,
      isOAuth: false,
      envKey: 'OPENAI_API_KEY',
      enabled: true,
      supportsModelListing: true,
      credentialsPresent: true,
    },
  ],
};

const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
  '/api/setup': [200, { required: false }],
  '/api/status': [200, { ...STATUS, model: 'llama3', toolCount: 0 }],
  '/api/sessions': [200, { sessions: [], total: 0 }],
  '/api/notifications': [200, { notifications: [], unreadCount: 0, total: 0 }],
};

function mount(
  instanceId = 'ollama',
  overrides: Record<string, StubRoute> = {},
): {
  readonly user: ReturnType<typeof userEvent.setup>;
  readonly calls: RecordedRequest[];
  readonly router: ReturnType<typeof createAppRouter>;
} {
  const calls = stubApi({
    ...SHELL_ROUTES,
    '/api/settings': [200, SETTINGS],
    'PATCH /api/settings': [200, SETTINGS],
    'PUT /api/settings/credentials': [204, null],
    '/api/providers': [200, PROVIDERS],
    '/api/models': [200, { models: [], errors: {} }],
    '/api/tools': [200, { tools: [] }],
    ...overrides,
  });

  const user = userEvent.setup();
  const router = createAppRouter();
  router.update({
    history: createMemoryHistory({
      initialEntries: [`/settings/providers/${instanceId}`],
    }),
  });
  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );

  return { user, calls, router };
}

const patchesOf = (calls: readonly RecordedRequest[]): ConfigPatch[] =>
  calls
    .filter((call) => call.method === 'PATCH')
    .map((call) => call.body as ConfigPatch);

const probesOf = (calls: readonly RecordedRequest[]): RecordedRequest[] =>
  calls.filter((call) => call.path === '/api/providers/test');

describe('the provider editor', () => {
  it('opens on what the endpoint currently is', async () => {
    mount();

    expect(await screen.findByLabelText('API base')).toHaveValue(
      'http://127.0.0.1:11434/v1',
    );
    expect(screen.getByLabelText('Extra models')).toHaveValue('llama3');
    // The type is a fact, not a control: the vault entry is keyed to this id,
    // so an endpoint that could change protocol would be a key handed to a
    // stranger.
    expect(
      screen.queryByRole('combobox', { name: 'Type' }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(
      'Ollama',
    );
  });

  it('says so for a link to an endpoint that is not there', async () => {
    mount('gone');
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'There is no provider called “gone”',
    );
  });

  it('saves the connection and the key behind a single press', async () => {
    const { user, calls } = mount();

    const apiBase = await screen.findByLabelText('API base');
    await user.clear(apiBase);
    await user.type(apiBase, 'http://elsewhere/v1');
    await user.type(screen.getByLabelText('API token (optional)'), 'lan-token');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(calls.filter((call) => call.method === 'PUT')).toHaveLength(1);
    });
    expect(patchesOf(calls)[0]?.providers).toEqual({
      // No `type`: an endpoint cannot change protocol by being edited.
      ollama: {
        label: '',
        apiBase: 'http://elsewhere/v1',
        models: ['llama3'],
        enabled: true,
      },
    });
    // After the patch, because the vault is keyed by instance id.
    expect(calls.find((call) => call.method === 'PUT')?.body).toEqual({
      namespace: 'providers',
      key: 'ollama',
      value: 'lan-token',
    });
  });

  it('sends a key to the vault and keeps it out of everything else', async () => {
    const secret = 'sk-test-abc123';
    const { user, calls } = mount('openai');

    const field = await screen.findByLabelText('API key');
    // A stored key shows the placeholder, which is a constant and not a
    // credential — the client is never sent one. Masked either way.
    expect(field).toHaveValue(KEY_PLACEHOLDER);
    expect(field).toHaveAttribute('type', 'password');

    await user.clear(field);
    await user.type(field, secret);
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(calls.filter((call) => call.method === 'PUT')).toHaveLength(1);
    });

    // The one request that may carry it is the one that did. Nothing else —
    // not the settings patch, not the check, not a refetch — has it.
    const elsewhere = calls
      .filter((call) => call.method !== 'PUT')
      .map((call) => JSON.stringify(call.body ?? ''));
    expect(elsewhere.some((body) => body.includes(secret))).toBe(false);
    expect(field).toHaveValue(KEY_PLACEHOLDER);
    expect(document.body.textContent).not.toContain(secret);
  });

  it('leaves an untouched key alone, so a rename cannot delete one', async () => {
    // The accident the placeholder exists to prevent: the field is not blank
    // for an endpoint that has a key, so a save that never went near it writes
    // nothing to the vault at all.
    const { user, calls } = mount('openai');

    await user.type(await screen.findByLabelText('Name'), 'Work account');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    expect(calls.filter((call) => call.method === 'PUT')).toHaveLength(0);
  });

  it('removes a key by clearing the field', async () => {
    const { user, calls } = mount('openai');

    await user.clear(await screen.findByLabelText('API key'));
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(calls.filter((call) => call.method === 'PUT')).toHaveLength(1);
    });
    expect(calls.find((call) => call.method === 'PUT')?.body).toEqual({
      namespace: 'providers',
      key: 'openai',
      value: null,
    });
  });

  it('writes nothing to the vault for a keyless endpoint', async () => {
    // An empty field on an endpoint with no key is its resting state, not an
    // instruction to delete anything.
    const { user, calls } = mount();

    await user.type(await screen.findByLabelText('Name'), 'GPU box');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    expect(calls.filter((call) => call.method === 'PUT')).toHaveLength(0);
  });

  it('fetches the catalogue, which is also the connection test', async () => {
    // One button, one round trip. There used to be two that made the same
    // request: `GET /models` answers "does this respond" and "what is on it" at
    // the same time, so asking them separately was asking twice.
    const { user, calls } = mount('openai', {
      'POST /api/providers/test': [
        200,
        { ok: true, models: ['gpt-5', 'gpt-5-mini'] },
      ],
    });

    await user.click(
      await screen.findByRole('button', { name: 'Fetch models' }),
    );

    expect(
      await screen.findByText('Reachable — 2 models listed.'),
    ).toBeInTheDocument();
    expect(screen.getByText('gpt-5-mini')).toBeInTheDocument();
    // Fetching is not saving.
    expect(patchesOf(calls)).toHaveLength(0);
    expect(calls.filter((call) => call.method === 'PUT')).toHaveLength(0);
  });

  it('reports an unreachable endpoint as the same action failing', async () => {
    const { user } = mount('openai', {
      'POST /api/providers/test': [
        200,
        { ok: false, models: [], reason: 'auth', message: 'rejected' },
      ],
    });

    await user.click(
      await screen.findByRole('button', { name: 'Fetch models' }),
    );
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'the key was rejected',
    );
  });

  it('fetches against the connection being edited, not the one on disk', async () => {
    const { user, calls } = mount('ollama', {
      'POST /api/providers/test': [200, { ok: true, models: [] }],
    });

    const apiBase = await screen.findByLabelText('API base');
    await user.clear(apiBase);
    await user.type(apiBase, 'http://typed-just-now/v1');
    await user.type(screen.getByLabelText('API token (optional)'), 'lan-token');
    await user.click(screen.getByRole('button', { name: 'Fetch models' }));

    await waitFor(() => {
      expect(probesOf(calls)).toHaveLength(1);
    });
    expect(probesOf(calls)[0]?.body).toEqual({
      type: 'ollama',
      apiBase: 'http://typed-just-now/v1',
      // Sending `{}` here checked a gateway without the headers a turn sends
      // it, which is a different endpoint wearing the same URL.
      extraHeaders: { 'X-Title': 'GhostAI' },
      apiKey: 'lan-token',
      instanceId: 'ollama',
    });
  });

  it('fetches with no key once the field has been cleared', async () => {
    // Falling back to the stored key would report a working endpoint using the
    // very credential the save is about to delete.
    const { user, calls } = mount('openai', {
      'POST /api/providers/test': [200, { ok: true, models: [] }],
    });

    await user.clear(await screen.findByLabelText('API key'));
    await user.click(screen.getByRole('button', { name: 'Fetch models' }));

    await waitFor(() => {
      expect(probesOf(calls)).toHaveLength(1);
    });
    expect(probesOf(calls)[0]?.body).toMatchObject({ apiKey: '' });
  });

  it('checks after saving, and warns without undoing the save', async () => {
    const { user, calls } = mount('ollama', {
      'POST /api/providers/test': [
        200,
        {
          ok: false,
          models: [],
          reason: 'transport',
          message: 'nothing is listening there',
        },
      ],
    });

    await user.type(await screen.findByLabelText('Name'), 'GPU box');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    // The write went out, and it went out first.
    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    await waitFor(() => {
      expect(probesOf(calls)).toHaveLength(1);
    });
    // No key: the check reads what the vault now holds, which is the only way
    // to re-check a credential this page has never been given.
    expect(probesOf(calls)[0]?.body).toEqual({
      type: 'ollama',
      apiBase: 'http://127.0.0.1:11434/v1',
      extraHeaders: { 'X-Title': 'GhostAI' },
      instanceId: 'ollama',
    });
    expect(await screen.findByRole('alert')).toHaveTextContent('Saved — but');
  });

  it('deletes after asking, and returns to the list', async () => {
    const { user, calls, router } = mount();

    await user.click(
      await screen.findByRole('button', { name: 'Actions for Ollama' }),
    );
    await user.click(
      await screen.findByRole('menuitem', { name: 'Delete this provider' }),
    );
    expect(await screen.findByRole('dialog')).toHaveTextContent(
      'its saved key is deleted with it',
    );

    await user.click(screen.getByRole('button', { name: 'Delete' }));

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    expect(patchesOf(calls)[0]?.providers).toEqual({ ollama: null });
    // On success, not on the press: the list reads the settings tree, and
    // leaving early lands it on one that still holds what is on its way out.
    await waitFor(() => {
      expect(router.state.location.pathname).toBe('/settings');
    });
  });
});
