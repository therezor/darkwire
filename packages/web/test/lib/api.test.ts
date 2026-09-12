/**
 * The REST client.
 *
 * Three things are worth pinning down, and they are the three that decide how
 * the rest of the UI behaves when the server is unhappy: that an error body is
 * turned into an `ApiError` carrying the server's own code, that a 401 is
 * distinguishable from everything else without string-matching a message, and
 * that a response which does not match its schema is a failure rather than a
 * cast. The third is the one that would otherwise surface as `undefined`
 * reaching a component three renders later.
 */

import {
  AuthSessionResponseSchema,
  StatusResponseSchema,
} from '@ghostwire/protocol';
import { describe, expect, it, vi } from 'vitest';
import { z } from 'zod';

import { ApiError, api, request, requestVoid } from '@/lib/api.js';

function respondWith(status: number, body: unknown): void {
  vi.stubGlobal('fetch', () =>
    Promise.resolve(
      new Response(status === 204 ? null : JSON.stringify(body), {
        status,
        headers: { 'content-type': 'application/json' },
      }),
    ),
  );
}

const STATUS = {
  version: '0.0.0',
  protocolVersion: 2,
  uptimeMs: 10,
  model: 'test-model',
  provider: 'ollama',
  configured: true,
  workspaceId: 'default',
  workspaceCount: 1,
  authEnabled: true,
  toolCount: 4,
  mcpServersConnected: 0,
  extensionsLoaded: 0,
};

describe('request', () => {
  it('parses a good response against its schema', async () => {
    respondWith(200, STATUS);
    await expect(
      request('/api/status', StatusResponseSchema),
    ).resolves.toMatchObject({
      model: 'test-model',
    });
  });

  it('turns the server error envelope into an ApiError with its code', async () => {
    respondWith(422, {
      error: {
        code: 'invalid_request',
        message: 'Bad path',
        details: { p: 1 },
      },
    });

    const error = await request('/api/files', z.unknown()).catch(
      (cause: unknown) => cause,
    );

    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({
      status: 422,
      code: 'invalid_request',
      message: 'Bad path',
    });
    expect((error as ApiError).details).toEqual({ p: 1 });
  });

  it('marks a 401 as unauthenticated, which is a state rather than a failure', async () => {
    respondWith(401, {
      error: { code: 'unauthorized', message: 'No session' },
    });

    const error = (await api.me().catch((cause: unknown) => cause)) as ApiError;

    expect(error.isUnauthenticated).toBe(true);
  });

  it('survives a body that never went through the error serialiser', async () => {
    // A reverse proxy answering 502 with HTML — the case that produces an
    // unhelpful `SyntaxError: Unexpected token <` in most clients.
    vi.stubGlobal('fetch', () =>
      Promise.resolve(
        new Response('<html>bad gateway</html>', { status: 502 }),
      ),
    );

    const error = (await api
      .status()
      .catch((cause: unknown) => cause)) as ApiError;

    expect(error).toBeInstanceOf(ApiError);
    expect(error.status).toBe(502);
    expect(error.code).toBe('http_error');
  });

  it('refuses a 200 whose shape is wrong', async () => {
    respondWith(200, { authenticated: 'yes' });

    const error = (await request(
      '/api/auth/me',
      AuthSessionResponseSchema,
    ).catch((cause: unknown) => cause)) as ApiError;

    expect(error.code).toBe('invalid_response');
  });

  it('sends the session cookie and a JSON body only when there is one', async () => {
    const fetchSpy = vi.fn<
      (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>
    >(() => Promise.resolve(new Response(null, { status: 204 })));
    vi.stubGlobal('fetch', fetchSpy);

    await requestVoid('/api/auth/logout', { method: 'POST' });

    const init = fetchSpy.mock.calls[0]?.[1];
    expect(init?.credentials).toBe('same-origin');
    expect(init?.body).toBeUndefined();
    expect(init?.headers).toBeUndefined();
  });

  it('serialises a body and its content type together', async () => {
    const fetchSpy = vi.fn<
      (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>
    >(() =>
      Promise.resolve(
        new Response(JSON.stringify({ ok: true, expiresAtMs: 1 }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      ),
    );
    vi.stubGlobal('fetch', fetchSpy);

    await api.login('ghost', 'hunter2');

    const init = fetchSpy.mock.calls[0]?.[1];
    expect(init?.body).toBe(
      JSON.stringify({ username: 'ghost', password: 'hunter2' }),
    );
    expect(init?.headers).toEqual({ 'content-type': 'application/json' });
  });

  it('encodes a session key that contains a channel prefix', async () => {
    const fetchSpy = vi.fn<
      (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>
    >(() =>
      Promise.resolve(
        new Response(
          JSON.stringify({ sessionKey: 'telegram:44', messages: [] }),
          {
            status: 200,
            headers: { 'content-type': 'application/json' },
          },
        ),
      ),
    );
    vi.stubGlobal('fetch', fetchSpy);

    await api.messages('telegram:44');

    // `telegram:44` unencoded would be read as a scheme by some proxies.
    expect(fetchSpy.mock.calls[0]?.[0]).toBe(
      '/api/sessions/telegram%3A44/messages',
    );
  });

  it('drops undefined query parameters rather than sending the string', async () => {
    const fetchSpy = vi.fn<
      (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>
    >(() =>
      Promise.resolve(
        new Response(JSON.stringify({ path: '', entries: [] }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      ),
    );
    vi.stubGlobal('fetch', fetchSpy);

    await request('/api/files', z.unknown(), {
      query: { path: 'docs', cursor: undefined },
    });

    expect(fetchSpy.mock.calls[0]?.[0]).toBe('/api/files?path=docs');
  });
});
