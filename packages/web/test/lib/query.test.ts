/**
 * The Query client's defaults.
 *
 * The retry policy is the part worth pinning: retrying a 401 delays the login
 * overlay and spends three attempts against the login rate limit for an answer
 * that will not change, and retrying a 422 asks the server to reject the same
 * malformed request twice more. Only a server-side failure is worth another go.
 */

import { describe, expect, it } from 'vitest';

import { ApiError } from '@/lib/api.js';
import { createQueryClient, queryKeys } from '@/lib/query.js';

/** The client stores the resolved options, which is where the policy lands. */
function retryOf(): (failureCount: number, error: Error) => boolean {
  const client = createQueryClient();
  const retry = client.getDefaultOptions().queries?.retry;

  if (typeof retry !== 'function') {
    throw new Error('Expected a retry predicate');
  }
  return retry;
}

describe('the query client', () => {
  it('never retries an unauthenticated request', () => {
    expect(retryOf()(0, new ApiError(401, 'unauthorized', 'No session'))).toBe(
      false,
    );
  });

  it('never retries a request the server refused on its merits', () => {
    expect(retryOf()(0, new ApiError(422, 'invalid_request', 'Bad path'))).toBe(
      false,
    );
    expect(
      retryOf()(0, new ApiError(404, 'not_found', 'No such session')),
    ).toBe(false);
  });

  it('retries a server-side failure, twice', () => {
    const retry = retryOf();
    const error = new ApiError(503, 'unavailable', 'Restarting');

    expect(retry(0, error)).toBe(true);
    expect(retry(1, error)).toBe(true);
    expect(retry(2, error)).toBe(false);
  });

  it('retries a transport failure, which has no status at all', () => {
    expect(retryOf()(0, new TypeError('Failed to fetch'))).toBe(true);
  });

  it('does not refetch on window focus, because the socket is the live channel', () => {
    expect(
      createQueryClient().getDefaultOptions().queries?.refetchOnWindowFocus,
    ).toBe(false);
  });

  it('keys a session-scoped query by its session', () => {
    // Two sessions must not share a cache entry — the whole reason the keys are
    // functions rather than constants.
    expect(queryKeys.messages('web:1')).not.toEqual(
      queryKeys.messages('web:2'),
    );
  });
});
