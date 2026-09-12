/**
 * The welcome card names the model that is about to answer.
 *
 * Worth its own file because the value it prints comes from three sources that
 * disagree by design: `/api/status` carries the install's model, `/api/agents`
 * carries each agent's after inheritance, and which of those agents applies is
 * either the session's stored binding or this browser's remembered preference.
 * The screen has been wrong about two of the three — it read `/api/status`,
 * which is wrong for any agent pinning a model, and then read the preference,
 * which is wrong for any conversation bound to another agent.
 */

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { Providers } from '@/app/providers.js';
import { stubFetch, testQueryClient } from '@testkit/render.js';
import { Welcome } from '@/chat/welcome.js';

/**
 * Every field `StatusResponseSchema` requires.
 *
 * Not a nicety: the client parses the response, so one missing field makes
 * `status.data` `undefined` and the card renders nothing at all — which looks
 * exactly like the bug this file is about. Worth knowing when one of these
 * assertions fails for a reason that has nothing to do with the model.
 */
const STATUS = {
  version: '0.0.0',
  protocolVersion: 2,
  uptimeMs: 1,
  model: 'install-default-model',
  provider: 'ollama',
  configured: true,
  workspaceId: 'default',
  workspaceCount: 1,
  authEnabled: false,
  toolCount: 3,
  mcpServersConnected: 0,
  extensionsLoaded: 0,
};

const AGENTS = {
  agents: [
    {
      id: 'default',
      label: 'Default',
      model: 'install-default-model',
      provider: 'ollama',
    },
    {
      id: 'researcher',
      label: 'Researcher',
      model: 'pinned-research-model',
      provider: 'lmstudio',
    },
  ],
};

function mount(sessionKey?: string): void {
  render(
    <Providers client={testQueryClient()}>
      <Welcome {...(sessionKey === undefined ? {} : { sessionKey })} />
    </Providers>,
  );
}

/**
 * A full `SessionSummary`, because the client parses what it is handed.
 *
 * A partial body is a failed parse and an empty query, which reads here as "the
 * card ignored the binding" — the very bug under test, arriving for the wrong
 * reason.
 */
const session = (agentId: string) => ({
  key: 'web-1',
  title: 'A cleared session',
  messageCount: 0,
  createdAtMs: 1,
  updatedAtMs: 2,
  origin: 'web',
  workspaceId: 'default',
  agentId,
});

/**
 * `localStorage`, stubbed per file rather than taken from the environment.
 *
 * The reasoning `workspaces.test.tsx` gives, and it applies identically here:
 * Node ships an experimental global that shadows jsdom's and is inert, so a
 * `setItem` succeeds and the `getItem` after it returns null — which reads as a
 * bug in the code under test rather than in the harness.
 */
beforeEach(() => {
  const entries = new Map<string, string>();
  vi.stubGlobal('localStorage', {
    get length() {
      return entries.size;
    },
    clear: () => {
      entries.clear();
    },
    getItem: (key: string) => entries.get(key) ?? null,
    key: (index: number) => [...entries.keys()][index] ?? null,
    removeItem: (key: string) => entries.delete(key),
    setItem: (key: string, value: string) => entries.set(key, value),
  });

  stubFetch({
    '/api/status': [200, STATUS],
    '/api/agents': [200, AGENTS],
    '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
    '/api/setup': [200, { required: false }],
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('the welcome card', () => {
  it('names the selected agent’s model, not the install’s', async () => {
    // The reported bug: a new conversation on an agent with its own model
    // announced the default. `agent-context` persists the choice here.
    localStorage.setItem('ghostai:agent', 'researcher');
    mount();

    expect(
      await screen.findByText('pinned-research-model'),
    ).toBeInTheDocument();
    expect(screen.getByText('lmstudio')).toBeInTheDocument();
    expect(screen.queryByText('install-default-model')).not.toBeInTheDocument();
  });

  it('names the bound conversation’s model, not this browser’s pick', async () => {
    // An empty transcript is not an unbound session: `/clear` leaves the
    // binding in place and so does a branch nobody has spoken in. The card read
    // the remembered preference, so on either of those it announced the model of
    // an agent that was not going to answer.
    localStorage.setItem('ghostai:agent', 'default');
    stubFetch({
      '/api/status': [200, STATUS],
      '/api/agents': [200, AGENTS],
      '/api/sessions/web-1': [200, session('researcher')],
      '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
      '/api/setup': [200, { required: false }],
    });
    mount('web-1');

    expect(
      await screen.findByText('pinned-research-model'),
    ).toBeInTheDocument();
    expect(screen.queryByText('install-default-model')).not.toBeInTheDocument();
  });

  it('names the default agent’s model when that is the selection', async () => {
    mount();

    expect(
      await screen.findByText('install-default-model'),
    ).toBeInTheDocument();
    expect(screen.getByText('ollama')).toBeInTheDocument();
  });

  it('falls back to the install when the stored agent no longer exists', async () => {
    // A `localStorage` id survives the agent being deleted, and a blank line here
    // reads as "no model configured" — a different and more alarming claim than
    // the truth, which is that this conversation will run on the default.
    localStorage.setItem('ghostai:agent', 'deleted-agent');
    mount();

    expect(
      await screen.findByText('install-default-model'),
    ).toBeInTheDocument();
  });

  it('names no model at all before the install has one', async () => {
    stubFetch({
      '/api/status': [
        200,
        { ...STATUS, configured: false, model: '', provider: '' },
      ],
      '/api/agents': [200, { agents: [] }],
      '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
      '/api/setup': [200, { required: false }],
    });
    mount();

    expect(
      await screen.findByRole('heading', { name: 'Ready when you are.' }),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(
        screen.queryByText('install-default-model'),
      ).not.toBeInTheDocument();
    });
  });
});
