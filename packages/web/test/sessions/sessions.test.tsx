/**
 * The sessions page.
 *
 * The cases worth holding here are the ones that separate this list from every
 * other one in the app: the search and the sort go to the *server*, so what has
 * to be asserted is the request, not the rows left on screen after a client-side
 * filter. A test that only checked the rows would pass against an implementation
 * that filtered the page in hand — which is the bug this page exists to avoid,
 * because it would search the newest 25 sessions and confidently report
 * nothing for the one from last month.
 *
 * The rename and delete flows are here rather than in the sidebar's tests
 * because both now live in `use-sessions.ts` and the sidebar calls the same two.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { Providers } from '@/app/providers.js';
import { createAppRouter } from '@/app/router.js';
import {
  stubApi,
  testQueryClient,
  type RecordedRequest,
  type StubRoute,
} from '@testkit/render.js';
import { STATUS } from '@testkit/fixtures.js';

function session(
  over: Partial<Record<string, unknown>> = {},
): Record<string, unknown> {
  return {
    key: 'web:1',
    title: 'Fix the login throttle',
    messageCount: 12,
    createdAtMs: Date.now() - 86_400_000,
    updatedAtMs: Date.now() - 7_200_000,
    origin: 'web',
    workspaceId: 'default',
    ...over,
  };
}

const LIST = {
  sessions: [
    session(),
    session({
      key: 'auto:1',
      title: 'Nightly digest',
      origin: 'automation',
      messageCount: 1,
    }),
  ],
  total: 2,
};

const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
  '/api/setup': [200, { required: false }],
  '/api/status': [200, { ...STATUS, model: 'llama3', toolCount: 0 }],
  '/api/notifications': [200, { notifications: [], unreadCount: 0, total: 0 }],
  '/api/workspaces': [
    200,
    {
      workspaces: [
        {
          id: 'default',
          name: 'Default',
          isDefault: true,
          sessionCount: 2,
          createdAtMs: 1,
          updatedAtMs: 2,
        },
      ],
    },
  ],
  '/api/agents': [200, { agents: [] }],
};

function mount(overrides: Record<string, StubRoute> = {}): {
  readonly user: ReturnType<typeof userEvent.setup>;
  readonly calls: RecordedRequest[];
} {
  const calls = stubApi({
    ...SHELL_ROUTES,
    '/api/sessions': [200, LIST],
    ...overrides,
  });

  const router = createAppRouter();
  router.update({
    history: createMemoryHistory({ initialEntries: ['/sessions'] }),
  });

  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );

  return { user: userEvent.setup(), calls };
}

/**
 * The page's own list.
 *
 * The sidebar is on screen too and holds the same sessions, so its kebab
 * carries the same accessible name as this one's. Scoping to the `DataList`
 * rather than renaming either: they are the same action on the same object, and
 * a screen reader tells them apart by the landmark they sit in.
 */
function list(): HTMLElement {
  return screen.getByRole('list', { name: 'Sessions' });
}

/** The requests this page made for a page of sessions. */
function listCalls(calls: readonly RecordedRequest[]): RecordedRequest[] {
  return calls.filter(
    (call) => call.method === 'GET' && call.path === '/api/sessions',
  );
}

describe('the sessions page', () => {
  it('lists what the server sent', async () => {
    mount();

    expect(
      await screen.findByRole('link', { name: 'Open Fix the login throttle' }),
    ).toBeVisible();
    expect(
      screen.getByRole('link', { name: 'Open Nightly digest' }),
    ).toBeVisible();
  });

  it('names the count rather than leaving a bare number beside a time', async () => {
    mount();

    await screen.findByRole('link', { name: 'Open Fix the login throttle' });
    expect(screen.getByText('12 messages')).toBeInTheDocument();
    // Pluralised, not "1 messages".
    expect(screen.getByText('1 message')).toBeInTheDocument();
  });

  /**
   * The badge `SessionStore.listSessions` asks for in its note about a
   * five-minute job filling a recency-sorted list. `web` gets none: it is
   * almost every row, so badging it would say nothing.
   */
  it('marks the sessions nobody started by hand', async () => {
    mount();

    await screen.findByRole('link', { name: 'Open Nightly digest' });
    expect(screen.getByText('automation')).toBeInTheDocument();
    expect(screen.queryByText('web')).not.toBeInTheDocument();
  });

  it('falls back to a name for a session nobody has spoken in', async () => {
    mount({
      '/api/sessions': [200, { sessions: [session({ title: '' })], total: 1 }],
    });

    expect(
      await screen.findByRole('link', { name: 'Open New session' }),
    ).toBeVisible();
  });

  it('asks for every session, whatever workspace it is in', async () => {
    // The page used to scope itself to a sidebar switcher, which meant a
    // conversation moved to another workspace silently left the list and the
    // only way back was to guess where it had gone. A workspace says where a
    // conversation's files are, not which list it belongs in.
    const { calls } = mount();

    await screen.findByRole('link', { name: 'Open Fix the login throttle' });
    expect(listCalls(calls).at(-1)?.query.get('workspace')).toBeNull();
  });
});

