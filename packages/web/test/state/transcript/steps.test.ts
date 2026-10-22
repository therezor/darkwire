/**
 * Steps: the end of a message in a turn's parts, written by `step.end` live
 * and by the stats on an assistant row from storage, and split back off those
 * parts into messages.
 */

import { describe, expect, it } from 'vitest';

import type {
  ServerMessage,
  StepStats,
  StoredMessage,
} from '@darkwire/protocol';

import {
  applyServerMessage,
  fromStoredMessages,
  messagesOf,
  truncateTranscriptAfter,
  type Transcript,
  type TurnItem,
} from '@/state/transcript.js';

type Unsequenced<T> = T extends unknown ? Omit<T, 'seq'> : never;

function play(
  ...frames: ReadonlyArray<Unsequenced<ServerMessage>>
): Transcript {
  let seq = 0;
  return frames.reduce<Transcript>(
    (items, frame) =>
      applyServerMessage(items, { ...frame, seq: (seq += 1) } as ServerMessage),
    [],
  );
}

function turnOf(items: Transcript): TurnItem {
  const turn = items.find((item) => item.kind === 'turn');
  if (turn === undefined) throw new Error('no turn in the transcript');
  return turn;
}

function stats(prompt: number, completion: number): StepStats {
  return {
    usage: {
      promptTokens: prompt,
      completionTokens: completion,
      totalTokens: prompt + completion,
    },
    model: 'm',
    elapsedMs: 1200,
    generationMs: 400,
    generationTokens: completion,
  };
}

const START = {
  type: 'turn.start',
  agentId: 'default',
  sessionKey: 'web:1',
  turnId: 't1',
  firstSeq: 1,
  model: 'm',
  provider: 'p',
} as const;

/** A step that read a file, then a step that answered: the frames. */
const TWO_STEPS: ReadonlyArray<Unsequenced<ServerMessage>> = [
  START,
  { type: 'assistant.delta', turnId: 't1', text: 'Looking.' },
  {
    type: 'tool.call',
    turnId: 't1',
    callId: 'c1',
    name: 'read',
    args: {},
    risk: 'safe',
  },
  {
    type: 'tool.result',
    turnId: 't1',
    callId: 'c1',
    ok: true,
    content: 'x',
    truncated: false,
    durationMs: 3,
  },
  {
    type: 'step.end',
    turnId: 't1',
    firstSeq: 2,
    lastSeq: 3,
    stats: stats(10, 4),
    turnElapsedMs: 1500,
  },
  { type: 'assistant.delta', turnId: 't1', text: 'It says x.' },
  {
    type: 'step.end',
    turnId: 't1',
    firstSeq: 4,
    lastSeq: 4,
    stats: stats(20, 6),
    turnElapsedMs: 2600,
  },
];

/** The same two steps, as storage holds them. */
function storedTwoSteps(): readonly StoredMessage[] {
  const row = (
    seq: number,
    message: StoredMessage['message'],
  ): StoredMessage => ({
    id: `m${String(seq)}`,
    sessionKey: 'web:1',
    seq,
    createdAtMs: 0,
    turnId: 't1',
    message,
  });
  return [
    row(1, { role: 'user', content: [{ type: 'text', text: 'read it' }] }),
    row(2, {
      role: 'assistant',
      content: [{ type: 'text', text: 'Looking.' }],
      toolCalls: [{ id: 'c1', name: 'read', argumentsJson: '{}' }],
      stats: stats(10, 4),
    }),
    row(3, {
      role: 'tool',
      toolCallId: 'c1',
      name: 'read',
      content: 'x',
      isError: false,
      truncated: false,
    }),
    row(4, {
      role: 'assistant',
      content: [{ type: 'text', text: 'It says x.' }],
      toolCalls: [],
      stats: stats(20, 6),
    }),
  ];
}

