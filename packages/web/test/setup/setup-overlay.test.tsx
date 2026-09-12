/**
 * The first-run wizard, driven through the real request stack.
 *
 * Three properties decide whether this landed, and none of them is a rendering
 * question:
 *
 *  - **An unclaimed install shows the wizard and not the login.** Both mount on
 *    a browser with no session, because `/api/auth/me` 401s either way, and the
 *    one that can actually get the user in has to be the one on top.
 *  - **The code is spent for a session, and the password re-issues it.**
 *    `setPassword` revokes every session including the caller's own, so a
 *    wizard that did not re-issue would sign the browser out at step two with
 *    the code it needs to get back in already used.
 *  - **A claimed install never sees it.** The overlay is mounted for every user
 *    on every page, so "does nothing when there is nothing to do" is the case
 *    that runs a million times more often than the other two.
 */

import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { Providers } from '@/app/providers.js';
import {
  stubApi,
  testQueryClient,
  type RecordedRequest,
  type StubRoute,
} from '@testkit/render.js';
import { UNCONFIGURED_STATUS } from '@testkit/fixtures.js';

const UNAUTHENTICATED: StubRoute = [
  401,
  { error: { code: 'unauthorized', message: 'nope' } },
];

const PROVIDERS = {
  types: [
    {
      id: 'ollama',
      displayName: 'Ollama',
      wire: 'openai-chat',
      isLocal: true,
      isGateway: false,
      isOAuth: false,
      defaultApiBase: 'http://127.0.0.1:11434/v1',
      supportsModelListing: true,
    },
  ],
  instances: [],
};

function mount(overrides: Record<string, StubRoute> = {}): {
  readonly calls: RecordedRequest[];
} {
  const calls = stubApi({
    '/api/setup': [200, { required: true }],
    'POST /api/setup/claim': [200, { ok: true, expiresAtMs: 1 }],
    'POST /api/setup/password': [200, { ok: true, expiresAtMs: 1 }],
    '/api/auth/me': UNAUTHENTICATED,
    '/api/status': UNAUTHENTICATED,
    '/api/providers': [200, PROVIDERS],
    'PATCH /api/settings': [200, { config: {}, credentialsPresent: {} }],
    'PUT /api/settings/credentials': [204, null],
    'POST /api/models/refresh': [
      200,
      { models: [{ id: 'qwen3:8b', providerId: 'ollama' }], errors: {} },
    ],
    ...overrides,
  });

  // `Providers` mounts the overlay itself, above the login one. Rendering it a
  // second time here would be testing a copy nothing ships.
  render(<Providers client={testQueryClient()}>{null}</Providers>);

  return { calls };
}

/**
 * Past the language step, which every unclaimed install now opens on.
 *
 * Skipping rather than choosing: the browser's language is already applied, so
 * `Skip` is the answer "yes, that one" — and it is what the majority of first
 * runs will click. The step has its own test below.
 */
async function pastLanguage(
  user: ReturnType<typeof userEvent.setup>,
): Promise<void> {
  await user.click(await screen.findByRole('button', { name: 'Skip' }));
}

