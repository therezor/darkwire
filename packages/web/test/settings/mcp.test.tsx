/**
 * The MCP servers panel and the MCP editor, driven through the real router.
 *
 * **This is where the connection states are asserted**, and it is deliberate
 * that they are asserted here and nowhere else. Whether a row reads
 * `Connecting`, `Unreachable` or `Connected` depends on when a background dial
 * settles, and the e2e suite has no way to hold that still — asserting it there
 * is the class of flake `CLAUDE.md` names, red in CI four runs running while
 * green on every laptop. Here the status response is a fixture, so the state is
 * a fact rather than a race.
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
  tools: {
    mcpServers: {
      files: { command: 'npx', args: ['-y', 'server-filesystem'] },
      github: { url: 'https://mcp.github.test/mcp' },
    },
  },
});

const READY = {
  id: 'files',
  transport: 'stdio',
  state: 'ready',
  enabled: true,
  tools: ['mcp_files_read', 'mcp_files_write'],
  filteredTools: [],
  serverName: 'filesystem',
  serverVersion: '1.0.0',
  warnings: [],
};

const FAILED = {
  id: 'github',
  transport: 'streamableHttp',
  state: 'failed',
  enabled: true,
  tools: [],
  filteredTools: [],
  serverName: '',
  serverVersion: '',
  warnings: [],
  lastError: 'ECONNREFUSED 140.82.121.5:443',
};

const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
  '/api/setup': [200, { required: false }],
  '/api/status': [200, STATUS],
  '/api/sessions': [200, { sessions: [], total: 0 }],
  '/api/notifications': [200, { notifications: [], unreadCount: 0, total: 0 }],
};

function mount(
  path = '/settings?panel=mcp',
  overrides: Record<string, StubRoute> = {},
): {
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
    '/api/mcp': [200, { servers: [READY, FAILED] }],
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

  return { user, calls };
}

const patchesOf = (calls: readonly RecordedRequest[]): ConfigPatch[] =>
  calls
    .filter((call) => call.method === 'PATCH')
    .map((call) => call.body as ConfigPatch);

describe('the MCP servers panel', () => {
  it('shows the servers and a way to add one', async () => {
    // The two halves of a settings panel that is actually a settings panel:
    // what is configured, and a control that changes it.
    //
    // Both awaited, and the first one deliberately is not the words `MCP
    // servers`: the tab strip renders that immediately from the panel table,
    // so a `findByText` for it would resolve before `GET /api/settings` had
    // answered and leave the second assertion racing an empty tabpanel.
    mount();
    expect(
      await screen.findByRole('link', { name: 'New MCP server' }),
    ).toBeInTheDocument();
    expect(await screen.findByText('files')).toBeInTheDocument();
  });

  it('promises no screen that is not on it', async () => {
    // This panel used to end in a "Still to come" list naming OAuth, extensions
    // and — before them — skills and channels. It is gone: a settings screen
    // advertising a form an operator cannot open is a screen they check twice.
    // Skills in particular are workspace folders rather than configuration, so
    // a row promising them taught the wrong thing.
    mount();
    await screen.findByText('files');

    for (const gone of ['Still to come', 'OAuth connections', 'Skills']) {
      expect(screen.queryByText(gone)).not.toBeInTheDocument();
    }
  });

  it('joins the configured servers to their live state', async () => {
    // The whole reason the screen makes two requests: `GET /api/settings` says
    // what was asked for and `GET /api/mcp` says what came of it.
    mount();

    // Awaited, because the two halves arrive on their own schedule: the row
    // exists as soon as the settings do, and its badge as soon as the status
    // does. Both are durable once settled.
    expect(await screen.findByText('files')).toBeInTheDocument();
    expect(await screen.findByText('Connected')).toBeInTheDocument();
    expect(screen.getByText('2 tools')).toBeInTheDocument();

    expect(screen.getByText('github')).toBeInTheDocument();
    expect(screen.getByText('Unreachable')).toBeInTheDocument();
  });

  it('shows the reason a server is down, which is the sentence nothing else carries', async () => {
    mount();
    expect(
      await screen.findByText(/ECONNREFUSED 140\.82\.121\.5/),
    ).toBeInTheDocument();
  });

  it('says so rather than guessing when the client has no answer', async () => {
    // A build with `mcp: false` answers `{servers: []}`, and a configured
    // server it knows nothing about is `Unknown` rather than a guess at `Off`.
    mount('/settings?panel=mcp', { '/api/mcp': [200, { servers: [] }] });
    expect(await screen.findByText('files')).toBeInTheDocument();
    expect(screen.getAllByText('Unknown')).toHaveLength(2);
  });

  it('switches a server off without opening the editor', async () => {
    const { user, calls } = mount();
    await screen.findByText('files');

    await user.click(
      await screen.findByRole('button', { name: 'Actions for files' }),
    );
    await user.click(await screen.findByRole('menuitem', { name: 'Disable' }));

    await waitFor(() => {
      expect(patchesOf(calls)).toEqual([
        { tools: { mcpServers: { files: { enabled: false } } } },
      ]);
    });
  });

  it('asks before deleting, and deletes with a null', async () => {
    const { user, calls } = mount();
    await screen.findByText('github');

    await user.click(
      await screen.findByRole('button', { name: 'Actions for github' }),
    );
    await user.click(await screen.findByRole('menuitem', { name: 'Delete' }));
    // The question is asked: a delete takes the server's tools away from every
    // agent that had been granted one.
    expect(await screen.findByRole('dialog')).toHaveTextContent('github');

    await user.click(await screen.findByRole('button', { name: 'Delete' }));

    await waitFor(() => {
      expect(patchesOf(calls)).toEqual([
        { tools: { mcpServers: { github: null } } },
      ]);
    });
  });
});

describe('the MCP editor', () => {
  it('opens on the stored settings, on the right half of the form', async () => {
    mount('/settings/mcp/files');

    expect(await screen.findByLabelText('Command')).toHaveValue('npx');
    expect(screen.getByLabelText('Arguments')).toHaveValue(
      '-y\nserver-filesystem',
    );
    // The URL half belongs to another transport and is not on screen.
    expect(screen.queryByLabelText('URL')).not.toBeInTheDocument();
  });

  it('has no Authorization section for a transport that cannot carry one', async () => {
    mount('/settings/mcp/files');
    await screen.findByLabelText('Command');
    expect(screen.queryByText('Authorization')).not.toBeInTheDocument();
  });

  it('offers Authorization on an HTTP server, and only when asked for', async () => {
    mount('/settings/mcp/github');

    expect(await screen.findByLabelText('URL')).toHaveValue(
      'https://mcp.github.test/mcp',
    );
    expect(screen.getByText('Authorization')).toBeInTheDocument();
    expect(screen.queryByLabelText('Client ID')).not.toBeInTheDocument();
  });

  it('clears the other transport half when the transport moves', async () => {
    // `resolveSpec` refuses an entry naming both a command and a url, so a
    // switched transport has to take the old one back out.
    const { user, calls } = mount('/settings/mcp/files');
    await screen.findByLabelText('Command');

    await user.click(screen.getByRole('combobox', { name: 'Transport' }));
    await user.click(
      await screen.findByRole('option', { name: 'Streamable HTTP' }),
    );
    await user.type(await screen.findByLabelText('URL'), 'https://a.test/mcp');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      const [patch] = patchesOf(calls);
      expect(patch?.tools?.mcpServers?.files).toMatchObject({
        type: 'streamableHttp',
        url: 'https://a.test/mcp',
        command: '',
        args: [],
      });
    });
  });

  it('refuses to save a URL this client will not dial', async () => {
    const { user, calls } = mount('/settings/mcp/github');
    const url = await screen.findByLabelText('URL');

    await user.clear(url);
    await user.type(url, 'file:///etc/passwd');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    expect(await screen.findByText(/http/)).toBeInTheDocument();
    expect(patchesOf(calls)).toEqual([]);
  });

  it('says so on a link to a server that is not there', async () => {
    mount('/settings/mcp/gone');
    expect(await screen.findByRole('alert')).toHaveTextContent('gone');
  });

  it('surfaces the authorize link for a server waiting on an operator', async () => {
    mount('/settings/mcp/github', {
      '/api/mcp': [
        200,
        {
          servers: [
            {
              ...FAILED,
              state: 'needs_authorization',
              lastError: undefined,
              authorizationUrl: 'https://auth.github.test/authorize?state=abc',
            },
          ],
        },
      ],
    });

    const link = await screen.findByRole('link', {
      name: /Authorize GhostAI with github/,
    });
    expect(link).toHaveAttribute(
      'href',
      'https://auth.github.test/authorize?state=abc',
    );
  });

  it('shows what a server warned about, beside the field that fixes it', async () => {
    mount('/settings/mcp/files', {
      '/api/mcp': [
        200,
        {
          servers: [
            {
              ...READY,
              warnings: [
                '"nope" in enabledTools matches no tool this server offers',
              ],
            },
          ],
        },
      ],
    });

    expect(await screen.findByText(/matches no tool/)).toBeInTheDocument();
  });
});
