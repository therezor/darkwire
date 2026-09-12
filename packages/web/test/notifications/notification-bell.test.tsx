/**
 * The header bell, closed.
 *
 * The assertion that matters is the accessible name. The unread count is drawn
 * as a dot — a numeral inside an icon-sized control is either unreadable or
 * bursts it — so the *only* place the number exists for a screen reader is the
 * button's label. If that regressed, the dot would look fine and the count
 * would have silently stopped being announced.
 *
 * The panel itself is asserted in `app/shell.test.tsx`: it contains a router
 * `Link`, and standing a second router up here to render one link would be more
 * scaffolding than the test is worth.
 */

import { screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { NotificationBell } from '@/notifications/notification-bell.js';
import {
  renderWithProviders,
  stubApi,
  type StubRoute,
} from '@testkit/render.js';

const NOTIFICATION = {
  id: 'n1',
  title: 'A turn failed',
  body: 'The provider rate limited the request.',
  level: 'error' as const,
  createdAtMs: Date.now(),
  readAtMs: undefined,
};

function mount(routes: Record<string, StubRoute> = {}): void {
  stubApi({
    '/api/notifications': [
      200,
      { notifications: [NOTIFICATION], unreadCount: 1, total: 1 },
    ],
    ...routes,
  });
  renderWithProviders(<NotificationBell />);
}

describe('the notification bell', () => {
  it('states the unread count in its accessible name', async () => {
    mount();

    expect(
      await screen.findByRole('button', { name: 'Notifications, 1 unread' }),
    ).toBeInTheDocument();
  });

  it('says nothing about a count when there is none', async () => {
    mount({
      '/api/notifications': [
        200,
        { notifications: [], unreadCount: 0, total: 0 },
      ],
    });

    expect(
      await screen.findByRole('button', { name: 'Notifications' }),
    ).toBeInTheDocument();
  });
});
