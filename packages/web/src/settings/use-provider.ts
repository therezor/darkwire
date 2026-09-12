/**
 * Saving one provider, and asking whether it answers.
 *
 * Both live here rather than in `use-settings.ts` because a provider is the one
 * thing in the settings tree that is not only in the settings tree. Its
 * connection is config and its key is a vault entry, on two routes with two
 * different rules — and from the operator's side they are one endpoint with one
 * Save. `useSaveProvider` is what makes that true without the panel sequencing
 * two hooks and emitting two toasts for one press.
 *
 * The probe never gates the save. A save that has landed is a save, and an
 * endpoint that is unreachable right now is a normal state — a laptop closed, a
 * model server not started yet, a key that will be pasted in a minute. So the
 * write is reported successful the moment it completes and the check follows
 * it, arriving as a warning on a row that already holds what was typed. Refusing
 * to store a URL because nothing is listening at it yet would make the panel
 * unusable for exactly the case it is most needed in.
 */

import type { TFunction } from 'i18next';
import { useRef } from 'react';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import type {
  ConfigPatch,
  ProviderTestRequest,
  ProviderTestResponse,
} from '@ghostwire/protocol';

import { api } from '@/lib/api.js';
import { toast } from '@/components/ui/toast.js';
import { toDeleteProviderPatch } from './provider-form.js';
import { afterCredentialWrite, afterSettingsWrite } from './use-settings.js';

interface SaveProviderInput {
  readonly instanceId: string;
  /** The connection half — never contains a credential. */
  readonly patch: ConfigPatch;
  /** From `toCredentialValue`: `undefined` leaves the vault untouched. */
  readonly credential: string | null | undefined;
  /**
   * The connection to check once the write has landed, or `null` to skip it.
   *
   * Sent without an `apiKey` by every caller that has just saved one: the probe
   * should use what is now stored, which is also the only way to re-check a row
   * whose key the client has never held.
   */
  readonly test: ProviderTestRequest | null;
}

interface ProbeHandle {
  readonly result: ProviderTestResponse | null;
  readonly probing: boolean;
  readonly clear: () => void;
}

interface SaveProviderHandle extends ProbeHandle {
  readonly save: (
    input: SaveProviderInput,
    options?: {
      readonly onSuccess?: () => void;
      /**
       * The check's verdict, once it arrives — always after `onSuccess`.
       *
       * For a caller that will not be on screen to show `result` by the time it
       * lands. The Add dialog closes the moment the write succeeds, so its
       * warning has to go somewhere that outlives it.
       */
      readonly onProbe?: (result: ProviderTestResponse) => void;
    },
  ) => void;
  readonly saving: boolean;
}

interface TestProviderHandle extends ProbeHandle {
  readonly test: (request: ProviderTestRequest) => void;
}

/**
 * The probe, as a mutation that resolves rather than rejects.
 *
 * Every outcome it reports is a *result* — the route already answers "the key
 * was rejected" with a 200 and a reason — so the one remaining way to reject is
 * the hop to our own server failing. Folding that into the same shape keeps the
 * render to one branch instead of a result, an error and a pending flag that
 * can each mean "no answer".
 */
function useProbe(): {
  readonly run: (
    request: ProviderTestRequest,
    options?: { readonly onSettled?: (result: ProviderTestResponse) => void },
  ) => void;
  readonly result: ProviderTestResponse | null;
  readonly probing: boolean;
  readonly clear: () => void;
} {
  const mutation = useMutation<
    ProviderTestResponse,
    Error,
    ProviderTestRequest
  >({
    mutationFn: async (request) => {
      try {
        return await api.testProvider(request);
      } catch (error) {
        return {
          ok: false,
          models: [],
          reason: 'transport',
          message: `The server could not be asked — ${error instanceof Error ? error.message : String(error)}`,
        };
      }
    },
  });

  return {
    // `onSuccess` rather than `onSettled`: the mutation function above cannot
    // reject, so success is the only way this finishes.
    run: (request, options) => {
      mutation.mutate(request, {
        ...(options?.onSettled && { onSuccess: options.onSettled }),
      });
    },
    result: mutation.data ?? null,
    probing: mutation.isPending,
    clear: mutation.reset,
  };
}

export function useTestProvider(): TestProviderHandle {
  const probe = useProbe();
  return {
    test: (request) => {
      probe.run(request);
    },
    result: probe.result,
    probing: probe.probing,
    clear: probe.clear,
  };
}

