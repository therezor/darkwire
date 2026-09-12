/**
 * Rendering helpers.
 *
 * `renderWithProviders` mounts the same provider stack `main.tsx` does, minus
 * the router, so a component test exercises the real Query client and the real
 * tooltip provider rather than a simplified stand-in that behaves differently.
 *
 * The Query client is built per call with retries off: a test asserting an
 * error state should not wait for three exponential backoffs first.
 */

import { QueryClient } from '@tanstack/react-query';
import { render, type RenderResult } from '@testing-library/react';
import type { ReactElement } from 'react';
import { vi } from 'vitest';

import { Providers } from '@/app/providers.js';

export function testQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0, staleTime: 0 },
      mutations: { retry: false },
    },
  });
}

export function renderWithProviders(
  ui: ReactElement,
  client: QueryClient = testQueryClient(),
): RenderResult & {
  readonly client: QueryClient;
  /** `rerender`, with the providers still around it. */
  readonly update: (next: ReactElement) => void;
} {
  const result = render(<Providers client={client}>{ui}</Providers>);
  return Object.assign(result, {
    client,
    // RTL's own `rerender` replaces the *whole* tree, so passing the bare
    // component swaps the provider stack out from under it — which unmounts
    // and remounts the subject, silently resetting every piece of state a test
    // about "what happens on the next frame" is trying to observe.
    update: (next: ReactElement) => {
      result.rerender(<Providers client={client}>{next}</Providers>);
    },
  });
}

/**
 * A `fetch` that answers from a table of `path → [status, body]`.
 *
 * A hand-written mock per test ends up asserting the mock; this asserts the
 * component, and an unlisted path is a loud failure rather than a hang.
 */
export function urlOf(input: RequestInfo | URL): string {
  if (typeof input === 'string') return input;
  return input instanceof URL ? input.href : input.url;
}

/** One request the stub answered, in the shape an assertion wants to read. */
export interface RecordedRequest {
  readonly method: string;
  readonly path: string;
  readonly query: URLSearchParams;
  /** Parsed when the body was JSON; the raw value otherwise. */
  readonly body: unknown;
}

export type StubRoute =
  [number, unknown] | ((request: RecordedRequest) => [number, unknown]);

/**
 * `stubFetch` with a method, a body and a memory.
 *
 * The panels in Settings and Files are mostly *writes*, and a write is only
 * half-tested by what the screen says afterwards: the other half is what went
 * over the wire — that a settings patch carried the one section its panel owns,
 * that a credential went out as a `PUT` and came back as nothing. The returned
 * array is that half.
 *
 * Keys are `"PATCH /api/settings"`, or a bare path to answer any method.
 */
export function stubApi(routes: Record<string, StubRoute>): RecordedRequest[] {
  const calls: RecordedRequest[] = [];

  vi.stubGlobal('fetch', (input: RequestInfo | URL, init?: RequestInit) => {
    const url = urlOf(input);
    const [path = url, search = ''] = url.split('?');
    const method = (init?.method ?? 'GET').toUpperCase();

    const record: RecordedRequest = {
      method,
      path,
      query: new URLSearchParams(search),
      body: parseBody(init?.body),
    };
    calls.push(record);

    const match = routes[`${method} ${path}`] ?? routes[path];
    if (match === undefined) {
      return Promise.reject(new Error(`Unstubbed request: ${method} ${url}`));
    }

    const [status, payload] =
      typeof match === 'function' ? match(record) : match;
    return Promise.resolve(
      new Response(status === 204 ? null : JSON.stringify(payload), {
        status,
        headers: { 'content-type': 'application/json' },
      }),
    );
  });

  return calls;
}

function parseBody(body: BodyInit | null | undefined): unknown {
  if (typeof body !== 'string') return body;
  try {
    return JSON.parse(body) as unknown;
  } catch {
    return body;
  }
}

export function stubFetch(routes: Record<string, [number, unknown]>): void {
  vi.stubGlobal('fetch', (input: RequestInfo | URL) => {
    const url = urlOf(input);
    const path = url.split('?')[0] ?? url;
    const match = routes[path];

    if (match === undefined) {
      return Promise.reject(new Error(`Unstubbed request: ${url}`));
    }

    const [status, body] = match;
    return Promise.resolve(
      new Response(status === 204 ? null : JSON.stringify(body), {
        status,
        headers: { 'content-type': 'application/json' },
      }),
    );
  });
}