describe('an unclaimed install', () => {
  it('asks for the code rather than for a password nobody has set', async () => {
    const user = userEvent.setup();
    mount();
    await pastLanguage(user);

    expect(
      await screen.findByRole('heading', { name: 'Enter the setup code' }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText('Setup code')).toHaveFocus();
    // The login overlay is mounted too — both render on a 401 — and this one
    // has to be the one the user is looking at.
    expect(
      screen.queryByRole('heading', { name: 'Sign in' }),
    ).not.toBeInTheDocument();
  });

  it('offers no way to skip the two credential steps', async () => {
    const user = userEvent.setup();
    mount();
    await pastLanguage(user);
    await screen.findByRole('heading', { name: 'Enter the setup code' });

    // Skipping would leave a shell-capable agent with no password, which is the
    // state the wizard exists to end.
    expect(
      screen.queryByRole('button', { name: 'Skip' }),
    ).not.toBeInTheDocument();
    // `Back` *is* offered here, and only here among the credential steps: it
    // leads to the language question, where nothing has been spent yet. The
    // password step still has none — the code is single-use by then.
    expect(screen.getByRole('button', { name: 'Back' })).toBeInTheDocument();
  });

  it('spends the code, then sets a password, then offers a provider', async () => {
    const user = userEvent.setup();
    const { calls } = mount();
    await pastLanguage(user);

    await user.type(
      await screen.findByLabelText('Setup code'),
      'aaaa-bbbb-cccc',
    );
    await user.click(screen.getByRole('button', { name: 'Continue' }));

    // The username is prefilled with the default, so a first run is a password
    // and nothing else unless the operator wants otherwise.
    expect(await screen.findByLabelText('Username')).toHaveValue('ghost');
    await user.type(screen.getByLabelText('Password'), 'a-good-password');
    await user.type(
      screen.getByLabelText('Confirm password'),
      'a-good-password',
    );
    await user.click(screen.getByRole('button', { name: 'Continue' }));

    expect(
      await screen.findByRole('heading', { name: 'Add a model provider' }),
    ).toBeInTheDocument();

    const claim = calls.find((call) => call.path === '/api/setup/claim');
    expect(claim?.body).toEqual({ code: 'aaaa-bbbb-cccc' });
    expect(
      calls.find((call) => call.path === '/api/setup/password')?.body,
    ).toEqual({
      username: 'ghost',
      password: 'a-good-password',
    });
  });

  it('takes a username other than the default when one is typed', async () => {
    const user = userEvent.setup();
    const { calls } = mount();
    await pastLanguage(user);

    await user.type(
      await screen.findByLabelText('Setup code'),
      'aaaa-bbbb-cccc',
    );
    await user.click(screen.getByRole('button', { name: 'Continue' }));

    const username = await screen.findByLabelText('Username');
    await user.clear(username);
    await user.type(username, 'operator');
    await user.type(screen.getByLabelText('Password'), 'a-good-password');
    await user.type(
      screen.getByLabelText('Confirm password'),
      'a-good-password',
    );
    await user.click(screen.getByRole('button', { name: 'Continue' }));

    await waitFor(() => {
      expect(
        calls.find((call) => call.path === '/api/setup/password')?.body,
      ).toEqual({
        username: 'operator',
        password: 'a-good-password',
      });
    });
  });

  // The bound is checked in the browser as well as on the server, so the wizard
  // answers instantly rather than round-tripping to be told the rule.
  it('will not submit a password below the minimum length', async () => {
    const user = userEvent.setup();
    const { calls } = mount();
    await pastLanguage(user);

    await user.type(
      await screen.findByLabelText('Setup code'),
      'aaaa-bbbb-cccc',
    );
    await user.click(screen.getByRole('button', { name: 'Continue' }));

    await user.type(await screen.findByLabelText('Password'), 'short');
    await user.type(screen.getByLabelText('Confirm password'), 'short');

    expect(screen.getByRole('button', { name: 'Continue' })).toBeDisabled();
    expect(calls.some((call) => call.path === '/api/setup/password')).toBe(
      false,
    );
  });

  it('refuses to submit two passwords that differ, without asking the server', async () => {
    const user = userEvent.setup();
    const { calls } = mount();
    await pastLanguage(user);

    await user.type(
      await screen.findByLabelText('Setup code'),
      'aaaa-bbbb-cccc',
    );
    await user.click(screen.getByRole('button', { name: 'Continue' }));

    await user.type(
      await screen.findByLabelText('Password'),
      'one that is long enough',
    );
    await user.type(
      screen.getByLabelText('Confirm password'),
      'another that is long enough',
    );
    await user.click(screen.getByRole('button', { name: 'Continue' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('do not match');
    expect(calls.some((call) => call.path === '/api/setup/password')).toBe(
      false,
    );
  });

  it('says the code was wrong rather than leaving the field looking accepted', async () => {
    const user = userEvent.setup();
    mount({ 'POST /api/setup/claim': UNAUTHENTICATED });
    await pastLanguage(user);

    await user.type(
      await screen.findByLabelText('Setup code'),
      'zzzz-zzzz-zzzz',
    );
    await user.click(screen.getByRole('button', { name: 'Continue' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Incorrect or already-used',
    );
    expect(
      screen.getByRole('heading', { name: 'Enter the setup code' }),
    ).toBeInTheDocument();
  });
});

describe('a claimed install with no model', () => {
  it('starts at the provider step rather than asking for a spent code', async () => {
    // The tab was closed after the password step. The code no longer exists,
    // so asking for it would be a dead end on an install that is already
    // claimed.
    mount({
      '/api/setup': [200, { required: false }],
      '/api/status': [
        200,
        { ...UNCONFIGURED_STATUS, authEnabled: true, toolCount: 0 },
      ],
    });

    expect(
      await screen.findByRole('heading', { name: 'Add a model provider' }),
    ).toBeInTheDocument();
    // And it may be skipped: an install with no model still serves everything
    // but a turn.
    expect(screen.getByRole('button', { name: 'Skip' })).toBeInTheDocument();
  });
});

describe('a claimed, configured install', () => {
  it('renders nothing at all', async () => {
    mount({
      '/api/setup': [200, { required: false }],
      '/api/status': [
        200,
        {
          version: '0.0.0',
          protocolVersion: 2,
          uptimeMs: 1,
          model: 'llama3',
          provider: 'ollama',
          configured: true,
          workspaceId: 'default',
          workspaceCount: 1,
          authEnabled: true,
          toolCount: 1,
          mcpServersConnected: 0,
          extensionsLoaded: 0,
        },
      ],
    });

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
  });
});
