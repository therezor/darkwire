/**
 * The notification centre.
 *
 * Notifications are what the server says while nobody is looking — an
 * automation run that finished, an approval that expired, a channel that
 * dropped — so this panel is read-and-dismiss and has no way to create one. The
 * server has no route for that either, deliberately: a `POST /api/notifications`
 * would exist solely to let a client fabricate the server's own reports.
 *
 * "Read" is a timestamp rather than a flag, because the question a badge asks is
 * *when* did this stop being new. Marking one read writes the returned row into
 * the cache rather than refetching the list — the route answers with the updated
 * row for exactly that reason, so one item reconciles without replacing every
 * other row object on a list someone is in the middle of scrolling.
 *
 * A notification carrying a session key is a link into that conversation. That
 * is the whole point of the field: "the job you scheduled produced this" is only
 * useful if the answer is one press away.
 */

import {
  keepPreviousData,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query';
import { Link } from '@tanstack/react-router';
import { Bell, CheckCheck, Trash2 } from 'lucide-react';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type {
  Notification,
  NotificationListResponse,
} from '@ghostwire/protocol';

import { cn } from '@/lib/cn.js';
import { api } from '@/lib/api.js';
import { useFormat } from '@/lib/use-format.js';
import { queryKeys } from '@/lib/query.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { Pagination } from '@/components/crud/pagination.js';
import { PAGE_SIZE, usePagination } from '@/components/crud/use-pagination.js';
import { Button } from '@/components/ui/button.js';
import { toast } from '@/components/ui/toast.js';
import { LEVEL_CLASSES, LEVEL_ICONS } from '@/notifications/levels.js';

export function NotificationsRoute(): JSX.Element {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [confirmingClear, setConfirmingClear] = useState(false);

  // Page first, total after: the request needs a page number before there is a
  // response to count. Nothing here filters or sorts, so nothing sends the
  // reader back to page 1 — the key is constant.
  const page = usePagination({ resetOn: '' });

  const notifications = useQuery({
    queryKey: queryKeys.notificationPage(page.page),
    queryFn: ({ signal }) =>
      api.notifications({
        limit: PAGE_SIZE,
        offset: (page.page - 1) * PAGE_SIZE,
        signal,
      }),
    placeholderData: keepPreviousData,
  });

  const markRead = useMutation({
    mutationFn: (id: string) => api.readNotification(id),
    onSuccess: (updated) => {
      queryClient.setQueryData(
        queryKeys.notificationPage(page.page),
        (current: NotificationListResponse | undefined) =>
          current === undefined ? current : replaceRow(current, updated),
      );
    },
    onError: (error: Error) => {
      toast.error('Could not mark it read', error.message);
    },
  });

  const markAllRead = useMutation({
    mutationFn: () => api.readAllNotifications(),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.notifications });
    },
    onError: (error: Error) => {
      toast.error('Could not mark them read', error.message);
    },
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.deleteNotification(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.notifications });
    },
    onError: (error: Error) => {
      toast.error('Could not delete it', error.message);
    },
  });

  const removeAll = useMutation({
    mutationFn: () => api.deleteAllNotifications(),
    onSuccess: () => {
      setConfirmingClear(false);
      void queryClient.invalidateQueries({ queryKey: queryKeys.notifications });
    },
    onError: (error: Error) => {
      toast.error(t('notifications.clearFailed'), error.message);
    },
  });

  const unreadCount = notifications.data?.unreadCount ?? 0;
  const total = notifications.data?.total ?? 0;
  const pagination = page.withTotal(total);
  const rows = notifications.data?.notifications ?? [];
  // One instant for the whole list: two rows measured against two `Date.now()`
  // calls can disagree about which is older.
  const now = Date.now();

  return (
    <div className="stack page page--reading">
      {/* The title and the one routine action, and nothing else competing for
          that line. The status sentence used to sit inline here, which is the
          arrangement `data-list.css` calls "a slow-motion disaster" — a
          paragraph and a fixed-width box on one line have no arrangement that
          works at every width, and on a phone it pushed the controls onto a
          ragged second row. */}
      <div className="cluster page__header">
        <h1 className="page__title">{t('notifications.title')}</h1>
        <span className="spacer" />
        <Button
          variant="secondary"
          disabled={unreadCount === 0 || markAllRead.isPending}
          onClick={() => {
            markAllRead.mutate();
          }}
        >
          <CheckCheck />
          {t('common.markAllRead')}
        </Button>
      </div>

      {/* How much is here on the left, and the one act that changes all of it
          on the right — the same left-to-right split `.list-toolbar` uses for
          the filter and the sort, and for the same reason. What the count says
          is exactly what Clear all would remove, so the two belong on one line
          reading across to each other rather than stacked.

          Ghost, and the quietest control on the page. What makes it safe is the
          question in front of it, not its weight — and a page of alerts should
          not have its loudest element be the chrome. */}
      <div className="row">
        <p className="page__note">
          {unreadCount === 0
            ? t('notifications.allRead')
            : t('notifications.unreadCount', { count: unreadCount })}
        </p>
        <span className="spacer" />
        <Button
          variant="ghost"
          disabled={total === 0 || removeAll.isPending}
          onClick={() => {
            setConfirmingClear(true);
          }}
        >
          <Trash2 />
          {t('notifications.clearAll')}
        </Button>
      </div>

      {notifications.isPending && (
        <p className="page__note">{t('common.loading')}</p>
      )}
      {notifications.isError && (
        <p role="alert" className="page__error">
          {t('notifications.loadError', {
            message: notifications.error.message,
          })}
        </p>
      )}

      {notifications.isSuccess &&
        (rows.length === 0 ? (
          <div className="stack page__empty">
            <Bell />
            <p className="page__empty-title">{t('common.nothingHere')}</p>
            <p className="page__empty-note">{t('notifications.empty')}</p>
          </div>
        ) : (
          <ul className="stack notification-list">
            {rows.map((row) => (
              <li key={row.id}>
                <NotificationRow
                  notification={row}
                  now={now}
                  onRead={() => {
                    markRead.mutate(row.id);
                  }}
                  onDelete={() => {
                    remove.mutate(row.id);
                  }}
                />
              </li>
            ))}
          </ul>
        ))}

      {notifications.data !== undefined && (
        // Previous and Next only. A page number is a destination on a list of
        // records — "the agents starting with M are on page 3" — and means
        // nothing on a feed ordered by when things happened, where page 4 is
        // only "further back".
        <Pagination
          pagination={pagination}
          total={total}
          label={t('notifications.title')}
          numbered={false}
        />
      )}

      <ConfirmDialog
        open={confirmingClear}
        onOpenChange={setConfirmingClear}
        title={t('notifications.clearTitle')}
        description={t('notifications.clearHint', { count: total })}
        confirmLabel={t('notifications.clearAll')}
        pending={removeAll.isPending}
        onConfirm={() => {
          // Closed in `onSuccess`, so a failed clear leaves the question on
          // screen with its error rather than looking like it worked.
          removeAll.mutate();
        }}
      />
    </div>
  );
}

