/**
 * The picker, and the line it holds between a preference and a binding.
 *
 * A workspace can be detached between one page load and the next, and the
 * picker holds two copies of one that may already be gone: the session's
 * binding, and this browser's remembered preference. They are deliberately
 * treated differently — the preference is corrected in place, the binding never
 * is — and that asymmetry is most of what is asserted here.
 *
 * The other half is the create/move split: with no session row the choice is
 * only a preference and must issue no request at all, and with a row it must
 * issue exactly one `PATCH`. Getting that backwards is how a picker silently
 * moves a conversation somebody was in the middle of.
 *
 * The router is stubbed rather than mounted, for the reason `agent-picker`
 * gives: the only thing needed from it is the "Manage workspaces" link.
 */

import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Providers } from '@/app/providers.js';
import { WorkspacePicker } from '@/workspaces/workspace-picker.js';
import { stubApi, testQueryClient, type StubRoute } from '@testkit/render.js';

vi.mock('@tanstack/react-router', () => ({
  Link: ({ children }: { readonly children: ReactNode }) => (
    <a href="/workspaces">{children}</a>
  ),
}));

const STORAGE_KEY = 'ghostai:workspace';

/**
 * A full `SessionSummary`, because the client parses what it is handed.
 *
 * A partial body is a failed parse and an empty query, which reads in a test as
 * "the picker ignored the binding" — a much more interesting bug than the one
 * actually present.
 */
const session = (workspaceId: string) => ({
  key: 'web-1',
  title: 'A session',
  messageCount: 2,
  createdAtMs: 1,
  updatedAtMs: 2,
  origin: 'web',
  workspaceId,
  agentId: 'default',
});

const WORKSPACES = {
  workspaces: [
    {
      id: 'default',
      name: 'Default',
      isDefault: true,
      createdAtMs: 1,
      updatedAtMs: 2,
      sessionCount: 3,
    },
    {
      id: 'research',
      name: 'Research',
      isDefault: false,
      createdAtMs: 1,
      updatedAtMs: 2,
      sessionCount: 1,
    },
  ],
};

afterEach(() => {
  localStorage.clear();
});

function mount(
  routes: Record<string, StubRoute> = {},
  props: { readonly sessionKey?: string } = {},
): { readonly calls: ReturnType<typeof stubApi> } {
  const calls = stubApi({ '/api/workspaces': [200, WORKSPACES], ...routes });
  render(
    <Providers client={testQueryClient()}>
      <WorkspacePicker {...props} />
    </Providers>,
  );
  return { calls };
}

describe('the workspace picker', () => {
  it('names the workspace the session is bound to, not the remembered one', async () => {
    // The binding wins over the preference. They disagree here on purpose:
    // opening somebody else's session link is exactly this case.
    localStorage.setItem(STORAGE_KEY, 'default');
    mount(
      { '/api/sessions/web-1': [200, session('research')] },
      { sessionKey: 'web-1' },
    );

    expect(
      await screen.findByRole('button', {
        name: 'Workspace for this session: Research',
      }),
    ).toBeInTheDocument();
  });

  it('marks a binding whose workspace is no longer listed', async () => {
    mount(
      { '/api/sessions/web-1': [200, session('archive')] },
      { sessionKey: 'web-1' },
    );

    expect(
      await screen.findByRole('button', {
        name: /archive is no longer listed/,
      }),
    ).toBeInTheDocument();
  });

  it('does not move a bound session on its own', async () => {
    // The counterpart of the preference reset below. A detached workspace's
    // files are still on disk, so silently moving the conversation to tidy up a
    // label would be a worse answer than showing the label.
    const { calls } = mount(
      { '/api/sessions/web-1': [200, session('archive')] },
      { sessionKey: 'web-1' },
    );

    await screen.findByRole('button', { name: /no longer listed/ });
    expect(calls.filter((call) => call.method === 'PATCH')).toEqual([]);
  });

  it('resets a remembered preference that names nothing', async () => {
    localStorage.setItem(STORAGE_KEY, 'archive');
    mount();

    await waitFor(() => {
      expect(localStorage.getItem(STORAGE_KEY)).toBe('default');
    });
  });

  it('leaves a remembered preference alone while the listing is in flight', async () => {
    // Every id looks missing before the list arrives, and a picker that reset on
    // each cold load would throw away a perfectly good preference.
    localStorage.setItem(STORAGE_KEY, 'research');
    mount({ '/api/workspaces': () => new Promise(() => undefined) as never });

    await Promise.resolve();
    expect(localStorage.getItem(STORAGE_KEY)).toBe('research');
  });

  it('only sets a preference when there is no session row yet', async () => {
    // A 404 from the session route is the signal that nobody has spoken yet.
    // Choosing here must not reach the server at all: there is no row to patch,
    // and patching would mint one.
    const { calls } = mount(
      { '/api/sessions/web-1': [404, { error: { message: 'no' } }] },
      { sessionKey: 'web-1' },
    );

    await screen.findByRole('button', { name: /Default/ });
    await userEvent.click(screen.getByRole('button'));
    await userEvent.click(
      await screen.findByRole('menuitemradio', { name: /Research/ }),
    );

    await waitFor(() => {
      expect(localStorage.getItem(STORAGE_KEY)).toBe('research');
    });
    expect(calls.filter((call) => call.method === 'PATCH')).toEqual([]);
  });

  it('moves the session with exactly one PATCH once a row exists', async () => {
    const { calls } = mount(
      {
        '/api/sessions/web-1': [200, session('default')],
        'PATCH /api/sessions/web-1': [200, session('research')],
      },
      { sessionKey: 'web-1' },
    );

    await screen.findByRole('button', {
      name: 'Workspace for this session: Default',
    });
    await userEvent.click(screen.getByRole('button'));
    await userEvent.click(
      await screen.findByRole('menuitemradio', { name: /Research/ }),
    );

    await waitFor(() => {
      const patches = calls.filter((call) => call.method === 'PATCH');
      expect(patches).toHaveLength(1);
      expect(patches[0]?.body).toEqual({ workspaceId: 'research' });
    });
    // The switcher follows the conversation rather than claiming to have set it.
    await waitFor(() => {
      expect(localStorage.getItem(STORAGE_KEY)).toBe('research');
    });
  });

  it('reports a refused move rather than swallowing it', async () => {
    mount(
      {
        '/api/sessions/web-1': [200, session('default')],
        'PATCH /api/sessions/web-1': [
          404,
          { error: { message: 'No such workspace: research' } },
        ],
      },
      { sessionKey: 'web-1' },
    );

    await screen.findByRole('button', {
      name: 'Workspace for this session: Default',
    });
    await userEvent.click(screen.getByRole('button'));
    await userEvent.click(
      await screen.findByRole('menuitemradio', { name: /Research/ }),
    );

    expect(
      await screen.findByText('Could not move the session'),
    ).toBeInTheDocument();
  });
});
