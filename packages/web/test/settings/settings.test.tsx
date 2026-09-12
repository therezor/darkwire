/**
 * Settings, driven through the real router.
 *
 * The two cases that decide whether the step landed are here, and neither is a
 * rendering question:
 *
 *  - **A key saved in the UI reaches the vault and nothing else.** It goes out
 *    once, as a `PUT` to the credentials route, and appears in no other request,
 *    in no response, and nowhere in the DOM afterwards. The runtime re-reads the
 *    vault on every provider build, so that write is the whole of "usable on the
 *    next turn with no restart" from this side of the wire.
 *  - **A panel saves its own section and nothing else.** `ConfigPatch` is a
 *    deep-partial precisely so that saving one panel does not rewrite another's
 *    fields, and the assertion is on what went over the wire rather than on
 *    what the screen says afterwards.
 *
 * The agent cases used to live here. They moved to `agents/agents.test.tsx`
 * with the panel: the model and budget it edited are the default agent's, and
 * they are edited on that agent now.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it } from 'vitest';

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

const CONFIG = ConfigSchema.parse({
  agents: {
    list: { default: { model: 'llama3', provider: 'ollama', maxTokens: 4096 } },
  },
  providers: {
    ollama: {
      type: 'ollama',
      apiBase: 'http://127.0.0.1:11434/v1',
      models: ['llama3'],
    },
    openai: { type: 'openai' },
  },
});

const SETTINGS = {
  config: CONFIG,
  credentialsPresent: { ollama: false, openai: true },
};

/**
 * Both halves of the response: the registry catalogue an endpoint is added
 * from, and the endpoints that have been. They are different lists now — the
 * panel used to render one row per registry entry, and renders one per
 * configured endpoint instead.
 */
const PROVIDERS = {
  types: [
    {
      id: 'ollama',
      displayName: 'Ollama',
      wire: 'openai',
      isLocal: true,
      isGateway: false,
      isOAuth: false,
      defaultApiBase: 'http://127.0.0.1:11434/v1',
      supportsModelListing: true,
    },
    {
      id: 'openai',
      displayName: 'OpenAI',
      wire: 'openai',
      isLocal: false,
      isGateway: false,
      isOAuth: false,
      envKey: 'OPENAI_API_KEY',
      supportsModelListing: true,
    },
  ],
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

const TOOLS = {
  tools: [
    {
      name: 'read_file',
      description: 'Read a file',
      parameters: {},
      risk: 'safe',
      source: 'builtin',
    },
    {
      name: 'exec',
      description: 'Run a command',
      parameters: {},
      risk: 'exec',
      source: 'builtin',
    },
  ],
};

const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
  // Claimed: the setup overlay mounts above the login one and would
  // otherwise be deciding whether to open on an unstubbed request.
  '/api/setup': [200, { required: false }],
  '/api/status': [200, { ...STATUS, model: 'llama3', toolCount: 2 }],
  '/api/sessions': [200, { sessions: [], total: 0 }],
  '/api/notifications': [200, { notifications: [], unreadCount: 0, total: 0 }],
};

function mount(
  path = '/settings',
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
    '/api/models': [
      200,
      { models: [{ id: 'llama3', providerId: 'ollama' }], errors: {} },
    ],
    '/api/tools': [200, TOOLS],
    ...overrides,
  });

  const user = userEvent.setup();
  const router = createAppRouter();
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
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

beforeEach(() => {
  // Nothing to reset here beyond what `test/setup.ts` already does; the stub is
  // installed per mount so each case owns its own routes.
});

describe('the settings screen', () => {
  it('warns when the settings file failed to parse and defaults are in use', async () => {
    mount('/settings', {
      '/api/settings': [200, { ...SETTINGS, loadError: 'Unexpected token }' }],
    });

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'could not be read',
    );
  });
});

/** The kebab on one row, then the item on it. */
async function rowAction(
  user: ReturnType<typeof userEvent.setup>,
  row: string,
  action: string,
): Promise<void> {
  await user.click(
    await screen.findByRole('button', { name: `Actions for ${row}` }),
  );
  await user.click(await screen.findByRole('menuitem', { name: action }));
}

/**
 * The list only.
 *
 * Everything about *one* endpoint — its base URL, its key, its catalogue — is
 * edited on a route of its own now, and is covered in `provider-editor.test.tsx`
 * with the rest of that screen. What belongs here is what the list itself does:
 * what it reports at a glance, and the three acts that need no form.
 */
