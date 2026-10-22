/**
 * The installed environments, and the two writes that change them.
 *
 * Both writes answer with the whole list and seed the query cache with it, so a
 * save needs no refetch to show its result. That is worth doing here rather than
 * invalidating, because the facts beside a definition are derived server-side —
 * which hardening it weakens, whether a restricted allow-list could be enforced
 * — and a list refetched a moment later would briefly show the old verdict
 * beside the new definition.
 *
 * Read fresh on every visit rather than cached across one: a definition edited
 * outside the browser must report its new state the moment it changes, and an
 * operator who just fixed one is the likeliest visitor.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import type { UseQueryResult } from '@tanstack/react-query';
import type {
  EnvironmentDefinition,
  EnvironmentListResponse,
} from '@darkwire/protocol';

import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { toast } from '@/components/ui/toast.js';

export function useEnvironments(): UseQueryResult<EnvironmentListResponse> {
  return useQuery({
    queryKey: queryKeys.environments,
    queryFn: ({ signal }) => api.environments(signal),
  });
}

export function useSaveEnvironment(): {
  readonly save: (
    definition: EnvironmentDefinition,
    options?: { readonly onSuccess?: () => void },
  ) => void;
  readonly saving: boolean;
  readonly error: Error | null;
} {
  const { t } = useTranslation();
  const queryClient = useQueryClient();

  const mutation = useMutation<
    EnvironmentListResponse,
    Error,
    EnvironmentDefinition
  >({
    mutationFn: (definition) => api.saveEnvironment(definition),
    onSuccess: (listed, definition) => {
      queryClient.setQueryData(queryKeys.environments, listed);
      toast.success(
        t('toasts.environments.saved'),
        t('toasts.environments.savedBody', { name: definition.name }),
      );
    },
    onError: (error) => {
      toast.error(t('toasts.environments.saveFailed'), error.message);
    },
  });

  return {
    save: (definition, options) => {
      mutation.mutate(definition, {
        ...(options?.onSuccess && { onSuccess: options.onSuccess }),
      });
    },
    saving: mutation.isPending,
    error: mutation.error,
  };
}

/**
 * Removing one.
 *
 * The toast says the consequence an operator will not have thought about: a
 * running container started from this definition is not stopped by the delete.
 * It is swept when it next goes idle, because its definition no longer resolves.
 */
export function useRemoveEnvironment(): {
  readonly remove: (
    name: string,
    options?: { readonly onSuccess?: () => void },
  ) => void;
  readonly removing: boolean;
} {
  const { t } = useTranslation();
  const queryClient = useQueryClient();

  const mutation = useMutation<EnvironmentListResponse, Error, string>({
    mutationFn: (name) => api.removeEnvironment(name),
    onSuccess: (listed) => {
      queryClient.setQueryData(queryKeys.environments, listed);
      toast.success(
        t('toasts.environments.removed'),
        t('toasts.environments.removedBody'),
      );
    },
    onError: (error) => {
      toast.error(t('toasts.environments.removeFailed'), error.message);
    },
  });

  return {
    remove: (name, options) => {
      mutation.mutate(name, {
        ...(options?.onSuccess && { onSuccess: options.onSuccess }),
      });
    },
    removing: mutation.isPending,
  };
}
