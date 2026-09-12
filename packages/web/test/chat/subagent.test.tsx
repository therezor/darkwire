/**
 * A delegating tool card, driven directly.
 *
 * Every state worth asserting here is transient in a running app — the card
 * opens itself while a subagent works and the "working" label goes the instant
 * its `turn.end` arrives — which is exactly why they are asserted here rather
 * than end-to-end. `subagents.spec.ts` checks the durable settled state from a
 * browser; the wording an operator reads *while* a delegation is in flight is
 * covered here, where the state can be held still.
 */

import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import type { SubagentPart, ToolPart } from '@/state/transcript.js';
import { renderWithProviders, stubApi } from '@testkit/render.js';

import { ToolCard } from '@/chat/tool-card.js';

const RUN: SubagentPart = {
  agentId: 'researcher',
  label: 'Researcher',
  sessionKey: 'sub-1',
  parts: [],
  model: 'qwen3:8b',
  stopReason: undefined,
  usage: undefined,
  iterations: 0,
  elapsedMs: undefined,
  done: false,
  loaded: true,
  partial: false,
};

const DELEGATION: ToolPart = {
  kind: 'tool',
  id: 'c1',
  name: 'ask_researcher',
  args: { task: 'find how retries are configured' },
  risk: 'safe',
  status: 'running',
  elapsedMs: 0,
  durationMs: undefined,
  content: undefined,
  truncated: false,
  approval: undefined,
  notices: [],
  subagent: RUN,
};

function toolWith(
  tool: Partial<ToolPart>,
  run: Partial<SubagentPart> | null,
): ToolPart {
  return {
    ...DELEGATION,
    subagent: run === null ? undefined : { ...RUN, ...run },
    ...tool,
  };
}

function card(
  tool: Partial<ToolPart> = {},
  run: Partial<SubagentPart> | null = {},
): {
  readonly update: (
    t: Partial<ToolPart>,
    r?: Partial<SubagentPart> | null,
  ) => void;
} {
  const result = renderWithProviders(
    <ToolCard tool={toolWith(tool, run)} onApprove={vi.fn()} />,
  );
  return {
    /** Re-renders the same card with a later state of the same call. */
    update: (nextTool, nextRun = {}) => {
      result.update(
        <ToolCard tool={toolWith(nextTool, nextRun)} onApprove={vi.fn()} />,
      );
    },
  };
}

/** Opens a finished card, which is closed on a fresh mount like any other. */
async function open(): Promise<void> {
  await userEvent.click(screen.getByRole('button', { name: /ask_researcher/ }));
}

