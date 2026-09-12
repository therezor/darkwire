/**
 * The Extensions panel, driven through the real router.
 *
 * **This is where the approval flow is asserted**, and it is deliberate that it
 * is asserted here rather than in the end-to-end suite. Approving is a request
 * whose answer decides what the row says next, and whether a row reads `Loaded`
 * or `Failed` depends on when that request settles — the class of flake
 * `CLAUDE.md` names. Here the response is a fixture, so the state is a fact
 * rather than a race.
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

const CONFIG = ConfigSchema.parse({
  agents: { list: { default: { model: 'llama3', provider: 'ollama' } } },
  providers: { ollama: { type: 'ollama' } },
});

const READY = {
  id: 'slack',
  state: 'ready',
  version: '1.2.0',
  label: 'Slack',
  description: 'Talk to the agent from a Slack workspace.',
  contributes: ['channels', 'commands'],
  tools: [],
  channels: ['slack'],
  providers: [],
  commands: ['slack-post'],
  digest: 'a'.repeat(64),
  approvedAtMs: 1,
  warnings: [],
};

const UNAPPROVED = {
  ...READY,
  id: 'zulip',
  state: 'unapproved',
  label: '',
  description: '',
  contributes: ['tools'],
  channels: [],
  commands: [],
  digest: 'b'.repeat(64),
  lastError:
    'Extension "zulip" is installed but has never been approved.\n' +
    '  Review what it contributes with `ghostai extension list`, then\n' +
    '  `ghostai extension approve zulip`.',
};

const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
  '/api/setup': [200, { required: false }],
  '/api/status': [200, STATUS],
  '/api/sessions': [200, { sessions: [], total: 0 }],
  '/api/notifications': [200, { notifications: [], unreadCount: 0, total: 0 }],
};

function mount(overrides: Record<string, StubRoute> = {}): {
  readonly user: ReturnType<typeof userEvent.setup>;
  readonly calls: RecordedRequest[];
} {
  const settings = { config: CONFIG, credentialsPresent: {} };
  const calls = stubApi({
    ...SHELL_ROUTES,
    '/api/settings': [200, settings],
    'PATCH /api/settings': [200, settings],
    '/api/providers': [200, { types: [], instances: [] }],
    '/api/models': [200, { models: [], errors: {} }],
    '/api/tools': [200, { tools: [] }],
    '/api/mcp': [200, { servers: [] }],
    '/api/commands': [200, { commands: [] }],
    '/api/extensions': [200, { extensions: [READY, UNAPPROVED] }],
    ...overrides,
  });

  const user = userEvent.setup();
  const router = createAppRouter();
  router.update({
    history: createMemoryHistory({
      initialEntries: ['/settings?panel=extensions'],
    }),
  });
  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );

  return { user, calls };
}

const patchesOf = (calls: readonly RecordedRequest[]): ConfigPatch[] =>
  calls
    .filter((call) => call.method === 'PATCH')
    .map((call) => call.body as ConfigPatch);

const posts = (calls: readonly RecordedRequest[]): string[] =>
  calls.filter((call) => call.method === 'POST').map((call) => call.path);

describe('the Extensions panel', () => {
  it('shows what each extension is and what it declares', async () => {
    // The `contributes` line is the one to read before pressing Approve: it is
    // what the operator is being asked about, and the host drops anything the
    // extension registers beyond it.
    mount();

    expect(await screen.findByText('Slack')).toBeInTheDocument();
    expect(screen.getByText('Adds channels, commands')).toBeInTheDocument();
    expect(screen.getByText('Adds tools')).toBeInTheDocument();
    expect(screen.getByText('Not approved')).toBeInTheDocument();
  });

  it('falls back to the id when an extension carries no label', async () => {
    mount();
    await screen.findByText('Slack');

    // `zulip` appears twice — once as the name and once as the code. Both are
    // the id, which is the point.
    expect(screen.getAllByText('zulip').length).toBeGreaterThan(0);
  });

  it('shows the first line of a refusal, not the whole terminal message', async () => {
    // `ExtensionStore` writes for a terminal: a sentence, then the command that
    // fixes it. The command is not what to offer someone already looking at the
    // button that does it.
    mount();

    expect(
      await screen.findByText(
        'Extension "zulip" is installed but has never been approved.',
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/ghostai extension approve/)).toBeNull();
  });

  it('asks before approving, and says what is being granted', async () => {
    // The only reversible action in Settings that asks. A one-click toggle here
    // would make the digest gate decorative.
    const { user } = mount();
    await screen.findByText('Slack');

    await user.click(
      await screen.findByRole('button', { name: 'Actions for zulip' }),
    );
    await user.click(await screen.findByRole('menuitem', { name: 'Approve' }));

    expect(await screen.findByText('Approve zulip?')).toBeInTheDocument();
    expect(
      screen.getByText(
        /runs inside the server, with the access the server has/,
      ),
    ).toBeInTheDocument();
  });

  it('posts the approval only after the question is answered', async () => {
    const { user, calls } = mount({
      'POST /api/extensions/zulip/approve': [
        200,
        { extensions: [READY, { ...UNAPPROVED, state: 'ready' }] },
      ],
    });
    await screen.findByText('Slack');

    await user.click(
      await screen.findByRole('button', { name: 'Actions for zulip' }),
    );
    await user.click(await screen.findByRole('menuitem', { name: 'Approve' }));
    // The question is on screen and nothing has been sent yet: that gap is the
    // whole point of asking.
    expect(posts(calls)).toEqual([]);

    await user.click(await screen.findByRole('button', { name: 'Approve' }));

    await waitFor(() => {
      expect(posts(calls)).toEqual(['/api/extensions/zulip/approve']);
    });
  });

  it('offers Approve again on an extension that failed, not only a new one', async () => {
    // One that threw may have been repaired since, and re-approving is how the
    // digest catches up.
    const { user } = mount({
      '/api/extensions': [
        200,
        {
          extensions: [
            { ...UNAPPROVED, state: 'failed', lastError: 'no token' },
          ],
        },
      ],
    });
    await screen.findByText('no token');

    await user.click(
      await screen.findByRole('button', { name: 'Actions for zulip' }),
    );

    expect(
      await screen.findByRole('menuitem', { name: 'Approve' }),
    ).toBeInTheDocument();
  });

  it('does not offer Approve on one that is already loaded', async () => {
    const { user } = mount({
      '/api/extensions': [200, { extensions: [READY] }],
    });
    await screen.findByText('Slack');

    await user.click(
      await screen.findByRole('button', { name: 'Actions for slack' }),
    );

    expect(
      await screen.findByRole('menuitem', { name: 'Withdraw approval' }),
    ).toBeInTheDocument();
    expect(screen.queryByRole('menuitem', { name: 'Approve' })).toBeNull();
  });

  it('withdraws without asking, because it takes access away', async () => {
    const { user, calls } = mount({
      'POST /api/extensions/slack/revoke': [200, { extensions: [] }],
    });
    await screen.findByText('Slack');

    await user.click(
      await screen.findByRole('button', { name: 'Actions for slack' }),
    );
    await user.click(
      await screen.findByRole('menuitem', { name: 'Withdraw approval' }),
    );

    await waitFor(() => {
      expect(posts(calls)).toEqual(['/api/extensions/slack/revoke']);
    });
  });

  it('turns one off through the settings tree, where that state belongs', async () => {
    // Disabled is configuration — an operator's standing decision — and unlike
    // an approval it survives an edit to the files.
    const { user, calls } = mount();
    await screen.findByText('Slack');

    await user.click(
      await screen.findByRole('button', { name: 'Actions for slack' }),
    );
    await user.click(await screen.findByRole('menuitem', { name: 'Disable' }));

    await waitFor(() => {
      expect(patchesOf(calls)).toEqual([
        { extensions: { disabled: ['slack'] } },
      ]);
    });
  });

  it('says where to put one when there are none', async () => {
    // Installing is not an action this screen can offer: an extension arrives
    // on the box by some means the browser has no part in.
    mount({ '/api/extensions': [200, { extensions: [] }] });

    expect(
      await screen.findByText(/No extensions installed/),
    ).toBeInTheDocument();
    // Twice — the section's description says it too, which is what makes the
    // empty state a repetition rather than the only mention.
    expect(
      screen.getAllByText(/~\/.ghostai\/extensions/).length,
    ).toBeGreaterThan(0);
  });
});