describe('the providers panel', () => {
  it('reports each endpoint\u2019s key and status in words rather than in colour', async () => {
    mount('/settings?panel=providers');

    const rows = within(
      await screen.findByRole('list', { name: 'Providers' }),
    ).getAllByRole('listitem');
    // One per endpoint. There is no heading row any more — the list is cards.
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent('no key');
    expect(rows[0]).toHaveTextContent('Enabled');
    expect(rows[1]).toHaveTextContent('key saved');
  });

  it('shows a disabled endpoint as disabled rather than hiding it', async () => {
    mount('/settings?panel=providers', {
      '/api/providers': [
        200,
        {
          types: PROVIDERS.types,
          instances: [
            { ...PROVIDERS.instances[0], enabled: false },
            PROVIDERS.instances[1],
          ],
        },
      ],
    });

    const rows = within(
      await screen.findByRole('list', { name: 'Providers' }),
    ).getAllByRole('listitem');
    expect(rows[0]).toHaveTextContent('Disabled');
  });

  it('switches one off from the list, without asking', async () => {
    // Reversible where Delete is not, so it is one press and no dialog \u2014 the
    // same rule the agents list follows.
    const { user, calls } = mount('/settings?panel=providers');

    await rowAction(user, 'Ollama', 'Disable');

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    expect(patchesOf(calls)[0]?.providers).toEqual({
      ollama: { enabled: false },
    });
  });

  it('offers Enable on one that is already off', async () => {
    const { user, calls } = mount('/settings?panel=providers', {
      '/api/providers': [
        200,
        {
          types: PROVIDERS.types,
          instances: [{ ...PROVIDERS.instances[0], enabled: false }],
        },
      ],
    });

    await rowAction(user, 'Ollama', 'Enable');

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    expect(patchesOf(calls)[0]?.providers).toEqual({
      ollama: { enabled: true },
    });
  });

  it('creates an endpoint on the same form that edits one, writing nothing early', async () => {
    // The type is the one question the editor cannot ask afterwards — it is
    // fixed for the life of an instance — so it is the only create-only field.
    const { user, calls, router } = mount('/settings/providers/new');

    await user.click(await screen.findByRole('combobox', { name: 'Type' }));
    await user.click(await screen.findByRole('option', { name: 'Ollama' }));

    await user.type(await screen.findByLabelText('Name'), 'GPU box');
    // Nothing has gone to the wire yet: the dialog this replaced had already
    // created the endpoint by this point.
    expect(patchesOf(calls)).toHaveLength(0);

    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    // A free id: a second Ollama is a second endpoint, not a merge into the
    // first.
    expect(patchesOf(calls)[0]?.providers).toEqual({
      'ollama-2': {
        type: 'ollama',
        label: 'GPU box',
        // The type's own default endpoint, prefilled and visible before Save
        // rather than arriving after it.
        apiBase: 'http://127.0.0.1:11434/v1',
        models: [],
        enabled: true,
      },
    });

    // On success, not on the press: navigating early lands the editor on a
    // settings cache that has never seen the endpoint it was sent to.
    await waitFor(() => {
      expect(router.state.location.pathname).toBe(
        '/settings/providers/ollama-2',
      );
    });
  });

  it('opens the create page from the panel', async () => {
    const { user, router } = mount('/settings?panel=providers');

    await user.click(await screen.findByRole('link', { name: 'New provider' }));

    expect(router.state.location.pathname).toBe('/settings/providers/new');
  });

  it('asks before deleting, because the key goes with it', async () => {
    const { user, calls } = mount('/settings?panel=providers');

    await rowAction(user, 'OpenAI', 'Delete');
    expect(await screen.findByRole('dialog')).toHaveTextContent(
      'its saved key is deleted with it',
    );
    // Asking is not doing.
    expect(patchesOf(calls)).toHaveLength(0);

    await user.click(screen.getByRole('button', { name: 'Delete' }));

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    expect(patchesOf(calls)[0]?.providers).toEqual({ openai: null });
  });

  it('has no way to ask every endpoint at once', async () => {
    // It used to. One closed laptop made the whole action look broken, and
    // fetching a catalogue is a thing you do *to an endpoint* \u2014 so it lives in
    // that endpoint's editor.
    mount('/settings?panel=providers');

    await screen.findByRole('list', { name: 'Providers' });
    expect(
      screen.queryByRole('button', { name: /Refresh model lists/ }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: /Check for models/ }),
    ).not.toBeInTheDocument();
  });
});

describe('the tools panel', () => {
  it('is install-wide settings only — no tool list, no permissions', async () => {
    // Both were here when this screen decided what happened to a tool. The
    // matrix could not say which agent it bound, and the inventory below it was
    // a list you could read but not act on. Both live on the agent now.
    mount('/settings?panel=tools');

    await screen.findByLabelText('Approval timeout (seconds)');

    expect(
      screen.queryByRole('region', { name: 'Registered tools' }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('combobox', { name: /policy/i }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('option', { name: 'Ask first' }),
    ).not.toBeInTheDocument();
  });

  it('says how the install-wide exec switch differs from a per-agent one', async () => {
    mount('/settings?panel=tools');

    expect(
      await screen.findByLabelText('Let agents run commands'),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/removes exec from every agent/),
    ).toBeInTheDocument();
  });

  it('refuses an approval timeout of zero', async () => {
    const { user, calls } = mount('/settings?panel=tools');

    const timeout = await screen.findByLabelText('Approval timeout (seconds)');
    await user.clear(timeout);
    await user.type(timeout, '0');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Must be at least 1',
    );
    expect(patchesOf(calls)).toHaveLength(0);
  });
});