describe('a card that delegated', () => {
  it('opens itself while the subagent is working', () => {
    card();

    // The run is visible without anyone pressing anything: a collapsed card
    // over a running delegation says nothing for as long as it takes.
    expect(
      screen.getByRole('region', { name: 'Subagent run: Researcher' }),
    ).toBeInTheDocument();
    expect(screen.getByText('working')).toBeInTheDocument();
    expect(screen.getByText('qwen3:8b')).toBeInTheDocument();
  });

  it('stays open when the delegation returns, rather than swallowing its own output', () => {
    // The regression this exists for: a card whose open state is *derived* from
    // "is a subagent running" closes the instant the run ends — hiding both the
    // run and the answer the reader was waiting for.
    const { update } = card();
    expect(screen.getByText('working')).toBeInTheDocument();

    update(
      { status: 'ok', content: 'Retries default to 3.', durationMs: 4200 },
      {
        done: true,
        parts: [{ kind: 'text', id: 'sub-1#0', text: 'Found it.' }],
      },
    );

    expect(screen.queryByText('working')).not.toBeInTheDocument();
    expect(screen.getByText('Found it.')).toBeInTheDocument();
    expect(screen.getByText('Retries default to 3.')).toBeInTheDocument();
  });

  it('is closed on a fresh mount of a finished delegation', async () => {
    // A reloaded transcript with three delegations in it should not open as
    // three subagent transcripts. The disclosure rule is the same one every
    // tool card follows.
    card(
      { status: 'ok', content: 'Retries default to 3.' },
      {
        done: true,
        parts: [{ kind: 'text', id: 'sub-1#0', text: 'Found it.' }],
      },
    );

    expect(screen.queryByText('Found it.')).not.toBeInTheDocument();

    await open();
    expect(screen.getByText('Found it.')).toBeInTheDocument();
  });

  it("renders a subagent's own tool call as a card of its own", async () => {
    card(
      { status: 'ok' },
      {
        done: true,
        parts: [
          {
            kind: 'tool',
            id: 'n1',
            name: 'list_dir',
            args: { path: 'src' },
            risk: 'safe',
            status: 'ok',
            elapsedMs: 0,
            durationMs: 12,
            content: 'a.ts',
            truncated: false,
            approval: undefined,
            notices: [],
            subagent: undefined,
          },
        ],
      },
    );

    await open();

    // The same landmark and the same status label a top-level call gets — which
    // is the point of one renderer rather than a nested variant.
    expect(
      screen.getByRole('region', { name: 'Tool call: list_dir' }),
    ).toBeInTheDocument();
    expect(screen.getAllByLabelText('Succeeded')).toHaveLength(2);
  });

  it('says so when a subagent finished having done nothing', async () => {
    card({ status: 'ok' }, { done: true });
    await open();

    expect(
      screen.getByText('The subagent produced no steps.'),
    ).toBeInTheDocument();
  });

  it("fetches the run from the subagent's own session after a reload", async () => {
    // A rebuilt transcript knows a delegation happened and nothing else: the
    // steps are rows in the child's session, not in the parent's history.
    stubApi({
      '/api/sessions/sub-1/messages': [
        200,
        {
          sessionKey: 'sub-1',
          subagentRuns: {},
          messages: [
            {
              id: 'm1',
              sessionKey: 'sub-1',
              seq: 1,
              createdAtMs: 0,
              turnId: 't2',
              message: {
                role: 'user',
                content: [{ type: 'text', text: 'the task' }],
              },
            },
            {
              id: 'm2',
              sessionKey: 'sub-1',
              seq: 2,
              createdAtMs: 0,
              turnId: 't2',
              message: {
                role: 'assistant',
                content: [{ type: 'text', text: 'Found it.' }],
              },
            },
          ],
        },
      ],
    });

    card({ status: 'ok' }, { done: true, loaded: false });
    await open();

    expect(await screen.findByText('Found it.')).toBeInTheDocument();
    // The task is already the card's argument; repeating it inside the run
    // would read as though the subagent had been asked twice.
    expect(screen.queryByText('the task')).not.toBeInTheDocument();
  });

  it('offers the fetch again for a delegation the subagent made itself', async () => {
    // The recursion used to stop here. `subagentRuns` came back with the fetch
    // and was thrown away, so a subagent that delegated rebuilt its inner call
    // as a bare tool card — no run, and no way to ask for one.
    stubApi({
      '/api/sessions/sub-1/messages': [
        200,
        {
          sessionKey: 'sub-1',
          subagentRuns: {
            m1: {
              sessionKey: 'sub-2',
              agentId: 'summariser',
              label: 'Summariser',
            },
          },
          messages: [
            {
              id: 'r1',
              sessionKey: 'sub-1',
              seq: 1,
              createdAtMs: 0,
              turnId: 't2',
              message: {
                role: 'assistant',
                content: [],
                toolCalls: [
                  { id: 'm1', name: 'ask_summariser', argumentsJson: '{}' },
                ],
              },
            },
          ],
        },
      ],
      '/api/sessions/sub-2/messages': [
        200,
        {
          sessionKey: 'sub-2',
          subagentRuns: {},
          messages: [
            {
              id: 'r2',
              sessionKey: 'sub-2',
              seq: 1,
              createdAtMs: 0,
              turnId: 't3',
              message: {
                role: 'assistant',
                content: [{ type: 'text', text: 'Short.' }],
              },
            },
          ],
        },
      ],
    });

    card({ status: 'ok' }, { done: true, loaded: false });
    await open();

    // The inner card is a tool card like any other, so it opens on its own.
    // That it *has* something to open is the assertion.
    await userEvent.click(
      await screen.findByRole('button', { name: /ask_summariser/ }),
    );

    expect(
      await screen.findByRole('region', { name: 'Subagent run: Summariser' }),
    ).toBeInTheDocument();
    expect(await screen.findByText('Short.')).toBeInTheDocument();
  });

  it("says so when the subagent's session is gone", async () => {
    stubApi({
      '/api/sessions/sub-1/messages': [
        404,
        { error: { code: 'not_found', message: 'x' } },
      ],
    });

    card({ status: 'ok' }, { done: true, loaded: false });
    await open();

    expect(await screen.findByText(/no longer stored/)).toBeInTheDocument();
    // Never "produced no steps": that is a claim about the subagent, and this
    // is a fact about the database.
    expect(
      screen.queryByText('The subagent produced no steps.'),
    ).not.toBeInTheDocument();
  });

  it('adds nothing to an ordinary tool call', () => {
    card({ name: 'read_file', status: 'ok', content: 'hello' }, null);

    expect(
      screen.queryByRole('region', { name: /Subagent run/ }),
    ).not.toBeInTheDocument();
    // And it is closed by default, exactly as it was before delegation existed.
    expect(screen.queryByText('hello')).not.toBeInTheDocument();
  });
});
