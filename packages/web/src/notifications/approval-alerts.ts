/**
 * A desktop notification for an approval nobody is looking at.
 *
 * Off until somebody switches it on, and per browser: whether a site may
 * notify is the browser's to grant, and it is asked for on that click. It
 * fires only while the tab is hidden or unfocused, because a person looking at
 * the page already has the card, the title and the icon. Once per call: a call
 * that arrived while the page had focus was seen, and does not notify later.
 */

import { useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { create } from 'zustand';

import type { PendingApproval, SessionListResponse } from '@darkwire/protocol';

import { lookingAtPage } from '@/lib/looking.js';
import { queryKeys } from '@/lib/query.js';
import { useApprovalsStore } from '@/state/approvals.js';

const STORAGE_KEY = 'darkwire.approvalNotifications';

/** Whether this page may notify at all. Browsers allow it on HTTPS and localhost only. */
export function notificationsSupported(): boolean {
  return window.isSecureContext && 'Notification' in window;
}

function stored(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === 'on';
  } catch {
    return false;
  }
}

function remember(enabled: boolean): void {
  try {
    localStorage.setItem(STORAGE_KEY, enabled ? 'on' : 'off');
  } catch {
    // A choice that cannot be persisted still holds for this tab.
  }
}

interface AlertPreference {
  readonly enabled: boolean;
  readonly setEnabled: (enabled: boolean) => void;
}

export const useApprovalAlertPreference = create<AlertPreference>()((set) => ({
  enabled: stored(),
  setEnabled: (enabled) => {
    remember(enabled);
    set({ enabled });
  },
}));

/** One call. A `call_id` is only unique within its conversation. */
function idOf(approval: PendingApproval): string {
  return `${approval.sessionKey} ${approval.callId}`;
}

export function useApprovalAlerts(): void {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const enabled = useApprovalAlertPreference((state) => state.enabled);
  const pending = useApprovalsStore((state) => state.pending);
  const seen = useRef(new Set<string>());
  const shown = useRef(new Map<string, Notification>());

  useEffect(() => {
    // An answered or expired call takes its notification with it, and
    // switching this off takes them all.
    const open = new Set(enabled ? pending.map(idOf) : []);
    for (const [id, notification] of shown.current) {
      if (!open.has(id)) {
        notification.close();
        shown.current.delete(id);
      }
    }

    for (const approval of pending) {
      const id = idOf(approval);
      if (seen.current.has(id)) continue;
      seen.current.add(id);
      if (
        !enabled ||
        lookingAtPage() ||
        !notificationsSupported() ||
        Notification.permission !== 'granted'
      ) {
        continue;
      }

      const session = queryClient
        .getQueryData<SessionListResponse>(queryKeys.sessions())
        ?.sessions.find((row) => row.key === approval.sessionKey);
      const body =
        session === undefined || session.title === ''
          ? t('sessions.untitled')
          : session.title;
      let notification: Notification;
      try {
        // The tag is the call, so three tabs on one install raise one.
        notification = new Notification(
          t('notifications.approvalTitle', { tool: approval.name }),
          { body, tag: id },
        );
      } catch {
        // A browser that only notifies through a service worker, which is
        // Chrome on Android. The title and the icon still say it.
        continue;
      }
      notification.onclick = () => {
        window.focus();
        void navigate({
          to: '/sessions/$sessionKey',
          params: { sessionKey: approval.sessionKey },
        });
        notification.close();
      };
      shown.current.set(id, notification);
    }
  }, [enabled, pending, navigate, queryClient, t]);
}
