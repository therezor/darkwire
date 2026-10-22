/**
 * The reads several screens share, written once.
 *
 * Each was spelled out at every caller, and the rules that matter (a session
 * with no row is a 404 that must not be retried) drifted between the copies.
 * Query dedupes by key, so a second caller costs nothing; what this saves is
 * the second definition.
 */

import { useQuery, type UseQueryResult } from '@tanstack/react-query';

import { api } from './api.js';
import { queryKeys } from './query.js';

type Answer<T extends (...args: never[]) => Promise<unknown>> = Awaited<
  ReturnType<T>
>;

/** Every agent, configured or built in. */
export function useAgentsQuery(
  options: { readonly enabled?: boolean } = {},
): UseQueryResult<Answer<typeof api.agents>> {
  return useQuery({
    queryKey: queryKeys.agents,
    queryFn: ({ signal }) => api.agents(signal),
    enabled: options.enabled ?? true,
  });
}

/** Every workspace. */
export function useWorkspacesQuery(): UseQueryResult<
  Answer<typeof api.workspaces>
> {
  return useQuery({
    queryKey: queryKeys.workspaces,
    queryFn: ({ signal }) => api.workspaces(signal),
  });
}

/**
 * A session's stored row.
 *
 * A conversation nobody has spoken in has no row, so this 404s until the first
 * turn lands. That is the answer, not a failure, so it is never retried.
 */
export function useSessionRow(
  sessionKey: string | undefined,
): UseQueryResult<Answer<typeof api.session>> {
  return useQuery({
    queryKey: queryKeys.session(sessionKey ?? ''),
    queryFn: ({ signal }) => api.session(sessionKey ?? '', signal),
    enabled: sessionKey !== undefined,
    retry: false,
  });
}
