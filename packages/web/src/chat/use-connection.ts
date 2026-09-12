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
 * be discovered by polling — a turn ending changes the session list's message
 * counts, a notification changes the sidebar badge, and `tools.changed` means an
 * MCP server or an extension moved under the settings panel's feet.
 *
 * One of them **writes** rather than invalidates. `context.usage` already
 * carries the numbers, so refetching to learn them would re-download the whole
 * system prompt, every tool definition and every windowed message — up to forty
 * times in one turn — to change four integers. It is patched straight into the
 * cache instead.
 */

import { useQueryClient } from '@tanstack/react-query';
import { useEffect, useRef } from 'react';

import type { ContextResponse } from '@ghostwire/protocol';

import { queryKeys } from '@/lib/query.js';
import { useTurnStore } from '@/state/turn.js';
import {
  closeConnection,
  onServerMessage,
  openConnection,
  switchSession,
} from '@/lib/connection.js';
import { toast } from '@/components/ui/toast.js';
import { useWorkspace } from '@/workspaces/workspace-context.js';

export function useConnection(sessionKey: string | undefined): void {
  const queryClient = useQueryClient();
  const { adopt } = useWorkspace();

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

  useEffect(() => {
    if (sessionKey !== undefined) switchSession(sessionKey);
  }, [sessionKey]);

  useEffect(
    () =>
      onServerMessage((message) => {
        switch (message.type) {
          case 'turn.end':
            // Unscoped: `['sessions']` is the prefix every workspace-scoped key
            // starts with, so one invalidation refreshes whichever is showing.
            void queryClient.invalidateQueries({
              queryKey: queryKeys.sessions(),
            });
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
            queryClient.setQueryData<ContextResponse>(
              queryKeys.context(message.sessionKey),
              (previous) =>
                previous === undefined
                  ? previous
                  : {
                      ...previous,
                      estimatedTokens: message.estimatedTokens,
                      contextWindowTokens: message.contextWindowTokens,
                      breakdown: message.breakdown,
                    },
            );
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
            // Only the incomplete answer needs anything. The frame carries a
            // bare `messages` array — no `subagentRuns`, no `failures` — so the
            // rebuild it drives loses the pointers that let a finished
            // delegation be fetched back and the reason a failed turn failed.
            // The REST history carries all three, and `mergeStoredHistory` puts
            // them under whatever the socket has built since.
            if (!message.complete) {
              void queryClient.invalidateQueries({
                queryKey: queryKeys.messages(message.sessionKey),
              });
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
    [queryClient],
  );
}