export function useSaveProvider(): SaveProviderHandle {
  const queryClient = useQueryClient();
  const probe = useProbe();
  // The check is started from the mutation's own `onSuccess`, which cannot see
  // the per-call options the caller passed to `save`. A ref rather than state:
  // nothing renders from it, and setting state here would re-render the panel
  // between the save landing and the check starting.
  const onProbe = useRef<((result: ProviderTestResponse) => void) | undefined>(
    undefined,
  );

  const mutation = useMutation({
    mutationFn: async (input: SaveProviderInput) => {
      const settings = await api.patchSettings(input.patch);
      // After the instance exists, because the vault is keyed by instance id —
      // the same ordering the first-run wizard uses, and for the same reason.
      if (input.credential !== undefined) {
        await api.setCredential({
          namespace: 'providers',
          key: input.instanceId,
          value: input.credential,
        });
      }
      return settings;
    },
    onSuccess: (response, input) => {
      afterSettingsWrite(queryClient, response);
      if (input.credential !== undefined) afterCredentialWrite(queryClient);
      // One toast for one press. Composing `useSaveSettings` with
      // `useSaveCredential` would have stacked two, which reads as two saves.
      toast.success(
        'Provider saved',
        input.credential === undefined
          ? undefined
          : input.credential === null
            ? 'Its stored key was removed.'
            : 'The next turn will use the new key.',
      );
      if (input.test !== null) {
        probe.run(input.test, {
          ...(onProbe.current && { onSettled: onProbe.current }),
        });
      }
    },
    onError: (error: Error) => {
      // The server's own message: a refused patch says *which* field could not
      // be served, and "could not save" is none of that.
      toast.error('Could not save the provider', error.message);
    },
  });

  return {
    save: (input, options) => {
      onProbe.current = options?.onProbe;
      // The previous verdict describes the previous connection. Leaving it up
      // while the new one saves is the one moment it is actively misleading.
      probe.clear();
      mutation.mutate(input, {
        ...(options?.onSuccess && { onSuccess: options.onSuccess }),
      });
    },
    saving: mutation.isPending,
    result: probe.result,
    probing: probe.probing,
    clear: probe.clear,
  };
}

/**
 * Deleting an instance.
 *
 * Its own mutation rather than `useSaveSettings` so the toast can say what
 * happened to the key: the server drops the vault entry with the config entry,
 * and an operator who does not know that would reasonably assume a re-added
 * endpoint of the same name still has its credential. Shared by the list and
 * the editor, which both offer it.
 */
export function useRemoveProvider(): {
  readonly remove: (
    instanceId: string,
    options?: { readonly onSuccess?: () => void },
  ) => void;
  readonly removing: boolean;
} {
  const queryClient = useQueryClient();

  const mutation = useMutation<unknown, Error, string>({
    mutationFn: (instanceId) =>
      api.patchSettings(toDeleteProviderPatch(instanceId)),
    onSuccess: async () => {
      await queryClient.invalidateQueries();
      toast.success('Provider removed', 'Its saved key was deleted with it.');
    },
    onError: (error) => {
      toast.error('Could not remove the provider', error.message);
    },
  });

  return {
    remove: (instanceId, options) => {
      mutation.mutate(instanceId, {
        ...(options?.onSuccess && { onSuccess: options.onSuccess }),
      });
    },
    removing: mutation.isPending,
  };
}

/**
 * One line of prose for a probe result.
 *
 * Written from `reason`, not from `message`, wherever the reason is enough to
 * say something better: the classification is what `ghostai-providers` is sure
 * of, and it separates the two answers an operator actually came for — the key
 * is wrong, or nothing is there. `message` carries the detail underneath, and
 * for a transport fault it is already a full sentence naming the host and the
 * fault, so it is used as-is.
 */
export function describeProbe(
  result: ProviderTestResponse,
  saved: boolean,
  t: TFunction,
): string {
  const prefix = saved ? 'Saved — but ' : '';

  if (result.ok) {
    const count = result.models.length;
    return count === 0
      ? t('providers.reachableNoModels')
      : t('providers.reachable', { count });
  }

  switch (result.reason) {
    case 'auth':
      return `${prefix}the key was rejected. Check the API key.`;
    case 'timeout':
      return `${prefix}it did not answer in time.`;
    case 'model_not_found':
      return `${prefix}the endpoint answered without a model list. Check the API base.`;
    case 'unsupported':
      return result.message ?? 'This endpoint cannot be checked.';
    default:
      return `${prefix}${result.message ?? 'it could not be reached.'}`;
  }
}
