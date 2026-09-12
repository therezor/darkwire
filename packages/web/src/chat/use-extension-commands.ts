/**
 * The slash commands extensions contribute, for the composer.
 *
 * Its own query rather than a field on the extensions list, because the two
 * readers want different things at different rates: this one is read on every
 * `/` a person types and wants four strings, and the panel wants a row's state
 * and the sentence explaining it.
 *
 * Refetched on `tools.changed` like everything else the socket announces — see
 * `use-connection.ts` — so a command approved in another tab appears here
 * without a reload. An install with no extensions gets an empty array and one
 * cheap request, which is the same deal `GET /api/mcp` makes.
 */

import { useQuery } from '@tanstack/react-query';
import type { ExtensionCommand } from '@ghostwire/protocol';

import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';

const NONE: readonly ExtensionCommand[] = [];

export function useExtensionCommands(): readonly ExtensionCommand[] {
  const query = useQuery({
    queryKey: queryKeys.commands,
    queryFn: ({ signal }) => api.commands(signal),
  });
  // A stable empty array rather than `?? []`: this is a dependency of the
  // `useCallback` in `use-commands.ts`, and a fresh literal every render would
  // rebuild the whole command context on every keystroke.
  return query.data?.commands ?? NONE;
}
