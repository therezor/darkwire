/**
 * A signed URL for one workspace file, kept valid while it is on screen.
 *
 * A token lasts minutes and a conversation stays open for hours, so the URL is
 * renewed a minute before it lapses. A browser that throttled the timer in a
 * background tab still ends up with a dead URL, so an `<img>` that fails to
 * load past its expiry asks for a fresh one through `renew`.
 */

import { type UseQueryResult, useQuery } from '@tanstack/react-query';

import type { SignedUrl } from '@darkwire/protocol';

import { ApiError, api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';

/** How long before expiry the URL is renewed. */
const RENEW_BEFORE_MS = 60_000;

/** The shortest gap between two renewals, against a server clock far ahead. */
const MIN_RENEW_MS = 5_000;

export interface SignedFileUrl {
  readonly signed: UseQueryResult<SignedUrl>;
  /** The file is gone: deleted, or never in this workspace. */
  readonly missing: boolean;
  /** For an `<img>` that failed. Renews only a URL that has expired. */
  readonly renew: () => void;
}

export function useSignedUrl(workspace: string, path: string): SignedFileUrl {
  const signed = useQuery({
    queryKey: queryKeys.fileUrl(workspace, path),
    queryFn: ({ signal }) => api.signUrl(workspace, path, signal),
    gcTime: 0,
    refetchInterval: (query) => {
      const expiresAtMs = query.state.data?.expiresAtMs;
      if (expiresAtMs === undefined) return false;
      return Math.max(MIN_RENEW_MS, expiresAtMs - Date.now() - RENEW_BEFORE_MS);
    },
  });
  const missing =
    signed.error instanceof ApiError && signed.error.status === 404;
  const renew = (): void => {
    const expiresAtMs = signed.data?.expiresAtMs;
    if (expiresAtMs !== undefined && Date.now() >= expiresAtMs) {
      void signed.refetch();
    }
  };
  return { signed, missing, renew };
}
