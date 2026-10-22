/**
 * The task panel above the composer.
 *
 * What is worth asserting here is the seam rather than the markup: the list
 * comes from the store over REST, and the transcript is only the signal that it
 * has moved. A panel built the other way round would go on showing a plan that
 * `/tasks clear` had already emptied, and no amount of rendering assertions
 * would catch it.
 */

import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { TaskPanel, lastTodoCallId } from '@/tasks/task-panel.js';
import { useTurnStore } from '@/state/turn.js';
import type { Transcript, TurnItem } from '@/state/transcript.js';
import { seedTool } from '@/state/transcript/parts.js';
import { renderWithProviders, stubApi, urlOf } from '@testkit/render.js';

const PATH = '/api/sessions/web%3A1/tasks';

const PLAN = {
  tasks: [
    { text: 'Inspect auth', status: 'done' },
    { text: 'Update sessions', status: 'doing' },
    { text: 'Add tests', status: 'todo' },
  ],
};

function turn(parts: TurnItem['parts']): TurnItem {
  return {
    kind: 'turn',
    id: 't1',
    sessionKey: 'web:1',
    parts,
    stopReason: undefined,
    firstSeq: undefined,
    lastSeq: undefined,
    done: true,
    failure: undefined,
    authoritative: false,
  };
}

function setTranscript(transcript: Transcript): void {
  useTurnStore.getState().setTranscript(transcript);
}

beforeEach(() => {
  useTurnStore.getState().reset();
});

describe('the task panel', () => {
  it('renders nothing before a session exists', () => {
    stubApi({});
    const { container } = renderWithProviders(
      <TaskPanel sessionKey={undefined} />,
    );
    expect(container.querySelector('.tasks')).toBeNull();
  });

  it('renders nothing when the session has not started', async () => {
    stubApi({ [PATH]: [404, { error: { code: 'not_found' } }] });
    const { container } = renderWithProviders(<TaskPanel sessionKey="web:1" />);

    await waitFor(() => {
      expect(container.querySelector('.tasks')).toBeNull();
    });
  });

  it('renders nothing when there is no plan', async () => {
    stubApi({ [PATH]: [200, { tasks: [] }] });
    const { container } = renderWithProviders(<TaskPanel sessionKey="web:1" />);

    await waitFor(() => {
      expect(container.querySelector('.tasks')).toBeNull();
    });
  });

  it('renders one row per task, in the order the model wrote them', async () => {
    stubApi({ [PATH]: [200, PLAN] });
    const { container } = renderWithProviders(<TaskPanel sessionKey="web:1" />);

    expect(await screen.findByText('Inspect auth')).toBeInTheDocument();
    const rows = [...container.querySelectorAll('.tasks__item')].map(
      (row) => row.textContent,
    );
    expect(rows).toEqual(['Inspect auth', 'Update sessions', 'Add tests']);
  });

  it('marks the one task in hand apart from the rest', async () => {
    stubApi({ [PATH]: [200, PLAN] });
    const { container } = renderWithProviders(<TaskPanel sessionKey="web:1" />);

    await screen.findByText('Inspect auth');
    expect(container.querySelectorAll('.tasks__item--doing')).toHaveLength(1);
    expect(container.querySelectorAll('.tasks__item--done')).toHaveLength(1);
  });

  it('counts what is done beside the heading', async () => {
    stubApi({ [PATH]: [200, PLAN] });
    renderWithProviders(<TaskPanel sessionKey="web:1" />);

    expect(await screen.findByText('1/3')).toBeInTheDocument();
  });

  it('empties the plan by hand, and asks for it again', async () => {
    const calls = stubApi({
      [PATH]: [200, PLAN],
      [`DELETE ${PATH}`]: [204, undefined],
    });
    renderWithProviders(<TaskPanel sessionKey="web:1" />);
    await screen.findByText('Inspect auth');

    await userEvent
      .setup()
      .click(screen.getByRole('button', { name: 'Clear' }));

    await waitFor(() => {
      expect(
        calls.some((call) => call.method === 'DELETE' && call.path === PATH),
      ).toBe(true);
    });
  });

  it('refetches when a todo call lands mid-turn', async () => {
    const calls = stubApi({ [PATH]: [200, PLAN] });
    renderWithProviders(<TaskPanel sessionKey="web:1" />);
    await screen.findByText('Inspect auth');
    const before = calls.filter((call) => call.path === PATH).length;

    setTranscript([
      turn([seedTool('c1', 'todo', { tasks: [] }, 'safe', 'ok')]),
    ]);

    await waitFor(() => {
      expect(calls.filter((call) => call.path === PATH).length).toBeGreaterThan(
        before,
      );
    });
  });
});

describe('the signal the panel watches', () => {
  it('asks again when the plan moves while its first answer is on the way', async () => {
    // The first answer is still in flight when the second list is written.
    // A query with no data yet hands an invalidation the request it already
    // has, so without a cancel the older list lands last and stays.
    const answers: Array<(response: Response) => void> = [];
    let asked = 0;
    const json = (status: number, body: unknown): Response =>
      new Response(JSON.stringify(body), {
        status,
        headers: { 'content-type': 'application/json' },
      });
    vi.stubGlobal('fetch', (input: RequestInfo | URL) => {
      if (!urlOf(input).includes('/tasks')) {
        return Promise.reject(new Error('unstubbed'));
      }
      asked += 1;
      if (asked === 1) {
        return Promise.resolve(json(404, { error: { code: 'not_found' } }));
      }
      return new Promise<Response>((resolve) => {
        answers.push(resolve);
      });
    });
    renderWithProviders(<TaskPanel sessionKey="web:1" />);
    await waitFor(() => {
      expect(asked).toBe(1);
    });

    act(() => {
      setTranscript([
        turn([seedTool('c1', 'todo', { tasks: [] }, 'safe', 'ok')]),
      ]);
    });
    await waitFor(() => {
      expect(asked).toBe(2);
    });
    act(() => {
      setTranscript([
        turn([
          seedTool('c1', 'todo', { tasks: [] }, 'safe', 'ok'),
          seedTool('c2', 'todo', { tasks: [] }, 'safe', 'ok'),
        ]),
      ]);
    });
    await waitFor(() => {
      expect(asked).toBe(3);
    });

    act(() => {
      answers[1]?.(json(200, PLAN));
      answers[0]?.(
        json(200, { tasks: [{ text: 'Inspect auth', status: 'doing' }] }),
      );
    });
    expect(await screen.findByText('Update sessions')).toBeInTheDocument();
  });

  it('is empty when nothing has written a plan', () => {
    expect(lastTodoCallId([])).toBe('');
    expect(
      lastTodoCallId([turn([seedTool('c1', 'read', {}, 'safe', 'ok')])]),
    ).toBe('');
  });

  it('is the last successful call, so a refusal does not move it', () => {
    expect(
      lastTodoCallId([
        turn([
          seedTool('c1', 'todo', {}, 'safe', 'ok'),
          seedTool('c2', 'todo', {}, 'safe', 'error'),
        ]),
      ]),
    ).toBe('c1');
  });

  it('ignores a subagent, which runs its own list on its own session', () => {
    const nested = seedTool('c9', 'todo', {}, 'safe', 'ok');
    const delegation = seedTool('c1', 'researcher', {}, 'safe', 'ok', {
      agentId: 'researcher',
      label: 'Researcher',
      sessionKey: 'web:2',
      parts: [nested],
      model: 'qwen3',
      done: true,
      loaded: true,
      partial: false,
    });

    expect(lastTodoCallId([turn([delegation])])).toBe('');
  });
});
