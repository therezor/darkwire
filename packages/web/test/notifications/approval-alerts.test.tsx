/**
 * The desktop notification for an approval nobody is looking at.
 *
 * jsdom has no `Notification`, so a fake stands in, and the page's focus is
 * whatever each case says it is. The rule under test is narrow on purpose: one
 * notification per call, only while the page is out of sight, only when this
 * browser was asked and said yes.
 */

import { QueryClient } from '@tanstack/react-query';
import {
  Outlet,
  RouterProvider,
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
} from '@tanstack/react-router';
import { act, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { PendingApproval } from '@darkwire/protocol';

import { Providers } from '@/app/providers.js';
import { queryKeys } from '@/lib/query.js';
import {
  notificationsSupported,
  useApprovalAlertPreference,
  useApprovalAlerts,
} from '@/notifications/approval-alerts.js';
import { useApprovalsStore } from '@/state/approvals.js';

class FakeNotification {
  static permission: NotificationPermission = 'granted';
  static made: FakeNotification[] = [];
  static refuse = false;

  onclick: (() => void) | null = null;
  closed = false;

  constructor(
    readonly title: string,
    readonly options: NotificationOptions,
  ) {
    if (FakeNotification.refuse) throw new TypeError('Illegal constructor');
    FakeNotification.made.push(this);
  }

  close(): void {
    this.closed = true;
  }
}

function approval(sessionKey: string, callId = 'c1'): PendingApproval {
  return { sessionKey, callId, name: 'exec', expiresAtMs: 9 };
}

function waiting(...approvals: PendingApproval[]): void {
  act(() => {
    useApprovalsStore.getState().receive(approvals);
  });
}

/** Whether the person is looking at this page. */
function looking(visible: boolean): void {
  vi.spyOn(document, 'visibilityState', 'get').mockReturnValue(
    visible ? 'visible' : 'hidden',
  );
  vi.spyOn(document, 'hasFocus').mockReturnValue(visible);
}

/** Mounts the hook under a router, and answers where that router is. */
async function mount(): Promise<() => string> {
  const root = createRootRoute({
    component: function Root() {
      useApprovalAlerts();
      return (
        <main>
          <Outlet />
        </main>
      );
    },
  });
  const router = createRouter({
    routeTree: root.addChildren([
      createRoute({ getParentRoute: () => root, path: '/' }),
      createRoute({
        getParentRoute: () => root,
        path: '/sessions/$sessionKey',
      }),
    ]),
    history: createMemoryHistory({ initialEntries: ['/'] }),
  });
  // A client that keeps what it is given: the test one drops an unobserved
  // query at once, and the sidebar's list is what names the session.
  const client = new QueryClient();
  client.setQueryData(queryKeys.sessions(), {
    sessions: [
      {
        key: 'web:1',
        title: 'Fix the login',
        messageCount: 1,
        createdAtMs: 0,
        updatedAtMs: 0,
      },
      {
        key: 'web:2',
        title: '',
        messageCount: 0,
        createdAtMs: 0,
        updatedAtMs: 0,
      },
    ],
    total: 2,
  });
  render(
    <Providers client={client}>
      <RouterProvider router={router} />
    </Providers>,
  );
  // The router renders a tick late, and the hook lives in its root.
  await screen.findByRole('main');
  return () => router.state.location.pathname;
}

beforeEach(() => {
  FakeNotification.permission = 'granted';
  FakeNotification.made = [];
  FakeNotification.refuse = false;
  vi.stubGlobal('Notification', FakeNotification);
  vi.stubGlobal('isSecureContext', true);
  useApprovalAlertPreference.setState({ enabled: true });
  looking(false);
});

afterEach(() => {
  useApprovalAlertPreference.setState({ enabled: false });
});

describe('a desktop notification for a waiting approval', () => {
  it('names the tool and the conversation while the page is out of sight', async () => {
    await mount();

    waiting(approval('web:1'));

    expect(FakeNotification.made).toHaveLength(1);
    expect(FakeNotification.made[0]).toMatchObject({
      title: 'exec is waiting for approval',
      options: { body: 'Fix the login', tag: 'web:1 c1' },
    });
  });

  it('calls a conversation with no title a new session', async () => {
    await mount();

    waiting(approval('web:2'), approval('web:3'));

    expect(FakeNotification.made.map((made) => made.options.body)).toEqual([
      'New session',
      'New session',
    ]);
  });

  it('notifies once per call, however many lists name it', async () => {
    await mount();

    waiting(approval('web:1'));
    waiting(approval('web:1'), approval('web:1', 'c2'));

    expect(FakeNotification.made.map((made) => made.options.tag)).toEqual([
      'web:1 c1',
      'web:1 c2',
    ]);
  });

  it('stays quiet for a call that arrived while the page had focus, even after it is left', async () => {
    looking(true);
    await mount();
    waiting(approval('web:1'));

    looking(false);
    waiting(approval('web:1'));

    expect(FakeNotification.made).toEqual([]);
  });

  it('stays quiet when nobody switched it on', async () => {
    useApprovalAlertPreference.setState({ enabled: false });
    await mount();

    waiting(approval('web:1'));

    expect(FakeNotification.made).toEqual([]);
  });

  it('stays quiet when the browser has not granted it', async () => {
    FakeNotification.permission = 'default';
    await mount();

    waiting(approval('web:1'));

    expect(FakeNotification.made).toEqual([]);
  });

  it('stays quiet on a page the browser will not let notify', async () => {
    vi.stubGlobal('isSecureContext', false);
    await mount();

    waiting(approval('web:1'));

    expect(FakeNotification.made).toEqual([]);
  });

  it('carries on without one where the browser refuses to build it', async () => {
    FakeNotification.refuse = true;
    await mount();

    waiting(approval('web:1'));
    FakeNotification.refuse = false;
    waiting();

    expect(FakeNotification.made).toEqual([]);
  });

  it('closes when the call is answered or expires', async () => {
    await mount();
    waiting(approval('web:1'), approval('web:1', 'c2'));

    waiting(approval('web:1', 'c2'));

    expect(FakeNotification.made.map((made) => made.closed)).toEqual([
      true,
      false,
    ]);
  });

  it('closes every one when it is switched off', async () => {
    await mount();
    waiting(approval('web:1'));

    act(() => {
      useApprovalAlertPreference.getState().setEnabled(false);
    });

    expect(FakeNotification.made[0]?.closed).toBe(true);
  });

  it('opens the conversation when clicked', async () => {
    const focus = vi.spyOn(window, 'focus').mockReturnValue(undefined);
    const pathname = await mount();
    waiting(approval('web:1'));

    act(() => {
      FakeNotification.made[0]?.onclick?.();
    });

    expect(focus).toHaveBeenCalled();
    await waitFor(() => {
      expect(pathname()).toBe('/sessions/web%3A1');
    });
    expect(FakeNotification.made[0]?.closed).toBe(true);
  });
});

describe('whether this browser can notify', () => {
  it('needs a secure page and the API', () => {
    expect(notificationsSupported()).toBe(true);

    vi.stubGlobal('isSecureContext', false);
    expect(notificationsSupported()).toBe(false);
  });

  it('is not possible without the API at all', () => {
    vi.unstubAllGlobals();
    vi.stubGlobal('isSecureContext', true);

    expect(notificationsSupported()).toBe(false);
  });
});

describe('the switch, as this browser keeps it', () => {
  it('is written to this browser on every change', () => {
    useApprovalAlertPreference.getState().setEnabled(true);
    expect(localStorage.getItem('darkwire.approvalNotifications')).toBe('on');

    useApprovalAlertPreference.getState().setEnabled(false);
    expect(localStorage.getItem('darkwire.approvalNotifications')).toBe('off');
  });

  it('still holds for this tab where it cannot be written', () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('quota');
    });

    useApprovalAlertPreference.getState().setEnabled(true);

    expect(useApprovalAlertPreference.getState().enabled).toBe(true);
  });

  it('starts from what this browser stored', async () => {
    localStorage.setItem('darkwire.approvalNotifications', 'on');
    vi.resetModules();

    const fresh = await import('@/notifications/approval-alerts.js');

    expect(fresh.useApprovalAlertPreference.getState().enabled).toBe(true);
  });

  it('starts off where storage cannot be read', async () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('denied');
    });
    vi.resetModules();

    const fresh = await import('@/notifications/approval-alerts.js');

    expect(fresh.useApprovalAlertPreference.getState().enabled).toBe(false);
  });
});