describe('a step, live', () => {
  it('lands as a boundary after the tools it ran', () => {
    const parts = turnOf(play(...TWO_STEPS)).parts;

    expect(parts.map((part) => part.kind)).toEqual([
      'text',
      'tool',
      'step',
      'text',
      'step',
    ]);
    expect(parts[2]).toMatchObject({
      kind: 'step',
      firstSeq: 2,
      lastSeq: 3,
    });
  });

  it('is not added twice when the ring replays it', () => {
    const frames = TWO_STEPS.slice(0, 5);
    const once = turnOf(play(...frames)).parts;
    const twice = turnOf(play(...frames, frames[4]!)).parts;

    expect(twice).toEqual(once);
  });

  it('never lands on a turn that has already ended', () => {
    const ended = play(...TWO_STEPS.slice(0, 4), {
      type: 'turn.end',
      turnId: 't1',
      stopReason: 'complete',
      iterations: 1,
    });
    const after = applyServerMessage(ended, {
      ...TWO_STEPS[4]!,
      seq: 99,
    } as ServerMessage);

    expect(after).toBe(ended);
  });
});

describe('a step, from storage', () => {
  it('rebuilds the parts the live frames built', () => {
    const live = turnOf(play(...TWO_STEPS)).parts;
    const stored = turnOf(fromStoredMessages(storedTwoSteps())).parts;

    const shape = (parts: typeof live): unknown =>
      parts.map((part) =>
        part.kind === 'step'
          ? [part.kind, part.firstSeq, part.lastSeq, part.stats]
          : part.kind,
      );
    expect(shape(stored)).toEqual(shape(live));
  });

  it('draws no boundary for a row that carries no figures', () => {
    const rows = storedTwoSteps().map((row) =>
      row.message.role === 'assistant'
        ? {
            ...row,
            message: { ...row.message, stats: undefined },
          }
        : row,
    );

    const parts = turnOf(fromStoredMessages(rows)).parts;
    expect(parts.some((part) => part.kind === 'step')).toBe(false);
  });
});

describe('a turn as its messages', () => {
  it('ends each message at its step, keyed by its first part', () => {
    const pieces = messagesOf(turnOf(play(...TWO_STEPS)).parts);

    expect(pieces).toMatchObject([
      { kind: 'message', number: 1, text: 'Looking.', key: 't1#0' },
      { kind: 'message', number: 2, text: 'It says x.', key: 't1#3' },
    ]);
    expect(pieces[0]?.kind === 'message' && pieces[0].end?.lastSeq).toBe(3);
    // The tool card belongs to the message that asked for it.
    expect(
      pieces[0]?.kind === 'message' && pieces[0].parts.map((part) => part.kind),
    ).toEqual(['text', 'tool']);
  });

  it('keeps the message still being written open, with its key', () => {
    const running = messagesOf(turnOf(play(...TWO_STEPS.slice(0, 4))).parts);
    const ended = messagesOf(turnOf(play(...TWO_STEPS.slice(0, 5))).parts);

    expect(running).toMatchObject([{ kind: 'message', end: undefined }]);
    // Same key once it ends, so React keeps its tool cards as they were.
    expect(ended[0]).toMatchObject({
      kind: 'message',
      key: running[0]?.kind === 'message' ? running[0].key : '',
    });
  });

  it('puts a steer between the messages it came between', () => {
    const items = play(...TWO_STEPS.slice(0, 5), {
      type: 'steer',
      sessionKey: 'web:1',
      turnId: 't1',
      content: 'faster',
    });

    expect(messagesOf(turnOf(items).parts).map((piece) => piece.kind)).toEqual([
      'message',
      'steer',
    ]);
  });
});

describe('cutting inside a turn', () => {
  it('keeps the messages that end at or before the cut', () => {
    const items = truncateTranscriptAfter(play(...TWO_STEPS), 3);

    expect(turnOf(items).parts.map((part) => part.kind)).toEqual([
      'text',
      'tool',
      'step',
    ]);
  });

  it('drops a turn the cut leaves with no message', () => {
    expect(truncateTranscriptAfter(play(...TWO_STEPS), 1)).toEqual([]);
  });

  it('finds a turn that wrote no question by its first message', () => {
    // A continuation names no question, so its first message is its address.
    const continued = play(
      { ...START, firstSeq: undefined },
      ...TWO_STEPS.slice(1),
    );

    expect(truncateTranscriptAfter(continued, 1)).toEqual([]);
  });
});
