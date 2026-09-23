/**
 * `/model`, from the keypress to the request that goes out.
 *
 * `commands.test.ts` covers the table against a plain object, which is where
 * the refusals live. What it cannot see is the half `use-commands.ts`
 * assembles: **which agent** the write lands on. That answer comes from
 * `useAgentChoice`, and the two inputs it reads — the session's stored binding
 * and the browser's remembered preference — differ for exactly the
 * conversations a person is most likely to type `/model` in.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ConfigSchema, type ClientMessage } from '@darkwire/protocol';

import { Providers } from '@/app/providers.js';
import { createAppRouter } from '@/app/router.js';
import {
  stubApi,
  testQueryClient,
  type RecordedRequest,
} from '@testkit/render.js';
import { AGENTS, STATUS } from '@testkit/fixtures.js';

const SESSION = 'web:1';

class ControlledSocket {
  static readonly opened: ControlledSocket[] = [];
  readonly sent: ClientMessage[] = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  constructor(readonly url: string) {
    ControlledSocket.opened.push(this);
  }
  send(data: string): void {
    this.sent.push(JSON.parse(data) as ClientMessage);
  }
  close(): void {
    // Nothing to tear down; the test owns the instance list.
  }
}

const CONFIG = ConfigSchema.parse({
  providers: { ollama: { type: 'ollama', apiBase: 'http://x/v1' } },
  agents: {
    list: {
      default: { label: 'Default', model: 'test-model', provider: 'ollama' },
      researcher: {
        label: 'Researcher',
        model: 'pinned-research-model',
        provider: 'lmstudio',
      },
    },
  },
});

const SETTINGS = {
  config: CONFIG,
  credentialsPresent: {},
  channels: [],
  warnings: [],
};

const MODELS = {
  models: [
    { id: 'test-model', providerId: 'ollama' },
    { id: 'chosen-model', providerId: 'ollama' },
  ],
  errors: {},
};

function baseRoutes(): Record<string, [number, unknown]> {
  return {
    '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
    '/api/setup': [200, { required: false }],
    '/api/status': [200, STATUS],
    '/api/agents': [200, AGENTS],
    '/api/sessions': [200, { sessions: [], total: 0 }],
    '/api/notifications': [
      200,
      { notifications: [], unreadCount: 0, total: 0 },
    ],
    '/api/sessions/web%3A1/messages': [
      200,
      { sessionKey: SESSION, messages: [] },
    ],
    '/api/models': [200, MODELS],
    'GET /api/settings': [200, SETTINGS],
    'PATCH /api/settings': [200, SETTINGS],
  };
}

function mount(): void {
  const router = createAppRouter();
  router.update({
    history: createMemoryHistory({
      initialEntries: [`/sessions/${encodeURIComponent(SESSION)}`],
    }),
  });
  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );
}

async function connect(): Promise<void> {
  await waitFor(() => {
    expect(ControlledSocket.opened.length).toBeGreaterThan(0);
  });
  const instance = ControlledSocket.opened.at(-1);
  instance?.onopen?.();
  instance?.onmessage?.({
    data: JSON.stringify({
      type: 'connected',
      workspaceId: 'default',
      protocolVersion: 2,
      sessionKey: SESSION,
      serverTimeMs: Date.now(),
      lastSeq: 0,
    }),
  });
}

const patched = (calls: readonly RecordedRequest[]): RecordedRequest[] =>
  calls.filter(
    (call) => call.method === 'PATCH' && call.path === '/api/settings',
  );

describe('/model in the composer', () => {
  beforeEach(() => {
    ControlledSocket.opened.length = 0;
    vi.stubGlobal('WebSocket', ControlledSocket);
    window.localStorage.clear();
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it('writes the model onto the agent the conversation is bound to', async () => {
    const calls = stubApi({
      ...baseRoutes(),
      // The conversation has a row, and it is bound to `researcher` — not to
      // whatever this browser last picked in the sidebar.
      '/api/sessions/web%3A1': [
        200,
        {
          key: SESSION,
          title: '',
          origin: 'web',
          createdAtMs: 1,
          updatedAtMs: 1,
          messageCount: 2,
          workspaceId: 'default',
          agentId: 'researcher',
        },
      ],
    });

    mount();
    await connect();

    const box = await waitFor(() => {
      const found = document.querySelector('textarea');
      expect(found).not.toBeNull();
      return found!;
    });

    // Let the session-detail query land, so the binding is known before the
    // command runs. The race where it has not is the next case.
    await waitFor(() => {
      expect(calls.some((call) => call.path === '/api/sessions/web%3A1')).toBe(
        true,
      );
    });

    await userEvent.type(box, '/model chosen-model');
    await userEvent.keyboard('{Escape}');
    await userEvent.keyboard('{Enter}');

    await waitFor(() => {
      expect(patched(calls)).toHaveLength(1);
    });

    const body = patched(calls)[0]?.body as {
      agents: { list: Record<string, { model: string }> };
    };
    expect(Object.keys(body.agents.list)).toEqual(['researcher']);
    expect(body.agents.list.researcher?.model).toBe('chosen-model');
  });
});
