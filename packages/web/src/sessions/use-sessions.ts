/**
 * The sessions screen's reads and writes.
 *
 * Its own file rather than more of the sidebar, and that is the point of the
 * change: renaming and deleting a conversation lived inline in `sidebar.tsx`,
 * so the management page could not reuse them and would have been a second
 * implementation of both — with its own toasts, its own invalidations, and its
 * own answer to what happens when you delete the conversation you are reading.
 * There is now one of each, and the sidebar calls them too.
 *
 * **The listing is paged on the server**, unlike every other list in this app.
 * The others hold a config tree that arrives whole, so `filterRows` over what is
 * already in memory is both correct and faster than a request. Sessions are a
 * SQLite table that a five-minute cron job appends to all day: a filter applied
 * to the first page would search the newest 25 conversations and quietly report
 * nothing for the one from last month, which is worse than no search at all.
 *
 * So the search, the sort and the page all go to `GET /api/sessions`, and the
 * mode is `offset` rather than `cursor` for the reason `cursor.ts` gives — this
 * reader jumps to a page and acts on a row rather than walking the list.
 */

import {
  keepPreviousData,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query';
import type { UseQueryResult } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';

import type { SessionListResponse, SessionSummary } from '@ghostwire/protocol';

import type { MutationHandle } from '@/components/crud/mutation.js';
import { PAGE_SIZE } from '@/components/crud/use-pagination.js';
import { toast } from '@/components/ui/toast.js';
import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';

/** Which column the sessions list is ordered by. Mirrors the server's `SessionOrderBy`. */
export type SessionSortKey = 'updated' | 'created' | 'title';

interface SessionQuery {
  readonly page: number;
  readonly q: string;
  readonly sort: SessionSortKey;
  readonly desc: boolean;
}

/**
 * One page of conversations.
 *
 * `placeholderData` holds the previous page on screen while the next is in
 * flight. Without it every keystroke in the search box blanks the list and the
 * page collapses to the height of a loading sentence, which on a full page is
 * most of a screen jumping under the cursor.
 */
export function useSessionPage(
  query: SessionQuery,
): UseQueryResult<SessionListResponse> {
  return useQuery({
    queryKey: queryKeys.sessionPage(query),
    queryFn: ({ signal }) =>
      api.sessions({
        limit: PAGE_SIZE,
        offset: (query.page - 1) * PAGE_SIZE,
        q: query.q,
        sort: query.sort,
        desc: query.desc,
        signal,
      }),
    placeholderData: keepPreviousData,
  });
}

/**
 * Refreshes every view of the session list at once.
 *
 * By prefix, not by key: the sidebar, the management page and each of its pages
 * are separate entries under `['sessions']`, and a rename that updated only the
 * one the operator was looking at would leave the old title in the column beside
 * it.
 */
function useRefreshSessions(): () => void {
  const queryClient = useQueryClient();
  return () => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.sessions() });
  };
}

export function useRenameSession(): MutationHandle<
  { readonly key: string; readonly title: string },
  SessionSummary
> {
  const { t } = useTranslation();
  const refresh = useRefreshSessions();

  const mutation = useMutation({
    mutationFn: ({ key, title }: { key: string; title: string }) =>
      api.renameSession(key, title),
    onSuccess: (session: SessionSummary) => {
      refresh();
      return session;
    },
    onError: () => {
      toast.error(t('sessions.renameFailed'));
    },
  });

  return {
    mutate: (input, options) => {
      mutation.mutate(input, {
        ...(options?.onSuccess === undefined
          ? {}
          : { onSuccess: options.onSuccess }),
      });
    },
    pending: mutation.isPending,
  };
}

export function useDeleteSession(): MutationHandle<string> {
  const { t } = useTranslation();
  const refresh = useRefreshSessions();

  const mutation = useMutation({
    mutationFn: (key: string) => api.deleteSession(key),
    onSuccess: () => {
      refresh();
    },
    onError: () => {
      toast.error(t('sessions.deleteFailed'));
    },
  });

  return {
    mutate: (key, options) => {
      mutation.mutate(key, {
        ...(options?.onSuccess === undefined
          ? {}
          : { onSuccess: options.onSuccess }),
      });
    },
    pending: mutation.isPending,
  };
}
