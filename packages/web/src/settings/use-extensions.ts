/**
 * The extensions list, and the two writes that change it.
 *
 * Two queries rather than one, for the reason `use-mcp.ts` gives: the settings
 * tree is what an operator configured and changes only when they save, and this
 * is what came of it. "Never approved" is not something to write into
 * `config.json` — an approval is a statement about the bytes on disk right now.
 *
 * Approve and revoke answer with the whole list rather than one row, and the
 * hook writes that answer straight into the cache. Loading an extension can
 * move another row — an id it shadows, a tool name it takes — so a refetch would
 * be a second request for something the first already told us.
 *
 * The list is also refetched on `tools.changed`, which an extension load already
 * produces through `ToolRegistry.subscribe` — see `use-connection.ts`. So a row
 * settles by itself when someone approves the same extension from a terminal.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import type { ExtensionListResponse } from '@ghostwire/protocol';
import type { UseQueryResult } from '@tanstack/react-query';

import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { toast } from '@/components/ui/toast.js';

export function useExtensions(): UseQueryResult<ExtensionListResponse> {
  return useQuery({
    queryKey: queryKeys.extensions,
    queryFn: ({ signal }) => api.extensions(signal),
  });
}

export function useApproveExtension(): {
  readonly approve: (id: string) => void;
  readonly revoke: (id: string) => void;
  readonly pending: boolean;
} {
  const queryClient = useQueryClient();

  const settle = async (answer: ExtensionListResponse): Promise<void> => {
    queryClient.setQueryData(queryKeys.extensions, answer);
    // The rest of the app, because loading an extension changes more than this
    // panel: its tools reach `GET /api/tools`, its commands reach the composer,
    // and `GET /api/status` counts it.
    await queryClient.invalidateQueries();
  };

  const approve = useMutation<ExtensionListResponse, Error, string>({
    mutationFn: (id) => api.approveExtension(id),
    onSuccess: async (answer, id) => {
      await settle(answer);
      const row = answer.extensions.find((one) => one.id === id);
      // The row's own state, not a fixed sentence: approving an extension whose
      // `activate` throws is a *successful* approval of something that then
      // failed, and a toast reading "Loaded" over a row reading "Failed" is the
      // kind of disagreement an operator stops trusting the screen over.
      if (row?.state === 'ready') {
        toast.success(
          'Extension approved',
          'It is loaded. Grant its tools per agent in Settings → Tools.',
        );
      } else {
        toast.warning(
          'Extension approved, and did not load',
          row?.lastError ?? 'See the row for what went wrong.',
        );
      }
    },
    onError: (error) => {
      toast.error('Could not approve the extension', error.message);
    },
  });

  const revoke = useMutation<ExtensionListResponse, Error, string>({
    mutationFn: (id) => api.revokeExtension(id),
    onSuccess: async (answer) => {
      await settle(answer);
      toast.success(
        'Approval withdrawn',
        'The files are still installed; nothing from it is loaded.',
      );
    },
    onError: (error) => {
      toast.error('Could not withdraw the approval', error.message);
    },
  });

  return {
    approve: (id) => {
      approve.mutate(id);
    },
    revoke: (id) => {
      revoke.mutate(id);
    },
    pending: approve.isPending || revoke.isPending,
  };
}
