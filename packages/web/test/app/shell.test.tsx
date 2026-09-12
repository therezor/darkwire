/**
 * The shell, mounted over the real router and the real Query client.
 *
 * A shell test that stubbed the router would assert that a layout renders,
 * which is not where shells go wrong. What goes wrong is the wiring: a
 * navigation that remounts the sidebar, a drawer that traps a phone user, a
 * connection badge that never announces, a route that renders nothing when its
 * query is a 401. So this drives the real thing through `fetch`.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ConfigSchema } from '@ghostwire/protocol';

import { Providers } from '@/app/providers.js';
import { createAppRouter } from '@/app/router.js';
import { useTurnStore } from '@/state/turn.js';
import {
  stubApi,
  testQueryClient,
  type RecordedRequest,
  type StubRoute,
} from '@testkit/render.js';
import { STATUS } from '@testkit/fixtures.js';

/** What `POST /api/settings/reload` answers with: the settings it now serves. */
const SETTINGS = { config: ConfigSchema.parse({}), credentialsPresent: {} };

const SESSIONS = {
  sessions: [
    {
      key: 'web:1',
      title: 'First session',
      messageCount: 2,
      createdAtMs: 1,
      updatedAtMs: 2,
      origin: 'web',
    },
  ],
  total: 1,
};

const MESSAGES = {
  sessionKey: 'web:7',
  messages: [
    {
      id: 'm1',
      sessionKey: 'web:7',
      seq: 1,
      createdAtMs: 1,
      turnId: 't1',
      message: {
        role: 'user',
        content: [{ type: 'text', text: 'a stored question' }],
      },
    },
  ],
};

function renderApp(
  initial = '/',
  overrides: Record<string, StubRoute> = {},
): {
  readonly user: ReturnType<typeof userEvent.setup>;
  readonly calls: RecordedRequest[];
} {
  // `stubApi` rather than `stubFetch` for its request log: "New session writes
  // nothing" is an assertion about what was *not* sent.
  const calls = stubApi({
    '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
    // Claimed: the setup overlay mounts above the login one and would
    // otherwise be deciding whether to open on an unstubbed request.
    '/api/setup': [200, { required: false }],
    '/api/status': [200, STATUS],
    '/api/sessions': [200, SESSIONS],
    '/api/notifications': [
      200,
      {
        notifications: [
          {
            id: 'n1',
            title: 'A turn failed',
            body: 'The provider rate limited the request.',
            level: 'error',
            createdAtMs: Date.now(),
          },
        ],
        unreadCount: 3,
        total: 1,
      },
    ],
    '/api/sessions/web%3A7/messages': [200, MESSAGES],
    'POST /api/settings/reload': [200, SETTINGS],
    ...overrides,
  });

  const router = createAppRouter();
  router.update({
    history: createMemoryHistory({ initialEntries: [initial] }),
  });

  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );

  return { user: userEvent.setup(), calls };
}