describe('searching sessions', () => {
  /**
   * The whole reason this page exists. A client-side filter over the page in
   * hand would search the newest 25 and report nothing for the rest, which is a
   * search that is confidently wrong.
   */
  it('sends the query to the server rather than filtering the page it has', async () => {
    const { user, calls } = mount();

    await screen.findByRole('link', { name: 'Open Fix the login throttle' });
    await user.type(
      screen.getByRole('searchbox', { name: 'Filter sessions' }),
      'login',
    );

    await waitFor(() => {
      expect(
        listCalls(calls).some((call) => call.query.get('q') === 'login'),
      ).toBe(true);
    });
  });

  /**
   * Two different sentences, because they are two different situations: an
   * install with no sessions yet, and a search that found none. Showing
   * "No sessions yet" to someone who has just typed a query reads as data
   * loss.
   */
  it('tells an empty workspace apart from a search that matched nothing', async () => {
    const { user } = mount({
      '/api/sessions': [200, { sessions: [], total: 0 }],
    });

    expect(await screen.findByText('No sessions yet.')).toBeInTheDocument();

    await user.type(
      screen.getByRole('searchbox', { name: 'Filter sessions' }),
      'login',
    );

    expect(
      await screen.findByText('No session matches “login”.'),
    ).toBeInTheDocument();
  });

  it('sends the chosen column and direction', async () => {
    const { user, calls } = mount();

    await screen.findByRole('link', { name: 'Open Fix the login throttle' });
    await user.click(screen.getByRole('button', { name: /Sort by/ }));
    await user.click(
      await screen.findByRole('menuitemradio', { name: 'Title' }),
    );

    await waitFor(() => {
      const last = listCalls(calls).at(-1);
      expect(last?.query.get('sort')).toBe('title');
      // A title reads from A, so choosing it opens ascending. See `sortBy`.
      expect(last?.query.get('desc')).toBe('false');
    });
  });
});

describe('paging sessions', () => {
  const full = {
    sessions: Array.from({ length: 25 }, (unused, index) =>
      session({
        key: `web:${String(index)}`,
        title: `Session ${String(index)}`,
      }),
    ),
    total: 60,
  };

  it('says how many there are, which is what a search is judged by', async () => {
    mount({ '/api/sessions': [200, full] });

    expect(await screen.findByText('Showing 1–25 of 60')).toBeInTheDocument();
  });

  it('asks the server for the page rather than slicing the one it has', async () => {
    const { user, calls } = mount({ '/api/sessions': [200, full] });

    const pager = await screen.findByRole('navigation', { name: 'Sessions' });
    await user.click(within(pager).getByRole('button', { name: 'Page 3' }));

    await waitFor(() => {
      expect(
        listCalls(calls).some((call) => call.query.get('offset') === '50'),
      ).toBe(true);
    });
  });

  /**
   * Otherwise a search from page 3 asks for rows 51–75 of a result set with four
   * matches in it, and the page comes back empty under a control saying there
   * are matches.
   */
  it('returns to the first page when the search changes', async () => {
    const { user, calls } = mount({ '/api/sessions': [200, full] });

    const pager = await screen.findByRole('navigation', { name: 'Sessions' });
    await user.click(within(pager).getByRole('button', { name: 'Page 3' }));
    await waitFor(() => {
      expect(
        listCalls(calls).some((call) => call.query.get('offset') === '50'),
      ).toBe(true);
    });

    await user.type(
      screen.getByRole('searchbox', { name: 'Filter sessions' }),
      'login',
    );

    await waitFor(() => {
      const searched = listCalls(calls).filter(
        (call) => call.query.get('q') === 'login',
      );
      expect(searched.length).toBeGreaterThan(0);
      expect(
        searched.every((call) => (call.query.get('offset') ?? '0') === '0'),
      ).toBe(true);
    });
  });
});

describe('acting on a session', () => {
  it('renames one through a dialog rather than in place', async () => {
    const { user, calls } = mount({
      'PATCH /api/sessions/web%3A1': [200, session({ title: 'Renamed' })],
    });

    await screen.findByRole('link', { name: 'Open Fix the login throttle' });
    await user.click(
      within(list()).getByRole('button', {
        name: 'Actions for Fix the login throttle',
      }),
    );
    await user.click(await screen.findByRole('menuitem', { name: 'Rename' }));

    const dialog = await screen.findByRole('dialog');
    await user.clear(within(dialog).getByRole('textbox'));
    await user.type(within(dialog).getByRole('textbox'), 'Renamed');
    await user.click(within(dialog).getByRole('button', { name: 'Rename' }));

    await waitFor(() => {
      expect(calls.some((call) => call.method === 'PATCH')).toBe(true);
    });
    expect(calls.find((call) => call.method === 'PATCH')?.body).toEqual({
      title: 'Renamed',
    });
  });

  /** The sidebar deleted on one click. This is the thing that fixed. */
  it('asks before it deletes, and sends nothing until the answer is yes', async () => {
    const { user, calls } = mount({
      'DELETE /api/sessions/web%3A1': [204, null],
    });

    await screen.findByRole('link', { name: 'Open Fix the login throttle' });
    await user.click(
      within(list()).getByRole('button', {
        name: 'Actions for Fix the login throttle',
      }),
    );
    await user.click(await screen.findByRole('menuitem', { name: 'Delete' }));

    expect(calls.some((call) => call.method === 'DELETE')).toBe(false);
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('There is no undo.');

    await user.click(within(dialog).getByRole('button', { name: 'Delete' }));

    await waitFor(() => {
      expect(
        calls.some(
          (call) =>
            call.method === 'DELETE' && call.path === '/api/sessions/web%3A1',
        ),
      ).toBe(true);
    });
  });
});