describe('the tab strip', () => {
  it('puts the panel it opens in the URL', async () => {
    // Appearance rather than a panel behind the settings request: the point
    // here is the routing, and this one renders without waiting on the server.
    //
    // This used to be asserted on `Knowledge`, the one panel that rendered a
    // "Phase 5" placeholder instead of a form. That panel is gone, but the
    // behaviour it happened to cover is not — the panel being in the URL is
    // what makes "set your key here" a link rather than a sentence describing
    // four clicks.
    const { user, router } = mount();

    await user.click(await screen.findByRole('tab', { name: 'Appearance' }));

    expect(router.state.location.searchStr).toContain('panel=appearance');
  });
});

/**
 * The Account panel.
 *
 * Not a settings form, and the cases below are the three ways that shows: it
 * posts to the credential route rather than to `PATCH /api/settings`, it will
 * not submit without the current password, and it sends the username only when
 * the username actually changed.
 */
describe('the account panel', () => {
  const ACCOUNT_ROUTES: Record<string, StubRoute> = {
    '/api/auth/me': [
      200,
      { authenticated: true, authEnabled: true, username: 'ghost' },
    ],
    'POST /api/setup/password': [200, { ok: true, expiresAtMs: 99 }],
  };

  it('changes the password without touching the settings tree', async () => {
    const { user, calls } = mount('/settings?panel=account', ACCOUNT_ROUTES);

    await user.type(
      await screen.findByLabelText('Current password'),
      'the old password',
    );
    await user.type(screen.getByLabelText('New password'), 'the new password');
    await user.type(
      screen.getByLabelText('Confirm new password'),
      'the new password',
    );
    await user.click(screen.getByRole('button', { name: 'Change password' }));

    await waitFor(() => {
      expect(
        calls.find((call) => call.path === '/api/setup/password')?.body,
      ).toEqual({
        currentPassword: 'the old password',
        password: 'the new password',
      });
    });
    // The username is absent because it did not change — sending it back
    // unchanged would be a rotation of a credential nobody asked to rotate.
    expect(patchesOf(calls)).toEqual([]);
  });

  it('sends the username when it is the thing that changed', async () => {
    const { user, calls } = mount('/settings?panel=account', ACCOUNT_ROUTES);

    const username = await screen.findByLabelText('Username');
    await waitFor(() => {
      expect(username).toHaveValue('ghost');
    });
    await user.clear(username);
    await user.type(username, 'operator');
    await user.type(
      screen.getByLabelText('Current password'),
      'the old password',
    );
    await user.type(screen.getByLabelText('New password'), 'the new password');
    await user.type(
      screen.getByLabelText('Confirm new password'),
      'the new password',
    );
    await user.click(screen.getByRole('button', { name: 'Change password' }));

    await waitFor(() => {
      expect(
        calls.find((call) => call.path === '/api/setup/password')?.body,
      ).toEqual({
        username: 'operator',
        currentPassword: 'the old password',
        password: 'the new password',
      });
    });
  });

  // A session is not enough to change the credential it was minted from, and
  // the form says so before spending a request finding out.
  it('will not submit without the current password', async () => {
    const { user } = mount('/settings?panel=account', ACCOUNT_ROUTES);

    await user.type(
      await screen.findByLabelText('New password'),
      'the new password',
    );
    await user.type(
      screen.getByLabelText('Confirm new password'),
      'the new password',
    );

    expect(
      screen.getByRole('button', { name: 'Change password' }),
    ).toBeDisabled();
  });

  it('refuses two new passwords that differ, without asking the server', async () => {
    const { user, calls } = mount('/settings?panel=account', ACCOUNT_ROUTES);

    await user.type(
      await screen.findByLabelText('Current password'),
      'the old password',
    );
    await user.type(screen.getByLabelText('New password'), 'the new password');
    await user.type(
      screen.getByLabelText('Confirm new password'),
      'a different one',
    );
    await user.click(screen.getByRole('button', { name: 'Change password' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('do not match');
    expect(calls.some((call) => call.path === '/api/setup/password')).toBe(
      false,
    );
  });

  it('says the current password was wrong rather than reporting a generic failure', async () => {
    const { user } = mount('/settings?panel=account', {
      ...ACCOUNT_ROUTES,
      'POST /api/setup/password': [
        401,
        {
          error: {
            code: 'unauthorized',
            message: 'Incorrect current password',
          },
        },
      ],
    });

    await user.type(
      await screen.findByLabelText('Current password'),
      'not the old one',
    );
    await user.type(screen.getByLabelText('New password'), 'the new password');
    await user.type(
      screen.getByLabelText('Confirm new password'),
      'the new password',
    );
    await user.click(screen.getByRole('button', { name: 'Change password' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'not the current password',
    );
  });
});
