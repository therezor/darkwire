/**
 * Settings → Channels, through the real router.
 *
 * Four things this panel could get wrong without looking wrong, and each is a
 * case below:
 *
 *  - **The token must not reach `config.yaml`.** It goes to the vault through
 *    its own endpoint, so the assertion is not only that the `PUT` happens but
 *    that the secret appears in no other request.
 *  - **A save carries only the `channels` branch.** `ConfigPatch` is a
 *    deep-partial precisely so one panel does not rewrite another's fields.
 *  - **A save carries the whole block, not the edited field.** Arrays replace
 *    on merge, so a patch naming only `allowlist` would leave `admins` as
 *    whatever it was before someone emptied it.
 *  - **The refusals happen here, not at the bot.** A channel reports a bad
 *    allowlist by refusing to start, which from a settings panel is a save that
 *    appears to work and a bot that quietly stops answering.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { ConfigSchema, type ChannelStatus } from '@darkwire/protocol';

import { Providers } from '@/app/providers.js';
import { createAppRouter } from '@/app/router.js';
import {
  stubApi,
  testQueryClient,
  type RecordedRequest,
  type StubRoute,
} from '@testkit/render.js';
import { STATUS } from '@testkit/fixtures.js';

const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
  '/api/setup': [200, { required: false }],
  '/api/status': [200, STATUS],
  '/api/sessions': [200, { sessions: [], total: 0 }],
  '/api/notifications': [200, { notifications: [], unreadCount: 0, total: 0 }],
};

const OFF: ChannelStatus = {
  id: 'telegram',
  enabled: true,
  configured: false,
  running: false,
};

function settings(
  telegram: Record<string, unknown> = {},
  channels: readonly ChannelStatus[] = [OFF],
): Record<string, unknown> {
  return {
    config: ConfigSchema.parse({ channels: { telegram } }),
    credentialsPresent: {},
    channels,
  };
}

function mount(response: Record<string, unknown> = settings()): {
  readonly user: ReturnType<typeof userEvent.setup>;
  readonly calls: RecordedRequest[];
} {
  const calls = stubApi({
    ...SHELL_ROUTES,
    '/api/settings': [200, response],
    'PATCH /api/settings': [200, response],
    'PUT /api/settings/credentials': [204, undefined],
  });

  const user = userEvent.setup();
  const router = createAppRouter();
  router.update({
    history: createMemoryHistory({
      initialEntries: ['/settings?panel=channels'],
    }),
  });
  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );
  return { user, calls };
}

const patchesOf = (calls: readonly RecordedRequest[]): RecordedRequest[] =>
  calls.filter((call) => call.method === 'PATCH');

const credentialsOf = (calls: readonly RecordedRequest[]): RecordedRequest[] =>
  calls.filter((call) => call.path === '/api/settings/credentials');

async function save(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.click(await screen.findByRole('button', { name: 'Save changes' }));
}

describe('the channels panel', () => {
  it('shows what the settings already hold', async () => {
    mount(settings({ allowlist: ['4471|me'], admins: ['4471'] }));

    expect(await screen.findByLabelText('Allowed accounts')).toHaveValue(
      '4471|me',
    );
    expect(screen.getByLabelText('Administrators')).toHaveValue('4471');
  });

  it('saves the whole block, and only the channels branch', async () => {
    const { user, calls } = mount(settings({ allowlist: ['4471'] }));

    const allowlist = await screen.findByLabelText('Allowed accounts');
    await user.clear(allowlist);
    await user.type(allowlist, '4471|me\n-100200|team');
    await save(user);

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    const patch = patchesOf(calls)[0]?.body as Record<string, unknown>;
    expect(Object.keys(patch)).toEqual(['channels']);
    expect(patch.channels).toEqual({
      telegram: {
        enabled: true,
        // Every field this panel owns, not only the edited one: arrays replace
        // on merge, so a partial patch would strand the rest.
        allowlist: ['4471|me', '-100200|team'],
        admins: [],
      },
    });
  });

  it('sends the token to the vault, and nowhere else', async () => {
    const { user, calls } = mount(settings({ allowlist: ['4471'] }));

    await user.type(
      await screen.findByLabelText('Bot token'),
      '123456:SECRET-VALUE',
    );
    await save(user);

    await waitFor(() => {
      expect(credentialsOf(calls)).toHaveLength(1);
    });
    expect(credentialsOf(calls)[0]?.body).toEqual({
      namespace: 'channels',
      key: 'telegram',
      value: '123456:SECRET-VALUE',
    });
    // The one assertion that matters: it is a credential, so it must appear in
    // no request that writes a file.
    for (const call of patchesOf(calls)) {
      expect(JSON.stringify(call.body)).not.toContain('SECRET-VALUE');
    }
  });

  it('leaves the vault alone when the token was not touched', async () => {
    const { user, calls } = mount(
      settings({ allowlist: ['4471'] }, [{ ...OFF, configured: true }]),
    );

    const allowlist = await screen.findByLabelText('Allowed accounts');
    await user.clear(allowlist);
    await user.type(allowlist, '9999');
    await save(user);

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    // An untouched box over a saved token means nothing, and writing `''` here
    // would delete the credential the operator never went near.
    expect(credentialsOf(calls)).toHaveLength(0);
  });

  it('clears the token when the box is emptied deliberately', async () => {
    const { user, calls } = mount(
      settings({ allowlist: ['4471'] }, [{ ...OFF, configured: true }]),
    );

    const token = await screen.findByLabelText('Bot token');
    await user.type(token, 'x');
    await user.clear(token);
    await save(user);

    await waitFor(() => {
      expect(credentialsOf(calls)).toHaveLength(1);
    });
    expect(credentialsOf(calls)[0]?.body).toMatchObject({ value: null });
  });

  it('refuses to enable a bot that would answer nobody', async () => {
    // The channel refuses to start on this, which from here is a save that
    // looks fine and a bot that never replies.
    const { user, calls } = mount(settings({ enabled: false, allowlist: [] }));

    await user.click(await screen.findByRole('switch', { name: 'Enabled' }));
    await save(user);

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'would answer nobody',
    );
    expect(patchesOf(calls)).toHaveLength(0);
  });

  it('refuses an entry that is not a Telegram id', async () => {
    const { user, calls } = mount(settings({ allowlist: ['4471'] }));

    const allowlist = await screen.findByLabelText('Allowed accounts');
    await user.clear(allowlist);
    await user.type(allowlist, '@someone');
    await save(user);

    expect(await screen.findByRole('alert')).toHaveTextContent('@someone');
    expect(patchesOf(calls)).toHaveLength(0);
  });

  it('refuses an administrator who cannot reach the bot', async () => {
    const { user, calls } = mount(settings({ allowlist: ['4471'] }));

    await user.type(await screen.findByLabelText('Administrators'), '9999');
    await save(user);

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'not on the allowlist',
    );
    expect(patchesOf(calls)).toHaveLength(0);
  });

  it('allows an empty allowlist while the channel is switched off', async () => {
    // Emptying the list is only a mistake while the bot is meant to answer.
    const { user, calls } = mount(settings({ allowlist: ['4471'] }));

    await user.click(await screen.findByRole('switch', { name: 'Enabled' }));
    await user.clear(screen.getByLabelText('Allowed accounts'));
    await save(user);

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    expect(
      (patchesOf(calls)[0]?.body as { channels: { telegram: unknown } })
        .channels.telegram,
    ).toMatchObject({ enabled: false, allowlist: [] });
  });

  it('says the bot is connected, and who it is', async () => {
    mount(
      settings({ allowlist: ['4471'] }, [
        {
          id: 'telegram',
          enabled: true,
          configured: true,
          running: true,
          detail: '@ghost_bot',
        },
      ]),
    );

    expect(await screen.findByText('Connected')).toBeInTheDocument();
    expect(screen.getByText('@ghost_bot')).toBeInTheDocument();
  });

  it('says what is missing when there is no token', async () => {
    mount(settings({ allowlist: ['4471'] }));

    expect(await screen.findByText('Needs a token')).toBeInTheDocument();
  });

  it('says why the bot is down, when the server knows', async () => {
    mount(
      settings({ allowlist: ['4471'] }, [
        {
          id: 'telegram',
          enabled: true,
          configured: true,
          running: false,
          detail: 'Telegram getMe failed (401): Unauthorized',
        },
      ]),
    );

    expect(await screen.findByText(/Unauthorized/u)).toBeInTheDocument();
  });

  it('reads a token box as saved rather than empty', async () => {
    // The vault is write-only, so an empty box over a working bot is the
    // failure this hint exists to prevent.
    mount(settings({ allowlist: ['4471'] }, [{ ...OFF, configured: true }]));

    expect(await screen.findByLabelText('Bot token')).toHaveValue('');
    expect(screen.getByText(/A token is saved/u)).toBeInTheDocument();
  });

  it('reverts to what the server sent', async () => {
    const { user } = mount(settings({ allowlist: ['4471'] }));

    const allowlist = await screen.findByLabelText('Allowed accounts');
    await user.clear(allowlist);
    await user.type(allowlist, '9999');
    await user.click(screen.getByRole('button', { name: 'Revert' }));

    expect(await screen.findByLabelText('Allowed accounts')).toHaveValue(
      '4471',
    );
  });
});
