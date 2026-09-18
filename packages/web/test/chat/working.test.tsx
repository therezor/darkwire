/**
 * What a running turn says while nothing on screen is moving.
 *
 * Every state here is transient by definition, the whole point being the window
 * between one tool finishing and the next thing starting, so it is asserted
 * against the components directly, where the state can be held still. An e2e
 * test would be asserting that the machine was slow enough to see it.
 */

import { screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import type { ToolPart, TurnItem, TurnPart } from '@/state/transcript.js';
import { renderWithProviders, stubApi } from '@testkit/render.js';

import { Message } from '@/chat/message.js';
import { ToolCard } from '@/chat/tool-card.js';

function tool(over: Partial<ToolPart> = {}): ToolPart {
  return {
    kind: 'tool',
    id: 'call-1',
    name: 'exec',
    args: { command: 'pnpm test' },
    risk: 'exec',
    status: 'running',
    elapsedMs: 0,
    progress: undefined,
    durationMs: undefined,
    content: undefined,
    truncated: false,
    approval: undefined,
    notices: [],
    subagent: undefined,
    ...over,
  };
}

function turn(parts: readonly TurnPart[]): TurnItem {
  return {
    kind: 'turn',
    id: 'turn-1',
    sessionKey: 'cli:default',
    parts,
    model: 'qwen3:8b',
    provider: 'ollama',
    stopReason: undefined,
    usage: undefined,
    iterations: 1,
    done: false,
    failure: undefined,
    firstSeq: undefined,
    lastSeq: undefined,
    elapsedMs: undefined,
    generationMs: undefined,
    generationTokens: undefined,
    firstTokenMs: undefined,
    authoritative: true,
  };
}

function show(item: TurnItem): void {
  renderWithProviders(
    <Message
      item={item}
      streaming
      busy
      sessionKey="cli:default"
      onApprove={vi.fn()}
      onAction={vi.fn()}
    />,
  );
}

describe('a running turn', () => {
  it('says it is working before the first token arrives', () => {
    show(turn([]));
    expect(screen.getByRole('status')).toHaveTextContent(/thinking/i);
  });

  it('says it is working again after a tool finishes', () => {
    // The gap this exists for. The card says `ok`, the next `tool.call` has not
    // arrived, and the model is generating in between. That used to show a
    // finished card and a Stop button and nothing else at all.
    show(turn([tool({ status: 'ok', durationMs: 1200, content: 'done' })]));
    expect(screen.getByRole('status')).toHaveTextContent(/thinking/i);
  });

  it('says it is working after a tool fails, too', () => {
    show(turn([tool({ status: 'error', durationMs: 90, content: 'no' })]));
    expect(screen.getByRole('status')).toHaveTextContent(/thinking/i);
  });

  it('stays quiet while a tool is running, which says so itself', () => {
    // Two things claiming the turn is busy is one thing too many: the card
    // spins and counts, which is more specific than "thinking".
    show(turn([tool()]));
    const statuses = screen.getAllByRole('status');
    expect(statuses.every((node) => !/thinking/i.test(node.textContent))).toBe(
      true,
    );
  });

  it('stays quiet while text is streaming', () => {
    show(turn([{ kind: 'text', id: 'text-1', text: 'the answer so far' }]));
    expect(screen.queryByText(/thinking/i)).toBeNull();
  });
});

describe('a running tool card', () => {
  it('says it is still running without being opened first', () => {
    // Collapsed is the default, and the one thing worth saying about a call
    // that has been going for a minute is that it still is. Behind the
    // disclosure it would be hidden by the very silence that prompts the click.
    stubApi({});
    renderWithProviders(<ToolCard tool={tool()} onApprove={vi.fn()} />);
    expect(screen.getByText(/running/i)).toBeInTheDocument();
  });

  it('prefers the server’s own sentence when a heartbeat carried one', () => {
    stubApi({});
    renderWithProviders(
      <ToolCard
        tool={tool({ progress: 'exec is still running' })}
        onApprove={vi.fn()}
      />,
    );
    expect(screen.getByText('exec is still running')).toBeInTheDocument();
  });

  it('says nothing of the sort once the call has finished', () => {
    stubApi({});
    renderWithProviders(
      <ToolCard
        tool={tool({ status: 'ok', durationMs: 1200, content: 'done' })}
        onApprove={vi.fn()}
      />,
    );
    expect(screen.queryByText(/still running/i)).toBeNull();
  });
});
