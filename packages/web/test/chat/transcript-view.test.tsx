/**
 * What a streaming delta costs the rest of the transcript.
 *
 * Every delta hands the view a new transcript. Only the part it grew may be
 * lexed again: a long conversation re-lexing every finished answer on every
 * token is what makes a busy tab fall behind the stream.
 */

import { act } from '@testing-library/react';
import type { JSX } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { ServerMessage } from '@darkwire/protocol';

import { applyServerMessage, type Transcript } from '@/state/transcript.js';
import { useTurnStore } from '@/state/turn.js';
import { renderWithProviders } from '@testkit/render.js';

import * as blocks from '@/chat/markdown/blocks.js';
import * as actions from '@/chat/message-actions.js';
import { TranscriptView } from '@/chat/transcript-view.js';

vi.mock('@/chat/markdown/blocks.js', async (importActual) => {
  const actual = await importActual<typeof blocks>();
  return { ...actual, splitBlocks: vi.fn(actual.splitBlocks) };
});

// The row under a finished answer, as a probe for whether it rendered at all.
vi.mock('@/chat/message-actions.js', async (importActual) => {
  const actual = await importActual<typeof actions>();
  return { ...actual, MessageActions: vi.fn(actual.MessageActions) };
});

type Unsequenced<T> = T extends unknown ? Omit<T, 'seq'> : never;

let seq = 0;

function play(
  items: Transcript,
  ...frames: ReadonlyArray<Unsequenced<ServerMessage>>
): Transcript {
  return frames.reduce<Transcript>(
    (next, frame) =>
      applyServerMessage(next, { ...frame, seq: (seq += 1) } as ServerMessage),
    items,
  );
}

const start = (turnId: string) =>
  ({
    type: 'turn.start',
    agentId: 'default',
    sessionKey: 'web:1',
    turnId,
    model: 'm',
    provider: 'p',
  }) as const;

const delta = (turnId: string, text: string) =>
  ({ type: 'assistant.delta', turnId, text }) as const;

// Stable, as the route's are. A new callback per frame would defeat the memo.
const onApprove = vi.fn();
const onAction = vi.fn();

/**
 * The view under a store subscription, as the route mounts it. Re-rendering
 * from outside would re-render the providers too, and every context consumer
 * with them.
 */
function Subject(): JSX.Element {
  const transcript = useTurnStore((state) => state.transcript);
  return (
    <TranscriptView
      transcript={transcript}
      busy
      sessionKey="web:1"
      onApprove={onApprove}
      onAction={onAction}
    />
  );
}

/** Mounts `before`, then applies `frame` with the spies cleared. */
function stream(before: Transcript, frame: Unsequenced<ServerMessage>): void {
  useTurnStore.getState().setTranscript(before);
  renderWithProviders(<Subject />);
  vi.mocked(blocks.splitBlocks).mockClear();
  vi.mocked(actions.MessageActions).mockClear();

  act(() => {
    useTurnStore.getState().setTranscript(play(before, frame));
  });
}

function lexed(): string[] {
  return vi.mocked(blocks.splitBlocks).mock.calls.map(([text]) => text);
}

function rendered(): string[] {
  return vi
    .mocked(actions.MessageActions)
    .mock.calls.map(([props]) => props.text);
}

beforeEach(() => {
  seq = 0;
});

describe('a delta arriving', () => {
  it('does not render or lex a finished answer again', () => {
    const before = play(
      [],
      start('t1'),
      delta('t1', 'the finished answer'),
      { type: 'turn.end', turnId: 't1', stopReason: 'complete', iterations: 1 },
      start('t2'),
      delta('t2', 'streaming'),
    );
    stream(before, delta('t2', ' on'));

    expect(rendered()).not.toContain('the finished answer');
    expect(lexed()).toEqual(['streaming on']);
  });

  it('does not lex the earlier text of the turn it grows', () => {
    const before = play(
      [],
      start('t1'),
      delta('t1', 'before the call'),
      {
        type: 'tool.call',
        turnId: 't1',
        callId: 'c1',
        name: 'ls',
        args: {},
        risk: 'safe',
      },
      {
        type: 'tool.result',
        turnId: 't1',
        callId: 'c1',
        ok: true,
        content: 'a.txt',
        truncated: false,
        durationMs: 1,
      },
      delta('t1', 'after'),
    );
    stream(before, delta('t1', ' the call'));

    expect(lexed()).toEqual(['after the call']);
  });
});
