/**
 * The socket's lifecycle, hung off the shell.
 *
 * The shell is the router's root component, so it never remounts — which is the
 * whole reason the socket lives there rather than in the chat route. Navigating
 * to Settings and back must not drop a turn in flight, and a socket opened by
 * the route would do exactly that.
 *
 * What this adds on top of `connection.ts` is the part that needs React: the
 * Query cache. Several server events move fetched state, and none of them can
 * be discovered by polling. A turn starting or ending changes the session list,
 * a notification changes the sidebar badge, and `tools.changed` means an MCP
 * server or an extension moved under the settings panel's feet.
 *
 * One of them **writes** rather than invalidates. `context.usage` already
 * carries the numbers, so refetching to learn them would re-download the whole
 * system prompt, every tool definition and every windowed message — up to forty
 * times in one turn — to change four integers. It is patched straight into the
 * cache instead.
 */

import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import type { QueryClient } from '@tanstack/react-query';
import { useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';

import type { ContextResponse } from '@darkwire/protocol';

import { api } from '@/lib/api.js';
import { deliveredLive, queryKeys, refreshSessions } from '@/lib/query.js';
import { useTurnStore } from '@/state/turn.js';
import {
  closeConnection,
  onConnectionNotice,
  onServerMessage,
  onSignedOut,
  openConnection,
  resumeConnection,
  switchSession,
} from '@/lib/connection.js';
import { toast } from '@/components/ui/toast.js';
import { useWorkspace } from '@/workspaces/workspace-context.js';

/**
 * Fetches a conversation's stored history and merges it under what the socket
 * has built. Called directly rather than left to the chat route's effect,
 * which runs only when the response object changes, and an unchanged history
 * comes back as the same object.
 */
function mergeStoredHistory(
  queryClient: QueryClient,
  sessionKey: string,
): void {
  void queryClient
    .query({
      queryKey: queryKeys.messages(sessionKey),
      queryFn: ({ signal }) => api.messages(sessionKey, signal),
      staleTime: 0,
      retry: false,
    })
    .then((history) => {
      const state = useTurnStore.getState();
      if (state.sessionKey !== sessionKey) return;
      state.mergeHistory(
        history.messages,
        history.subagentRuns,
        history.failures,
      );
    })
    // A conversation nobody has spoken in has no row yet: a 404, and nothing
    // to merge.
    .catch(() => undefined);
}

/**
 * A turn this page never saw start holds only what arrived after it attached.
 * Once it ends, storage has the whole of it.
 */
function refillJoinedTurn(queryClient: QueryClient, turnId: string): void {
  const { sessionKey, transcript } = useTurnStore.getState();
  const turn = transcript.find(
    (item) => item.kind === 'turn' && item.id === turnId,
  );
  if (sessionKey !== undefined && turn?.kind === 'turn' && !turn.watched) {
    mergeStoredHistory(queryClient, sessionKey);
  }
}

export function useConnection(sessionKey: string | undefined): void {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const { adopt } = useWorkspace();
  const { t } = useTranslation();

  // What the transport has to say, in this app's words and on its toaster.
  useEffect(
    () =>
      onConnectionNotice((notice) => {
        switch (notice.kind) {
          case 'lost':
            toast({
              title: t('connection.lost'),
              description: t('connection.lostBody'),
              role: 'warning',
              durationMs: 4000,
            });
            return;
          case 'unreadable':
            toast.error(t('connection.unreadable'), notice.reason);
            return;
          case 'serverError':
            toast.error(t('connection.serverError'), notice.message);
            return;
          case 'tooLarge':
            toast.error(
              t('connection.tooLarge'),
              t('connection.tooLargeBody', {
                limit: Math.floor(notice.limitBytes / (1024 * 1024)),
              }),
            );
            return;
        }
      }),
    [t],
  );

  // The server is the authority on which workspace a session is in, and it says
  // so on `connected` and on every `session.status`. Following it is what keeps
  // the switcher from claiming one workspace while the conversation on screen
  // runs in another — which is exactly what opening a link to someone else's
  // session would otherwise do.
  const reported = useTurnStore((state) => state.workspaceId);
  useEffect(() => {
    if (reported !== undefined) adopt(reported);
  }, [reported, adopt]);

  // The session the first render asked for, held in a ref so the effect below
  // can use it without listing it as a dependency — a change to `sessionKey` is
  // a `session.switch` on the socket that is already open, not a reason to
  // close one and dial another.
  const initial = useRef(sessionKey);

  // Once, for the life of the shell.
  useEffect(() => {
    openConnection(initial.current);
    return closeConnection;
  }, []);

  // A switch detaches this tab from the old session's frames. A first message
  // sent just before it would otherwise reach the list only on a reload.
  const previous = useRef(sessionKey);
  useEffect(() => {
    if (sessionKey === undefined) return;
    switchSession(sessionKey);
    if (previous.current !== undefined && previous.current !== sessionKey) {
      refreshSessions(queryClient);
    }
    previous.current = sessionKey;
  }, [sessionKey, queryClient]);

  // A socket the server signed out waits for a login rather than redialling
  // into a refusal. Refetching `me` is what raises the login overlay, and `me`
  // answering again is the login it was waiting for.
  const me = useQuery({
    queryKey: queryKeys.me,
    queryFn: ({ signal }) => api.me(signal),
  });
  useEffect(
    () =>
      onSignedOut(() => {
        void queryClient.invalidateQueries({ queryKey: queryKeys.me });
      }),
    [queryClient],
  );
  useEffect(() => {
    if (me.dataUpdatedAt > 0) resumeConnection();
  }, [me.dataUpdatedAt]);

  useEffect(
    () =>
      onServerMessage((message) => {
        switch (message.type) {
          case 'turn.start':
            // The user message and the title are written before this frame,
            // so a first turn's row exists now. Waiting for `turn.end` missed
            // it whenever the reader switched away mid-turn.
            refreshSessions(queryClient);
            return;

          case 'turn.end':
            // Unscoped: `['sessions']` is the prefix every workspace-scoped key
            // starts with, so one invalidation refreshes whichever is showing.
            //
            // Except a transcript or a context already in the cache, which go
            // stale without a refetch. The socket kept both current, and
            // refetching them downloaded every page of the conversation once
            // per turn. One with nothing cached is still fetched: before a
            // conversation's first turn there was no row to answer it, and
            // `context.usage` patches a cached context rather than seed one.
            void queryClient.invalidateQueries({
              queryKey: queryKeys.sessions(),
              predicate: (query) =>
                !deliveredLive(query.queryKey) ||
                query.state.data === undefined,
            });
            void queryClient.invalidateQueries({
              queryKey: queryKeys.sessions(),
              predicate: (query) =>
                deliveredLive(query.queryKey) && query.state.data !== undefined,
              refetchType: 'none',
            });
            refillJoinedTurn(queryClient, message.turnId);
            return;

          case 'context.usage':
            // Patched into the cache rather than held beside it, so the strip
            // and the inspector cannot disagree: a bar reading 41k that opens a
            // panel reading 23k is worse than a bar that was late.
            //
            // Only the totals. `messages`, `tools` and `systemPrompt` are the
            // last fetch's and stay that way — `ContextBody` refetches when it
            // opens, which is the moment anyone looks at them.
            //
            // `previous === undefined` is a real case, not a guard for form's
            // sake: the socket mints a session key before any row exists, so
            // this route 404s on a conversation nobody has spoken in yet.
            // Seeding an entry here would put a bar under the composer of a
            // conversation that does not exist.
            // Both answers, the full one and the strip's totals.
            for (const key of [
              queryKeys.context(message.sessionKey),
              queryKeys.contextSummary(message.sessionKey),
            ]) {
              queryClient.setQueryData<ContextResponse>(key, (previous) =>
                previous === undefined
                  ? previous
                  : {
                      ...previous,
                      estimatedTokens: message.estimatedTokens,
                      contextWindowTokens: message.contextWindowTokens,
                      breakdown: message.breakdown,
                      measured: message.measured,
                    },
              );
            }
            return;

          case 'notification':
            void queryClient.invalidateQueries({
              queryKey: queryKeys.notifications,
            });
            // The badge is the record; the toast is for the one that arrives
            // while the user is looking at something else.
            toast({
              title: message.title,
              description: message.body,
              role: message.level === 'error' ? 'danger' : message.level,
            });
            return;

          case 'session.replay':
            // Incomplete: the store kept only what storage cannot describe,
            // and storage supplies the rest.
            if (!message.complete) {
              mergeStoredHistory(queryClient, message.sessionKey);
            }
            return;

          case 'session.truncated':
            // The one invalidation that is not a nicety: `mergeStoredHistory`
            // puts a fetched history *underneath* the live transcript, so a
            // cached response holding the deleted rows would resurrect them.
            void queryClient.invalidateQueries({
              queryKey: queryKeys.messages(message.sessionKey),
            });
            void queryClient.invalidateQueries({
              queryKey: queryKeys.sessions(),
            });
            return;

          case 'session.reset':
            // Removed rather than invalidated: a cached history merged under
            // the emptied transcript would bring the cleared turns back.
            queryClient.removeQueries({
              queryKey: queryKeys.messages(message.sessionKey),
            });
            void queryClient.invalidateQueries({
              queryKey: queryKeys.sessions(),
            });
            return;

          case 'session.compacted':
            // The server has already moved this socket. Following it in the
            // address bar is what puts the summary on screen.
            if (message.sessionKey === useTurnStore.getState().sessionKey) {
              void navigate({
                to: '/sessions/$sessionKey',
                params: { sessionKey: message.continuedKey },
              });
            }
            refreshSessions(queryClient);
            return;

          case 'tools.changed':
            void queryClient.invalidateQueries({ queryKey: queryKeys.tools });
            // The same frame is what a server connecting or dropping produces,
            // so the Extensions row settles by itself rather than by polling.
            void queryClient.invalidateQueries({ queryKey: queryKeys.mcp });
            return;

          default:
            return;
        }
      }),
    [navigate, queryClient],
  );
}