function NotificationRow({
  notification,
  now,
  onRead,
  onDelete,
}: {
  readonly notification: Notification;
  readonly now: number;
  readonly onRead: () => void;
  readonly onDelete: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const fmt = useFormat();
  const unread = notification.readAtMs === undefined;
  const Icon = LEVEL_ICONS[notification.level];

  return (
    <article
      aria-label={notification.title}
      className={cn('notification', unread && 'notification--unread')}
    >
      <Icon
        className={cn('notification__icon', LEVEL_CLASSES[notification.level])}
      />

      <div className="stack notification__body">
        <div className="cluster notification__head">
          <span className="notification__title">{notification.title}</span>
          <time
            dateTime={new Date(notification.createdAtMs).toISOString()}
            className="notification__time"
          >
            {fmt.relativeTime(notification.createdAtMs, now)}
          </time>
        </div>

        {notification.body !== '' && (
          <p className="notification__text">{notification.body}</p>
        )}

        <div className="cluster notification__actions">
          {notification.sessionKey !== undefined && (
            <Link
              to="/"
              search={{ session: notification.sessionKey }}
              className="notification__link"
            >
              {t('notifications.openSession')}
            </Link>
          )}
          {unread && (
            <Button variant="ghost" size="sm" onClick={onRead}>
              {t('common.markRead')}
            </Button>
          )}
        </div>
      </div>

      <Button
        variant="ghost"
        size="icon"
        aria-label={`Delete “${notification.title}”`}
        onClick={onDelete}
      >
        <Trash2 />
      </Button>
    </article>
  );
}

/** One row replaced, with the unread count recounted from what is now in hand. */
function replaceRow(
  current: NotificationListResponse,
  updated: Notification,
): NotificationListResponse {
  const notifications = current.notifications.map((row) =>
    row.id === updated.id ? updated : row,
  );
  return {
    ...current,
    notifications,
    // Recounted rather than decremented: two tabs marking the same row read
    // would otherwise drive the badge below what is really unread.
    unreadCount: notifications.filter((row) => row.readAtMs === undefined)
      .length,
  };
}
