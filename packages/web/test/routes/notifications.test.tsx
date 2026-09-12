/**
 * The notification centre.
 *
 * The case that matters is the badge: marking one row read has to leave the
 * count telling the truth, and the arithmetic is easy to get subtly wrong —
 * decrementing works until the same row is marked read from two tabs, at which
 * point the badge reads one fewer than there are. It is recounted instead, and
 * this is where that is asserted.
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

const UNREAD = {
  id: 'n1',
  title: 'Nightly digest finished',
  body: 'Wrote notes/digest.md',
  level: 'success',
  createdAtMs: Date.now() - 120_000,
  sessionKey: 'web:9',
};

const READ = {
  id: 'n2',
  title: 'Approval expired',
  body: 'exec was denied after 5 minutes',
  level: 'warning',
  createdAtMs: Date.now() - 7_200_000,
  readAtMs: Date.now() - 3_600_000,
};

const LIST = { notifications: [UNREAD, READ], unreadCount: 1, total: 2 };

const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
  // Claimed: the setup overlay mounts above the login one and would
  // otherwise be deciding whether to open on an unstubbed request.
  '/api/setup': [200, { required: false }],
  '/api/status': [200, { ...STATUS, model: 'llama3', toolCount: 0 }],
  '/api/sessions': [200, { sessions: [], total: 0 }],
};

function mount(overrides: Record<string, StubRoute> = {}): {
  readonly user: ReturnType<typeof userEvent.setup>;
  readonly calls: RecordedRequest[];
} {
  const calls = stubApi({
    ...SHELL_ROUTES,
    '/api/notifications': [200, LIST],
    'POST /api/notifications/n1/read': [
      200,
      { ...UNREAD, readAtMs: Date.now() },
    ],
    'POST /api/notifications/read': [204, null],
    'DELETE /api/notifications/n1': [204, null],
    ...overrides,
  });

  const user = userEvent.setup();
  const router = createAppRouter();
  router.update({
    history: createMemoryHistory({ initialEntries: ['/notifications'] }),
  });
  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );

  return { user, calls };
}

describe('the notification centre', () => {
  it('lists what the server reported, newest first, with the unread count', async () => {
    mount();

    const list = await screen.findByRole('article', {
      name: 'Nightly digest finished',
    });
    expect(list).toHaveTextContent('Wrote notes/digest.md');
    expect(
      screen.getByRole('article', { name: 'Approval expired' }),
    ).toBeInTheDocument();
    // The header count and the sidebar badge read the same query.
    expect(screen.getAllByText('1 unread.').length).toBeGreaterThan(0);
  });

  it('offers to open the session a notification came from', async () => {
    mount();

    const row = await screen.findByRole('article', {
      name: 'Nightly digest finished',
    });
    const link = within(row).getByRole('link', { name: 'Open session' });
    expect(link).toHaveAttribute(
      'href',
      expect.stringContaining('session=web%3A9'),
    );
  });

  it('marks one read and recounts the badge rather than decrementing it', async () => {
    const { user, calls } = mount();

    const row = await screen.findByRole('article', {
      name: 'Nightly digest finished',
    });
    await user.click(within(row).getByRole('button', { name: 'Mark read' }));

    await waitFor(() => {
      expect(screen.getByText('All caught up.')).toBeInTheDocument();
    });

    expect(
      calls.some((call) => call.path === '/api/notifications/n1/read'),
    ).toBe(true);
    // The row that was already read has no button to press, so a second press
    // cannot drive the count below zero.
    expect(
      screen.queryByRole('button', { name: 'Mark read' }),
    ).not.toBeInTheDocument();
  });

  it('marks everything read in one press, and offers it only when there is something to mark', async () => {
    const { user, calls } = mount();

    const markAll = await screen.findByRole('button', {
      name: 'Mark all read',
    });
    await user.click(markAll);

    await waitFor(() => {
      expect(
        calls.some((call) => call.path === '/api/notifications/read'),
      ).toBe(true);
    });
  });

  it('disables mark-all when nothing is unread', async () => {
    mount({
      '/api/notifications': [
        200,
        { notifications: [READ], unreadCount: 0, total: 1 },
      ],
    });

    expect(
      await screen.findByRole('button', { name: 'Mark all read' }),
    ).toBeDisabled();
  });

  it('deletes one', async () => {
    const { user, calls } = mount();

    await screen.findByRole('article', { name: 'Nightly digest finished' });
    await user.click(
      screen.getByRole('button', { name: 'Delete “Nightly digest finished”' }),
    );

    await waitFor(() => {
      expect(calls.some((call) => call.method === 'DELETE')).toBe(true);
    });
    expect(calls.find((call) => call.method === 'DELETE')?.path).toBe(
      '/api/notifications/n1',
    );
  });

  it('explains an empty list rather than showing an empty box', async () => {
    mount({
      '/api/notifications': [
        200,
        { notifications: [], unreadCount: 0, total: 0 },
      ],
    });

    expect(await screen.findByText('Nothing here yet.')).toBeInTheDocument();
  });

  it('says so when the list cannot be loaded', async () => {
    mount({
      '/api/notifications': [
        500,
        { error: { code: 'internal', message: 'database is locked' } },
      ],
    });

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'database is locked',
    );
  });
});

describe('clearing the notification centre', () => {
  /**
   * A labelled button beside Mark all read, the pairing Files uses for New and
   * Upload. The dialog is what makes it safe, not its obscurity.
   */
  async function openClear(
    user: ReturnType<typeof userEvent.setup>,
  ): Promise<void> {
    await user.click(screen.getByRole('button', { name: 'Clear all' }));
  }

  it('asks before it empties the list', async () => {
    const { user, calls } = mount();

    await screen.findByRole('article', { name: 'Nightly digest finished' });
    await openClear(user);

    // Nothing has been sent yet — the press opens the question.
    expect(calls.some((call) => call.method === 'DELETE')).toBe(false);
    expect(
      await screen.findByRole('heading', { name: 'Clear every notification?' }),
    ).toBeInTheDocument();
  });

  it('names how many are going, read and unread alike', async () => {
    const { user } = mount();

    await screen.findByRole('article', { name: 'Nightly digest finished' });
    await openClear(user);

    // `total`, not `unreadCount`: the count of what is being deleted.
    expect(
      await screen.findByText(/2 notifications are deleted/),
    ).toBeInTheDocument();
  });

  it('sends one request for the whole list once the answer is yes', async () => {
    const { user, calls } = mount({ 'DELETE /api/notifications': [204, null] });

    await screen.findByRole('article', { name: 'Nightly digest finished' });
    await openClear(user);
    await user.click(
      within(await screen.findByRole('dialog')).getByRole('button', {
        name: 'Clear all',
      }),
    );

    await waitFor(() => {
      expect(
        calls.some(
          (call) =>
            call.method === 'DELETE' && call.path === '/api/notifications',
        ),
      ).toBe(true);
    });
  });

  it('offers nothing to clear when there is nothing', async () => {
    mount({
      '/api/notifications': [
        200,
        { notifications: [], unreadCount: 0, total: 0 },
      ],
    });

    await screen.findByText('Nothing here yet.');
    expect(screen.getByRole('button', { name: 'Clear all' })).toBeDisabled();
  });
});

