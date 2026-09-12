/**
 * TanStack Query, configured once.
 *
 * Two settings carry the reasoning:
 *
 *  - **A 401 is never retried.** It is not a transient failure; it is the
 *    answer. Retrying it three times delays the login overlay by a second and
 *    burns three requests against the login rate limit for nothing.
 *  - **`refetchOnWindowFocus` is off.** The live surfaces here are driven by the
 *    WebSocket, not by polling — the socket transport pushes turn state, and
 *    refetching every REST query each time the tab regains focus would fight it.
 *
 * Query keys live here rather than beside their components so that a mutation in
 * one panel can invalidate what another rendered without importing it.
 */

import { QueryClient } from '@tanstack/react-query';

import { ApiError } from './api.js';

export const queryKeys = {
  me: ['auth', 'me'] as const,
  setup: ['setup'] as const,
  status: ['status'] as const,
  workspaces: ['workspaces'] as const,
  agents: ['agents'] as const,
  toolboxes: ['toolboxes'] as const,
  /** Live connection state, which is not in the settings tree. See `use-mcp.ts`. */
  mcp: ['mcp'] as const,
  /** Which extensions loaded, and why the rest did not. See `use-extensions.ts`. */
  extensions: ['extensions'] as const,
  /**
   * The slash commands extensions contribute.
   *
   * Its own key rather than a field on `extensions`, because the composer reads
   * it on every keystroke of a `/` and the panel does not: one of them wants a
   * cached list of four strings and the other wants a row's `lastError`.
   */
  commands: ['commands'] as const,
  /**
   * Every conversation, unscoped.
   *
   * It was once keyed by workspace, when the sidebar filtered by one. Nothing
   * filters now — a workspace says where a conversation's files are, not which
   * list it belongs in — so there is one list and one key.
   *
   * Still a function rather than a constant: this is the prefix `sessionPage`,
   * `session`, `messages`, `turns` and `context` all sit under, and every
   * caller invalidating the lot spells it `queryKeys.sessions()`.
   */
  sessions: () => ['sessions'] as const,
  /**
   * One page of the sessions management screen.
   *
   * Under the `['sessions']` prefix like everything else here, so the
   * invalidation `use-connection.ts` already fires after every turn refreshes
   * it. Every input to the request is *in* the key rather than beside it: two
   * searches are two different answers, and a shared key would serve the old
   * rows for the new query until the refetch landed.
   */
  sessionPage: (params: {
    readonly page: number;
    readonly q: string;
    readonly sort: string;
    readonly desc: boolean;
  }) => ['sessions', 'page', params] as const,
  session: (key: string) => ['sessions', key] as const,
  messages: (key: string) => ['sessions', key, 'messages'] as const,
  /**
   * Under the `['sessions']` prefix on purpose: `use-connection.ts` already
   * invalidates that after every turn, so a conversation's costs refresh as it
   * grows without a second subscription.
   */
  turns: (key: string) => ['sessions', key, 'turns'] as const,
  /** The bell's recent list, unpaged. */
  notifications: ['notifications'] as const,
  /**
   * One page of the notification centre.
   *
   * A key of its own rather than the bell's, because the two ask for different
   * things: the bell wants the newest few and the centre wants rows 26–50. It
   * still sits under the `['notifications']` prefix, so one invalidation after a
   * write refreshes the badge and the list together.
   */
  notificationPage: (page: number) =>
    ['notifications', 'page', { page }] as const,
  settings: ['settings'] as const,
  providers: ['providers'] as const,
  models: ['models'] as const,
  tools: ['tools'] as const,
  // The workspace comes *second*, before the path, and that placement is the
  // point. Two workspaces both contain `notes.md`, so a key of `['files', path]`
  // would serve one workspace's listing for the other the instant the switcher
  // moved — the single most likely bug in this half of the feature. Putting it
  // at index 1 also keeps `invalidateQueries({ queryKey: ['files', workspace] })`
  // meaningful.
  files: (workspace: string, path: string) =>
    ['files', workspace, path] as const,
  // Their own roots rather than `['files', 'text', …]`: invalidation matches by
  // prefix, so nesting them under `files` would make refreshing a directory
  // that happens to be named `text` drop every open file's buffer.
  fileText: (workspace: string, path: string) =>
    ['file-text', workspace, path] as const,
  fileUrl: (workspace: string, path: string) =>
    ['file-url', workspace, path] as const,
  context: (key: string) => ['sessions', key, 'context'] as const,
  automation: ['automation'] as const,
  automationJob: (id: string) => ['automation', id] as const,
  // Under the job's own key, so invalidating one job refreshes both the row and
  // its history — which is what a `notification` carrying a `jobId` wants.
  //
  // The page is *in* the key rather than beside it, because two pages of runs
  // are two different answers: a shared key would serve page 1's rows for
  // page 2 until the refetch landed, which is the same row twice on screen.
  automationRuns: (id: string, page = 1) =>
    ['automation', id, 'runs', { page }] as const,
};

export function createQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        retry: (failureCount, error) => {
          if (
            error instanceof ApiError &&
            (error.isUnauthenticated || error.status < 500)
          ) {
            return false;
          }
          return failureCount < 2;
        },
        staleTime: 30_000,
        refetchOnWindowFocus: false,
      },
      mutations: { retry: false },
    },
  });
}
