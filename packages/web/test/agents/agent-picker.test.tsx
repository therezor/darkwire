/**
 * The picker, and the two states it has that nothing else in the UI reports.
 *
 * An agent id is user-authored and lives in the settings file, so it can be
 * deleted between one page load and the next — and the picker holds two copies
 * of one that may already be gone: the session's binding, and this browser's
 * remembered preference. They are deliberately treated differently, which is
 * most of what is asserted here.
 *
 * The router is stubbed rather than mounted: the only thing the picker needs
 * from it is the "Manage agents" link, and a memory router around one control
 * is a page test wearing a component test's name.
 */

import { render, screen, waitFor } from '@testing-library/react';
import type { QueryClient } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Providers } from '@/app/providers.js';
import { AgentPicker } from '@/agents/agent-picker.js';
import { stubApi, testQueryClient, type StubRoute } from '@testkit/render.js';

vi.mock('@tanstack/react-router', () => ({
  Link: ({ children }: { readonly children: ReactNode }) => (
    <a href="/agents">{children}</a>
  ),
}));

const STORAGE_KEY = 'ghostai:agent';

/**
 * A full `SessionSummary`, because the client parses what it is handed.
 *
 * A partial body is a failed parse and an empty query, which reads in a test as
 * "the picker ignored the binding" — a much more interesting bug than the one
 * actually present.
 */
const session = (agentId: string) => ({
  key: 'web-1',
  title: 'A session',
  messageCount: 2,
  createdAtMs: 1,
  updatedAtMs: 2,
  origin: 'web',
  workspaceId: 'default',
  agentId,
});

const AGENTS = {
  agents: [
    { id: 'default', label: 'Default', model: 'llama3', provider: 'ollama' },
    { id: 'writer', label: 'Writer', model: 'llama3', provider: 'ollama' },
  ],
};

afterEach(() => {
  localStorage.clear();
});

function mount(
  routes: Record<string, StubRoute> = {},
  props: { readonly sessionKey?: string } = {},
): { readonly client: QueryClient } {
  stubApi({ '/api/agents': [200, AGENTS], ...routes });
  const client = testQueryClient();
  render(
    <Providers client={client}>
      <AgentPicker {...props} />
    </Providers>,
  );
  return { client };
}

describe('the agent picker', () => {
  it('names the agent the session is bound to', async () => {
    mount(
      { '/api/sessions/web-1': [200, session('writer')] },
      { sessionKey: 'web-1' },
    );

    expect(
      await screen.findByRole('button', { name: 'Agent: Writer' }),
    ).toBeInTheDocument();
  });

  it('marks a binding whose agent is no longer configured', async () => {
    // The trigger used to render the dead id as though it were an ordinary
    // label, and the menu's radio group matched nothing — so the only signal
    // that a session pointed at a deleted agent was an unchecked list.
    mount(
      { '/api/sessions/web-1': [200, session('reviewer')] },
      { sessionKey: 'web-1' },
    );

    expect(
      await screen.findByRole('button', {
        name: /reviewer — no longer configured/,
      }),
    ).toBeInTheDocument();
  });

  it('does not move a bound session on its own', async () => {
    // Moving a session is a real edit and belongs to the operator: the
    // binding is what re-creating the agent would restore.
    const calls = stubApi({
      '/api/agents': [200, AGENTS],
      '/api/sessions/web-1': [200, session('reviewer')],
    });
    render(
      <Providers client={testQueryClient()}>
        <AgentPicker sessionKey="web-1" />
      </Providers>,
    );

    await screen.findByRole('button', { name: /no longer configured/ });
    expect(calls.filter((call) => call.method === 'PATCH')).toEqual([]);
  });

  it('resets a remembered preference that names nothing', async () => {
    // Only ever corrected in the browser that did the deleting, otherwise:
    // every other tab and device keeps sending a dead id indefinitely.
    localStorage.setItem(STORAGE_KEY, 'reviewer');
    mount();

    await waitFor(() => {
      expect(localStorage.getItem(STORAGE_KEY)).toBe('default');
    });
  });

  it('leaves a remembered preference alone while the listing is in flight', async () => {
    // Every id looks missing before the list arrives, and a picker that reset
    // on each cold load would throw away a perfectly good preference.
    localStorage.setItem(STORAGE_KEY, 'writer');
    mount({ '/api/agents': () => new Promise(() => undefined) as never });

    await Promise.resolve();
    expect(localStorage.getItem(STORAGE_KEY)).toBe('writer');
  });

  it('adopts the binding of the conversation it opens', async () => {
    // `agent-context.tsx` has always described this, and nothing did it: the
    // only caller of `adopt` was the move the operator had just made, so every
    // other way of arriving at a bound conversation left the preference naming
    // a different agent — and the welcome card announced that agent's model.
    localStorage.setItem(STORAGE_KEY, 'default');
    mount(
      { '/api/sessions/web-1': [200, session('writer')] },
      { sessionKey: 'web-1' },
    );

    await waitFor(() => {
      expect(localStorage.getItem(STORAGE_KEY)).toBe('writer');
    });
  });

  it('does not adopt a binding whose agent is gone', async () => {
    // A binding outlives its agent on purpose. Adopting a dead one would carry
    // it off the row it belongs to and onto every conversation started after,
    // which is the opposite of the correction two cases above.
    localStorage.setItem(STORAGE_KEY, 'default');
    mount(
      { '/api/sessions/web-1': [200, session('reviewer')] },
      { sessionKey: 'web-1' },
    );

    await screen.findByRole('button', { name: /no longer configured/ });
    expect(localStorage.getItem(STORAGE_KEY)).toBe('default');
  });

  describe('a row whose agentId is empty', () => {
    // The column is nullable and the DTO takes any string, so `''` reaches the
    // client — and `agentForTurn` on the server reads it as *unbound* and runs
    // the turn on the id the frame carried. Reading it as a binding here put
    // the two on opposite sides of one row.
    it('is not a binding, so the choice is still this session\u2019s', async () => {
      localStorage.setItem(STORAGE_KEY, 'writer');
      mount(
        { '/api/sessions/web-1': [200, session('')] },
        { sessionKey: 'web-1' },
      );

      // The preference answers, rather than an empty label from the row.
      expect(
        await screen.findByRole('button', { name: 'Agent: Writer' }),
      ).toBeInTheDocument();
    });

    it('is not moved, because there is nothing to move', async () => {
      const calls = stubApi({
        '/api/agents': [200, AGENTS],
        '/api/sessions/web-1': [200, session('')],
      });
      render(
        <Providers client={testQueryClient()}>
          <AgentPicker sessionKey="web-1" />
        </Providers>,
      );

      await screen.findByRole('button', { name: /Agent:/ });
      expect(calls.filter((call) => call.method === 'PATCH')).toEqual([]);
      expect(localStorage.getItem(STORAGE_KEY)).toBeNull();
    });
  });

  it('keeps a remembered preference that still names an agent', async () => {
    localStorage.setItem(STORAGE_KEY, 'writer');
    mount();

    expect(
      await screen.findByRole('button', { name: 'Agent: Writer' }),
    ).toBeInTheDocument();
    expect(localStorage.getItem(STORAGE_KEY)).toBe('writer');
  });
});