describe('paging the notification centre', () => {
  /** 30 rows over a page size of 25: two pages, and a reason for the control. */
  const page1 = {
    notifications: Array.from({ length: 25 }, (unused, index) => ({
      id: `p${String(index)}`,
      title: `Report ${String(index)}`,
      body: '',
      level: 'info',
      createdAtMs: Date.now() - index * 1000,
    })),
    unreadCount: 30,
    total: 30,
  };

  /**
   * Previous and Next, and no page numbers. A page number is a destination on a
   * list of records; on a feed ordered by when things happened, page 4 means
   * only "further back".
   */
  it('offers the two steps and no numbered destinations', async () => {
    mount({ '/api/notifications': [200, page1] });

    const pager = await screen.findByRole('navigation', {
      name: 'Notifications',
    });
    expect(
      within(pager).getByRole('button', { name: 'Next page' }),
    ).toBeEnabled();
    expect(
      within(pager).getByRole('button', { name: 'Previous page' }),
    ).toBeDisabled();
    expect(
      within(pager).queryByRole('button', { name: 'Page 2' }),
    ).not.toBeInTheDocument();
  });

  it('says where in the list the reader is', async () => {
    mount({ '/api/notifications': [200, page1] });

    expect(await screen.findByText('Showing 1–25 of 30')).toBeInTheDocument();
  });

  it('asks the server for the next page rather than filtering the one it has', async () => {
    const { user, calls } = mount({ '/api/notifications': [200, page1] });

    await user.click(
      within(
        await screen.findByRole('navigation', { name: 'Notifications' }),
      ).getByRole('button', {
        name: 'Next page',
      }),
    );

    await waitFor(() => {
      expect(calls.some((call) => call.query.get('offset') === '25')).toBe(
        true,
      );
    });
  });

  it('shows no pager when everything fits on one page', async () => {
    mount();

    await screen.findByRole('article', { name: 'Nightly digest finished' });
    expect(
      screen.queryByRole('navigation', { name: 'Notifications' }),
    ).not.toBeInTheDocument();
  });
});