describe('the shell', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('renders the sidebar and the route, without repeating the model in the header', async () => {
    renderApp();

    // The chat route on an empty session is the welcome screen.
    expect(
      await screen.findByRole('heading', { name: 'Ready when you are.' }),
    ).toBeInTheDocument();

    // The provider and the model are named on the welcome card and on each
    // turn. The header used to print them a third time, which is a third place
    // to keep in sync and the first one to go stale.
    const header = screen.getByRole('banner');
    expect(within(header).queryByText(/test-model/u)).not.toBeInTheDocument();

    // "New session" replaced the Chat nav link, which navigated nowhere: it
    // dropped `?session=` from a route that reads the store rather than the
    // URL, and the next message put the key straight back.
    const sidebar = screen.getByRole('complementary', { name: 'Sidebar' });
    expect(
      within(sidebar).getByRole('button', { name: 'New session' }),
    ).toBeInTheDocument();
    expect(
      within(sidebar).queryByRole('link', { name: /^Chat$/ }),
    ).not.toBeInTheDocument();
  });

  it('lists sessions in the sidebar and unread notifications in the header', async () => {
    renderApp();

    expect(await screen.findByText('First session')).toBeInTheDocument();
    // The count is in the bell's accessible name rather than drawn as a badge:
    // "3" beside an icon is not a sentence a screen reader can use.
    expect(
      await screen.findByRole('button', { name: 'Notifications, 3 unread' }),
    ).toBeInTheDocument();
  });

  /**
   * The column is a shortlist, not the archive. It used to take whatever the
   * server's default page was — fifty — which read as the whole list while being
   * the newest fraction of it, and had no way to the rest.
   */
  it('asks for a shortlist of sessions, and offers a way to the rest', async () => {
    const { calls } = renderApp();

    await screen.findByText('First session');

    const listed = calls.find(
      (call) => call.method === 'GET' && call.path === '/api/sessions',
    );
    expect(listed?.query.get('limit')).toBe('30');
    // The way to the rest is a nav row rather than a footer under the list: the
    // list scrolls, so anything below it is below the fold on a short window.
    expect(screen.getByRole('link', { name: 'Sessions' })).toHaveAttribute(
      'href',
      '/sessions',
    );
  });

  /**
   * A delegated run is a step inside a conversation, not a conversation. Left
   * in, an agent that delegates three times a turn fills a thirty-row column
   * with rows nobody chose to open.
   *
   * Asserted as the request rather than as the absence of a row, because that is
   * the part that matters: filtering the answer would keep the rows out of the
   * column while letting them eat the budget that decides how far back it goes.
   * The store still lists every origin, and `/sessions` still shows them.
   */
  it('leaves delegated runs out of the shortlist, at the request', async () => {
    const { calls } = renderApp();

    await screen.findByText('First session');

    const listed = calls.find(
      (call) => call.method === 'GET' && call.path === '/api/sessions',
    );
    expect(listed?.query.get('excludeOrigin')).toBe('subagent');
  });

  it('opens recent notifications from the header, with a way to the full list', async () => {
    const { user } = renderApp();

    await user.click(
      await screen.findByRole('button', { name: /^Notifications/ }),
    );

    expect(await screen.findByText('A turn failed')).toBeInTheDocument();
    // The glance is not a replacement for the archive.
    expect(screen.getByRole('link', { name: 'See all' })).toHaveAttribute(
      'href',
      '/notifications',
    );
  });

  it('tells the unread notifications from the read ones', async () => {
    // The dot on the bell said *that* something was unread; the list could not
    // say *which*, because every row rendered identically. Two visual signals
    // carry it — a raised surface and a full-strength title, the same pair
    // `/notifications` uses — and neither reaches a screen reader, so the word
    // is on the row as well.
    const { user } = renderApp('/', {
      '/api/notifications': [
        200,
        {
          notifications: [
            {
              id: 'n1',
              title: 'Still unread',
              body: '',
              level: 'error',
              createdAtMs: Date.now(),
            },
            {
              id: 'n2',
              title: 'Already read',
              body: '',
              level: 'info',
              createdAtMs: Date.now(),
              readAtMs: Date.now(),
            },
          ],
          unreadCount: 1,
          total: 1,
        },
      ],
    });

    await user.click(
      await screen.findByRole('button', { name: /^Notifications/ }),
    );

    const unread = (await screen.findByText('Still unread')).closest(
      '.notification-mini',
    );
    const read = screen.getByText('Already read').closest('.notification-mini');
    expect(unread).toHaveClass('notification-mini--unread');
    expect(read).not.toHaveClass('notification-mini--unread');

    // The part that does not depend on seeing either signal.
    expect(
      within(unread as HTMLElement).getByText('Unread'),
    ).toBeInTheDocument();
    expect(
      within(read as HTMLElement).queryByText('Unread'),
    ).not.toBeInTheDocument();
  });

  it('marks a notification read when it is opened', async () => {
    // Opening one *is* reading it. `POST .../read` existed and nothing called
    // it, so the only way to clear the dot was Mark all read — which also
    // clears the ones the operator has not looked at yet.
    const { user, calls } = renderApp('/', {
      // A notification that names a session is the one that is a link —
      // and a link is what an operator clicks. One with nowhere to go is not
      // made into a button just so it can be dismissed.
      '/api/notifications': [
        200,
        {
          notifications: [
            {
              id: 'n1',
              title: 'A turn failed',
              body: 'The provider rate limited the request.',
              level: 'error',
              createdAtMs: Date.now(),
              sessionKey: 'web:7',
            },
          ],
          unreadCount: 1,
          total: 1,
        },
      ],
      'POST /api/notifications/n1/read': [
        200,
        {
          id: 'n1',
          title: 'A turn failed',
          body: '',
          level: 'error',
          createdAtMs: 1,
          readAtMs: 2,
        },
      ],
    });

    await user.click(
      await screen.findByRole('button', { name: /^Notifications/ }),
    );
    await user.click(
      await screen.findByRole('link', { name: /A turn failed/u }),
    );

    await waitFor(() => {
      expect(
        calls.some((call) => call.path === '/api/notifications/n1/read'),
      ).toBe(true);
    });
  });

  it('starts a session without saving an empty one', async () => {
    const { user, calls } = renderApp();

    const sidebar = await screen.findByRole('complementary', {
      name: 'Sidebar',
    });
    const before = calls.length;
    await user.click(
      within(sidebar).getByRole('button', { name: 'New session' }),
    );

    // The URL moves to a key minted on the client, so the click can navigate to
    // it — but nothing is written. A row created on the press is a row that
    // survives someone changing their mind, and the sidebar fills with empty
    // sessions. The agent loop creates it when the first message lands.
    await waitFor(() => {
      expect(useTurnStore.getState().sessionKey).toMatch(/^[0-9a-f-]{8,}/u);
    });
    expect(
      calls.slice(before).filter((call) => call.method === 'POST'),
    ).toEqual([]);
  });

  it('names a session rather than showing its key', async () => {
    renderApp();

    expect(await screen.findByText('First session')).toBeInTheDocument();
    // The complaint this fixes: a list of uuids is not a list of sessions.
    const sidebar = screen.getByRole('complementary', { name: 'Sidebar' });
    expect(within(sidebar).queryByText('web:1')).not.toBeInTheDocument();
  });

  it('navigates without unmounting the shell around it', async () => {
    const { user } = renderApp();

    const sidebar = await screen.findByRole('complementary', {
      name: 'Sidebar',
    });
    const header = screen.getByRole('banner');

    await user.click(within(sidebar).getByRole('link', { name: /Settings/ }));

    expect(
      await screen.findByRole('heading', { name: 'Settings' }),
    ).toBeInTheDocument();
    // The same nodes, not replacements: a remount here would drop the socket
    // that hangs off the shell.
    expect(screen.getByRole('banner')).toBe(header);
    expect(screen.getByRole('complementary', { name: 'Sidebar' })).toBe(
      sidebar,
    );
  });

  it('reports the socket state in a live region', async () => {
    renderApp();

    // The shell opens the socket on mount, so the badge starts at Connecting
    // rather than Offline — `test/setup.ts` supplies a socket that never opens.
    const status = await screen.findByRole('status');
    expect(status).toHaveTextContent('Connecting');

    useTurnStore.getState().setConnection('open');
    await waitFor(() => {
      expect(status).toHaveTextContent('Connected');
    });
  });

  it('reloads the server and then the page, from the status indicator', async () => {
    // jsdom's own `reload` is a not-implemented stub that logs to the console,
    // so the assertion has to own it rather than watch it.
    // `stubGlobal` rather than a spy: `location.reload` is unforgeable, so
    // `spyOn` cannot redefine it and the whole object has to be swapped.
    const reload = vi.fn();
    vi.stubGlobal('location', { href: globalThis.location.href, reload });

    const { user, calls } = renderApp();

    // The badge is the trigger: the reasons to reload are all read off the
    // indicator, so that is where the control belongs.
    await user.click(await screen.findByRole('button', { name: 'Connecting' }));
    await user.click(
      await screen.findByRole('menuitem', { name: 'Reload app' }),
    );

    await waitFor(() => {
      expect(reload).toHaveBeenCalledTimes(1);
    });
    // The server first. A page that came back on the same stale config would
    // look like the button did nothing.
    expect(calls.some((call) => call.path === '/api/settings/reload')).toBe(
      true,
    );
  });

  it('keeps the page when the server could not reload, and says why', async () => {
    const reload = vi.fn();
    vi.stubGlobal('location', { href: globalThis.location.href, reload });

    const { user } = renderApp('/', {
      'POST /api/settings/reload': [
        500,
        {
          error: {
            code: 'config_invalid',
            message: 'config.json is not valid JSON',
          },
        },
      ],
    });

    await user.click(await screen.findByRole('button', { name: 'Connecting' }));
    await user.click(
      await screen.findByRole('menuitem', { name: 'Reload app' }),
    );

    // The message is the point. A navigation here would wipe it after one
    // frame and leave the operator pressing a button that does nothing.
    expect(
      await screen.findByText('config.json is not valid JSON'),
    ).toBeInTheDocument();
    expect(reload).not.toHaveBeenCalled();
  });

  it('opens the sidebar in a dialog on a narrow screen, and closes it on navigation', async () => {
    const { user } = renderApp();

    await user.click(await screen.findByRole('button', { name: 'Open menu' }));

    const drawer = await screen.findByRole('dialog');
    await user.click(within(drawer).getByRole('link', { name: /Files/ }));

    await waitFor(() => {
      expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    });
    expect(
      await screen.findByRole('heading', { name: 'Files' }),
    ).toBeInTheDocument();
  });

  it('answers an unknown address with a way back', async () => {
    const { user } = renderApp('/does-not-exist');

    expect(
      await screen.findByRole('heading', { name: 'Not found' }),
    ).toBeInTheDocument();

    await user.click(screen.getByRole('link', { name: 'Back to the session' }));
    expect(
      await screen.findByRole('heading', { name: 'Ready when you are.' }),
    ).toBeInTheDocument();
  });

  it('validates ?session= rather than handing a component whatever was in the URL', async () => {
    renderApp('/?session=web%3A7');

    // The decoded key reached the history fetch, which is the only thing that
    // proves the router parsed it rather than passing the raw parameter along.
    expect(await screen.findByText('a stored question')).toBeInTheDocument();
  });

  /**
   * New session marks a conversation that is *unsaved*, not one that is merely
   * missing from the column.
   *
   * The two used to be the same thing, so "no row matches" stood in for it. They
   * stopped being the same thing the moment the column began excluding an
   * origin: a delegated run is opened from `/sessions` and can never appear in
   * these rows, so the proxy would light New session over a real transcript and
   * claim it as `aria-current` to a screen reader. `web:7` is the same case
   * without a subagent — a stored session this list does not carry.
   */
  it('does not call a stored conversation a new one, just because the column omits it', async () => {
    renderApp('/?session=web%3A7');

    expect(await screen.findByText('a stored question')).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: 'New session' }),
    ).not.toHaveAttribute('aria-current');
  });
});
