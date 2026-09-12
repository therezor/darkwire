/**
 * The MCP servers list, and the two writes that change it.
 *
 * Two queries rather than one, because the two halves answer different
 * questions and move at different rates. `GET /api/settings` is what an operator
 * configured and changes only when they save; `GET /api/mcp` is where each of
 * those servers actually is and changes on its own. Folding live state into the
 * settings response would mean writing "unreachable" into `config.json`.
 *
 * The status query is refetched on `tools.changed`, which is the frame a server
 * connecting or dropping already produces — see `use-connection.ts`. So the row
 * settles by itself without this module polling for it.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import type { McpStatusResponse } from '@ghostwire/protocol';
import type { UseQueryResult } from '@tanstack/react-query';

import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { toast } from '@/components/ui/toast.js';
import { toDeleteMcpPatch } from './mcp-form.js';

export function useMcpServers(): UseQueryResult<McpStatusResponse> {
  return useQuery({
    queryKey: queryKeys.mcp,
    queryFn: ({ signal }) => api.mcpServers(signal),
  });
}

/**
 * Deleting one.
 *
 * Its own mutation rather than `useSaveSettings` so the toast can say the part
 * an operator will not have thought about: the tools this server contributed
 * leave the registry with it, and any agent that had granted one keeps a row
 * saying so — see the agent editor, which shows it as "not installed" rather
 * than dropping the operator's opinion.
 */
export function useRemoveMcpServer(): {
  readonly remove: (
    serverId: string,
    options?: { readonly onSuccess?: () => void },
  ) => void;
  readonly removing: boolean;
} {
  const queryClient = useQueryClient();

  const mutation = useMutation<unknown, Error, string>({
    mutationFn: (serverId) => api.patchSettings(toDeleteMcpPatch(serverId)),
    onSuccess: async () => {
      await queryClient.invalidateQueries();
      toast.success(
        'MCP server removed',
        'Its tools are no longer offered to any agent.',
      );
    },
    onError: (error) => {
      toast.error('Could not remove the MCP server', error.message);
    },
  });

  return {
    remove: (serverId, options) => {
      mutation.mutate(serverId, {
        ...(options?.onSuccess && { onSuccess: options.onSuccess }),
      });
    },
    removing: mutation.isPending,
  };
}
