/**
 * The reducer.
 *
 * Everything the chat view is judged on happens here rather than in a
 * component: a delta landing on the right part, a tool result finding its card,
 * a notice attaching to the call it describes, a reload rebuilding a turn that
 * has not finished. Testing it as a pure function over frames is what makes
 * "a mid-stream reload rebuilds the in-flight turn" a three-line assertion
 * instead of a browser.
 */

import { describe, expect, it } from 'vitest';

import type { ServerMessage, StoredMessage } from '@darkwire/protocol';

import {
  appendPendingUserMessage,
  applyServerMessage,
  fromStoredMessages,
  markApprovalAnswered,
  mergeStoredHistory,
  truncateTranscriptAfter,
  unwrapToolOutput,
  type ToolPart,
  type Transcript,
  type TurnItem,
} from '@/state/transcript.js';

/**
 * Distributes over the union, which a bare `Omit` would collapse into one
 * object type carrying only the fields every variant has.
 */
type Unsequenced<T> = T extends unknown ? Omit<T, 'seq'> : never;

/** Frames in order, with the sequence numbers the hub would have stamped. */
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

const START = {
  type: 'turn.start',
  agentId: 'default',
  sessionKey: 'web:1',
  turnId: 't1',
  model: 'm',
  provider: 'p',
} as const;

const turnOf = (items: Transcript): TurnItem => {
  const turn = items.find((item) => item.kind === 'turn');
  if (turn === undefined) throw new Error('no turn in the transcript');
  return turn;
};

const toolsOf = (items: Transcript): readonly ToolPart[] =>
  turnOf(items).parts.filter((part) => part.kind === 'tool');

describe('accumulating a turn', () => {
  it('appends deltas into one text part', () => {
    const items = play(
      START,
      { type: 'assistant.delta', turnId: 't1', text: 'Hello' },
      { type: 'assistant.delta', turnId: 't1', text: ', world' },
    );

    expect(turnOf(items).parts).toEqual([
      { kind: 'text', id: 't1#0', text: 'Hello, world' },
    ]);
  });

  it('starts a new text part after a tool call rather than merging across it', () => {
    const items = play(
      START,
      { type: 'assistant.delta', turnId: 't1', text: 'Let me look.' },
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
        content: 'ok',
        truncated: false,
        durationMs: 5,
      },
      { type: 'assistant.delta', turnId: 't1', text: 'Found it.' },
    );

    // Merging the second answer into the first would render the card below
    // text that was written before it.
    expect(turnOf(items).parts.map((part) => part.kind)).toEqual([
      'text',
      'tool',
      'text',
    ]);
  });

  it('keeps reasoning in its own part', () => {
    const items = play(
      START,
      { type: 'reasoning.delta', turnId: 't1', text: 'thinking' },
      { type: 'assistant.delta', turnId: 't1', text: 'answer' },
    );

    expect(turnOf(items).parts).toEqual([
      { kind: 'reasoning', id: 't1#0', text: 'thinking' },
      { kind: 'text', id: 't1#1', text: 'answer' },
    ]);
  });

  it('ignores an empty delta', () => {
    const items = play(START, {
      type: 'assistant.delta',
      turnId: 't1',
      text: '',
    });

    expect(turnOf(items).parts).toEqual([]);
  });

  it('does not open a second turn when the ring replays its start', () => {
    const items = play(
      START,
      { type: 'assistant.delta', turnId: 't1', text: 'hi' },
      START,
    );

    // Every resume re-delivers frames the client may already hold. Appending
    // would leave an empty turn above the real one on each one.
    expect(items.filter((item) => item.kind === 'turn')).toHaveLength(1);
    expect(turnOf(items).parts).toHaveLength(1);
  });

  it('closes the turn with its stop reason and usage', () => {
    const items = play(START, {
      type: 'turn.end',
      turnId: 't1',
      stopReason: 'aborted',
      iterations: 2,
      usage: { promptTokens: 10, completionTokens: 4, totalTokens: 14 },
    });

    expect(turnOf(items)).toMatchObject({
      done: true,
      stopReason: 'aborted',
      iterations: 2,
    });
  });

  it('records a turn-scoped error on the turn and leaves a connection error alone', () => {
    const scoped = play(START, {
      type: 'error',
      code: 'provider_error',
      message: 'upstream said no',
      retryable: true,
      turnId: 't1',
    });
    expect(turnOf(scoped)).toMatchObject({
      done: true,
      failure: { message: 'upstream said no', retryable: true },
    });

    // A connection-scoped error has no turn to render on; the toast is its home.
    const unscoped = play(START, {
      type: 'error',
      code: 'internal',
      message: 'boom',
      retryable: false,
    });
    expect(unscoped).toHaveLength(1);
  });
});

describe('tool calls', () => {
  const call = {
    type: 'tool.call',
    turnId: 't1',
    callId: 'c1',
    name: 'exec',
    args: { command: 'ls' },
    risk: 'exec',
  } as const;

  it('runs, then reports, and never shows the envelope', () => {
    const items = play(START, call, {
      type: 'tool.result',
      turnId: 't1',
      callId: 'c1',
      ok: true,
      content: 'a\nb',
      truncated: true,
      durationMs: 120,
    });

    expect(toolsOf(items)[0]).toMatchObject({
      name: 'exec',
      risk: 'exec',
      status: 'ok',
      content: 'a\nb',
      truncated: true,
      durationMs: 120,
    });
  });

  it('marks a failure as an error rather than inspecting the output for one', () => {
    const items = play(START, call, {
      type: 'tool.result',
      turnId: 't1',
      callId: 'c1',
      ok: false,
      content: 'no such file',
      truncated: false,
      durationMs: 2,
    });

    expect(toolsOf(items)[0]?.status).toBe('error');
  });

  it('only moves the heartbeat forward', () => {
    const items = play(
      START,
      call,
      { type: 'tool.progress', turnId: 't1', callId: 'c1', elapsedMs: 30_000 },
      { type: 'tool.progress', turnId: 't1', callId: 'c1', elapsedMs: 15_000 },
    );

    expect(toolsOf(items)[0]?.elapsedMs).toBe(30_000);
  });

  it('does not create a second card when the ring replays the call', () => {
    const items = play(START, call, call);

    expect(toolsOf(items)).toHaveLength(1);
  });

  it('opens a card for a result whose call arrived before this client did', () => {
    // The case a resume lands in: connected between the call and the result.
    const items = play(START, {
      type: 'tool.result',
      turnId: 't1',
      callId: 'orphan',
      ok: true,
      content: 'output',
      truncated: false,
      durationMs: 1,
    });

    expect(toolsOf(items)[0]).toMatchObject({
      id: 'orphan',
      name: 'tool',
      status: 'ok',
    });
  });

  it('gates on an approval request and clears it when the result lands', () => {
    const gated = play(START, call, {
      type: 'tool.approvalRequest',
      turnId: 't1',
      callId: 'c1',
      name: 'exec',
      args: { command: 'ls' },
      risk: 'exec',
      expiresAtMs: 1_000,
    });

    expect(toolsOf(gated)[0]).toMatchObject({
      status: 'awaiting-approval',
      approval: { expiresAtMs: 1_000, answered: undefined },
    });

    const answered = markApprovalAnswered(gated, 'c1', 'approved');
    expect(toolsOf(answered)[0]?.approval).toEqual({
      expiresAtMs: 1_000,
      answered: 'approved',
    });

    const resolved = applyServerMessage(answered, {
      type: 'tool.result',
      seq: 9,
      turnId: 't1',
      callId: 'c1',
      ok: true,
      content: '',
      truncated: false,
      durationMs: 3,
    });
    expect(toolsOf(resolved)[0]?.approval).toBeUndefined();
  });

  it('keeps what the rules made of a command, and reopens on a refused rule', () => {
    const command = { argv: ['ls'], shell: false };
    const gated = play(START, call, {
      type: 'tool.approvalRequest',
      turnId: 't1',
      callId: 'c1',
      name: 'exec',
      args: { argv: ['ls'] },
      risk: 'exec',
      expiresAtMs: 1_000,
      command,
    });
    expect(toolsOf(gated)[0]?.approval?.command).toEqual(command);

    const answered = markApprovalAnswered(gated, 'c1', 'approved');
    const refused = markApprovalAnswered(answered, 'c1', undefined, 'no');
    expect(toolsOf(refused)[0]?.approval).toMatchObject({
      answered: undefined,
      error: 'no',
      command,
    });
  });

  it('leaves a transcript alone when an answer names a call that is not in it', () => {
    const items = play(START, call);

    expect(markApprovalAnswered(items, 'nope', 'denied')).toEqual(items);
  });
});

describe('notices', () => {
  it('puts a call-scoped notice inside that call, not below the turn', () => {
    const items = play(
      START,
      {
        type: 'tool.call',
        turnId: 't1',
        callId: 'c1',
        name: 'fetch',
        args: {},
        risk: 'network',
      },
      {
        type: 'notice',
        kind: 'prompt_injection',
        message: 'instruction_override',
        turnId: 't1',
        callId: 'c1',
      },
    );

    // A warning rendered away from the output it describes is a warning about
    // nothing in particular.
    expect(toolsOf(items)[0]?.notices).toEqual([
      {
        kind: 'notice',
        id: 'notice:3',
        notice: 'prompt_injection',
        message: 'instruction_override',
      },
    ]);
  });

  it('puts a turn-scoped notice in the turn', () => {
    const items = play(START, {
      type: 'notice',
      kind: 'truncated_history',
      message: 'dropped 3 messages',
      turnId: 't1',
    });

    expect(turnOf(items).parts).toEqual([
      {
        kind: 'notice',
        id: 't1#0',
        notice: 'truncated_history',
        message: 'dropped 3 messages',
      },
    ]);
  });

  it('puts a notice with no turn at the top level', () => {
    const items = play({
      type: 'notice',
      kind: 'degraded',
      message: 'dropped images',
    });

    expect(items).toEqual([
      {
        kind: 'notice',
        id: 'notice:1',
        notice: 'degraded',
        message: 'dropped images',
      },
    ]);
  });
});

describe('the user bubble', () => {
  it('appears before the ack and settles on it', () => {
    const pending = appendPendingUserMessage([], {
      clientMessageId: 'c-1',
      text: 'hi',
    });
    expect(pending[0]).toMatchObject({ id: 'c-1', pending: true });

    const acked = applyServerMessage(pending, {
      type: 'message.ack',
      seq: 1,
      sessionKey: 'web:1',
      messageId: 't1',
      clientMessageId: 'c-1',
    });

    // The id does not become the turn id. Every item is a React key, and the
    // turn this message started is about to arrive carrying that exact id — two
    // siblings with one key is a subtree React cannot reconcile, which renders
    // as a message that appears twice.
    expect(acked[0]).toMatchObject({ id: 'c-1', turnId: 't1', pending: false });
  });

  it('never shares an id with the turn it started', () => {
    const acked = applyServerMessage(
      appendPendingUserMessage([], { clientMessageId: 'c-1', text: 'hi' }),
      {
        type: 'message.ack',
        seq: 1,
        sessionKey: 'web:1',
        messageId: 't1',
        clientMessageId: 'c-1',
      },
    );

    const withTurn = applyServerMessage(acked, { ...START, seq: 2 });
    const ids = withTurn.map((item) => item.id);

    expect(new Set(ids).size).toBe(ids.length);
  });

  it('is dropped rather than duplicated when storage already has the message', () => {
    // The order a reload produces: the REST history lands before the ack. Both
    // describe the same sentence, and the turn id is the only key they share.
    const withHistory = fromStoredMessages([
      stored('row-1', 't1', {
        role: 'user',
        content: [{ type: 'text', text: 'hi' }],
      }),
    ]);
    const pending = appendPendingUserMessage(withHistory, {
      clientMessageId: 'c-1',
      text: 'hi',
    });

    const acked = applyServerMessage(pending, {
      type: 'message.ack',
      seq: 1,
      sessionKey: 'web:1',
      messageId: 't1',
      clientMessageId: 'c-1',
    });

    expect(acked).toHaveLength(1);
    expect(acked[0]).toMatchObject({ id: 'row-1', turnId: 't1' });
  });

  it('records the turn id, so a history that lands later recognises it', () => {
    const pending = appendPendingUserMessage([], {
      clientMessageId: 'c-1',
      text: 'hi',
    });

    const acked = applyServerMessage(pending, {
      type: 'message.ack',
      seq: 1,
      sessionKey: 'web:1',
      messageId: 't1',
      clientMessageId: 'c-1',
    });

    expect(acked[0]).toMatchObject({ turnId: 't1' });
  });

  it('ignores an ack for someone else’s message', () => {
    const pending = appendPendingUserMessage([], {
      clientMessageId: 'c-1',
      text: 'hi',
    });

    const other = applyServerMessage(pending, {
      type: 'message.ack',
      seq: 1,
      sessionKey: 'web:1',
      messageId: 't9',
      clientMessageId: 'c-2',
    });

    expect(other[0]).toMatchObject({ id: 'c-1', pending: true });
  });
});

describe('session frames', () => {
  it('empties the transcript on a reset', () => {
    const items = play(START, { type: 'session.reset', sessionKey: 'web:1' });

    expect(items).toEqual([]);
  });

  it('records a steer so every tab sees what was injected', () => {
    const items = play(START, {
      type: 'steer',
      sessionKey: 'web:1',
      content: 'be brief',
    });

    expect(items.at(-1)).toEqual({
      kind: 'steer',
      id: 'steer:2',
      text: 'be brief',
    });
  });

  it('leaves the transcript alone when a replay was covered by the ring', () => {
    const items = play(START, {
      type: 'assistant.delta',
      turnId: 't1',
      text: 'hi',
    });

    const replayed = applyServerMessage(items, {
      type: 'session.replay',
      seq: 9,
      sessionKey: 'web:1',
      messages: [],
      complete: true,
    });

    // The frames themselves follow; rebuilding here would render them twice.
    expect(replayed).toBe(items);
  });

  it('rebuilds from storage when the resume fell outside the ring', () => {
    const replayed = applyServerMessage([], {
      type: 'session.replay',
      seq: 9,
      sessionKey: 'web:1',
      messages: [
        stored('m1', 'user', {
          role: 'user',
          content: [{ type: 'text', text: 'q' }],
        }),
      ],
      complete: false,
    });

    expect(replayed).toHaveLength(1);
    expect(replayed[0]).toMatchObject({ kind: 'user', text: 'q' });
  });

  it('keeps the running turn when the resume fell outside the ring', () => {
    // The same frame arrives on a *reconnect*, where the page never went away.
    // Storage cannot describe a turn that has not finished, so rebuilding over
    // it deleted the answer the user was watching, mid-stream, in front of them.
    const live = play(START, {
      type: 'assistant.delta',
      turnId: 't1',
      text: 'half an answer',
    });

    const replayed = applyServerMessage(live, {
      type: 'session.replay',
      seq: 9,
      sessionKey: 'web:1',
      messages: [
        stored('m1', 'told', {
          role: 'user',
          content: [{ type: 'text', text: 'an older question' }],
        }),
      ],
      complete: false,
    });

    expect(replayed.map((item) => item.kind)).toEqual(['user', 'turn']);
    expect(replayed.at(-1)).toMatchObject({
      kind: 'turn',
      id: 't1',
      done: false,
      parts: [{ kind: 'text', text: 'half an answer' }],
    });
  });

  it('drops a finished turn the rebuild already accounts for', () => {
    // Only what storage cannot supply survives. A turn that ended is in the
    // rows, and keeping the live copy beside it renders the answer twice.
    const live = applyServerMessage(
      play(START, { type: 'assistant.delta', turnId: 't1', text: 'done' }),
      {
        type: 'turn.end',
        seq: 8,
        turnId: 't1',
        stopReason: 'complete',
        iterations: 1,
      },
    );

    const replayed = applyServerMessage(live, {
      type: 'session.replay',
      seq: 9,
      sessionKey: 'web:1',
      messages: [
        stored('m1', 't1', {
          role: 'user',
          content: [{ type: 'text', text: 'q' }],
        }),
        stored('m2', 't1', {
          role: 'assistant',
          content: [{ type: 'text', text: 'done' }],
          toolCalls: [],
        }),
      ],
      complete: false,
    });

    expect(replayed.map((item) => item.kind)).toEqual(['user', 'turn']);
  });

  it('clears the way for a turn the server is about to replay in full', () => {
    // Past the ring *while a turn is running*, the server sends both the stored
    // tail and the turn's own frames — legal only because it names the one turn
    // they overlap on. The stored copy of that turn goes; its question stays,
    // because no frame recreates a user bubble.
    const replayed = applyServerMessage([], {
      type: 'session.replay',
      seq: 9,
      sessionKey: 'web:1',
      messages: [
        stored('m1', 'told', {
          role: 'user',
          content: [{ type: 'text', text: 'an older question' }],
        }),
        stored('m2', 'told', {
          role: 'assistant',
          content: [{ type: 'text', text: 'an older answer' }],
          toolCalls: [],
        }),
        stored('m3', 't1', {
          role: 'user',
          content: [{ type: 'text', text: 'and this one' }],
        }),
        stored('m4', 't1', {
          role: 'assistant',
          content: [{ type: 'text', text: 'a finished iteration' }],
          toolCalls: [],
        }),
      ],
      complete: false,
      resumingTurnId: 't1',
    });

    expect(replayed.map((item) => item.kind)).toEqual([
      'user',
      'turn',
      'user',
      'turn',
    ]);
    expect(replayed.at(-2)).toMatchObject({
      kind: 'user',
      text: 'and this one',
    });
    // The seat: empty, unfinished, and the one item the fetched history may not
    // overrule.
    expect(replayed.at(-1)).toMatchObject({
      kind: 'turn',
      id: 't1',
      parts: [],
      done: false,
      authoritative: true,
    });
  });

  it('carries the stored duration onto a replayed tool card', () => {
    // Nothing can work out afterwards how long a call took, so the figure is
    // stored with the result. A card read back says what it said live.
    const built = fromStoredMessages([
      stored('m0', 't1', {
        role: 'user',
        content: [{ type: 'text', text: 'read it' }],
      }),
      stored('m1', 't1', {
        role: 'assistant',
        content: [],
        toolCalls: [{ id: 'c1', name: 'read', argumentsJson: '{}' }],
      }),
      stored('m2', 't1', {
        role: 'tool',
        toolCallId: 'c1',
        name: 'read',
        content: 'contents',
        isError: false,
        truncated: false,
        durationMs: 120,
      }),
    ]);

    expect(toolOf(built)).toMatchObject({ kind: 'tool', durationMs: 120 });
  });

  it('leaves the card silent when the row kept no duration', () => {
    const built = fromStoredMessages([
      stored('m0', 't1', {
        role: 'user',
        content: [{ type: 'text', text: 'read it' }],
      }),
      stored('m1', 't1', {
        role: 'assistant',
        content: [],
        toolCalls: [{ id: 'c1', name: 'read', argumentsJson: '{}' }],
      }),
      stored('m2', 't1', {
        role: 'tool',
        toolCallId: 'c1',
        name: 'read',
        content: 'contents',
        isError: false,
        truncated: false,
      }),
    ]);

    expect(toolOf(built)).toMatchObject({
      kind: 'tool',
      durationMs: undefined,
    });
  });

  it('seats the replayed turn after its question, not before it', () => {
    // The tail of a running turn ends in a tool row as often as in an assistant
    // one — a delegation's result is written when the iteration closes. A tool
    // row attaches to the turn it names rather than opening an item, so dropping
    // the turn has to leave the question it followed, and the seat has to land
    // after that rather than wherever the row happened to be.
    const replayed = applyServerMessage([], {
      type: 'session.replay',
      seq: 9,
      sessionKey: 'web:1',
      messages: [
        stored('m1', 't1', {
          role: 'user',
          content: [{ type: 'text', text: 'the question' }],
        }),
        stored('m2', 't1', {
          role: 'assistant',
          content: [],
          toolCalls: [{ id: 'c1', name: 'read', argumentsJson: '{}' }],
        }),
        stored('m3', 't1', {
          role: 'tool',
          toolCallId: 'c1',
          name: 'read',
          content: 'contents',
          isError: false,
          truncated: false,
        }),
      ],
      complete: false,
      resumingTurnId: 't1',
    });

    expect(replayed.map((item) => item.kind)).toEqual(['user', 'turn']);
    expect(replayed[0]).toMatchObject({ text: 'the question' });
    expect(replayed[1]).toMatchObject({ id: 't1', parts: [], done: false });
  });

  it('rebuilds the replayed turn from the frames that follow', () => {
    const seated = applyServerMessage([], {
      type: 'session.replay',
      seq: 9,
      sessionKey: 'web:1',
      messages: [
        stored('m1', 't1', {
          role: 'user',
          content: [{ type: 'text', text: 'q' }],
        }),
      ],
      complete: false,
      resumingTurnId: 't1',
    });

    const rebuilt = [
      { ...START, seq: 10 },
      { type: 'assistant.delta', seq: 11, turnId: 't1', text: 'the whole ' },
      { type: 'assistant.delta', seq: 12, turnId: 't1', text: 'answer' },
    ].reduce<Transcript>(
      (items, frame) => applyServerMessage(items, frame as ServerMessage),
      seated,
    );

    // One turn, not two: the seat is the turn the `turn.start` names.
    expect(rebuilt.filter((item) => item.kind === 'turn')).toHaveLength(1);
    expect(turnOf(rebuilt)).toMatchObject({
      id: 't1',
      // The replayed start fills in what the seat could not know.
      model: 'm',
      provider: 'p',
      sessionKey: 'web:1',
      authoritative: true,
      parts: [{ kind: 'text', text: 'the whole answer' }],
    });
  });

  it('drops this tab’s own copy of a turn the server is replaying', () => {
    // A reconnect rather than a reload: the page never went away, so it holds a
    // tail of the same turn. Keeping it would render its text twice, since the
    // frames that follow start from the beginning.
    const live = play(START, {
      type: 'assistant.delta',
      turnId: 't1',
      text: 'half an answer',
    });

    const replayed = applyServerMessage(live, {
      type: 'session.replay',
      seq: 9,
      sessionKey: 'web:1',
      messages: [],
      complete: false,
      resumingTurnId: 't1',
    });

    expect(replayed).toHaveLength(1);
    expect(replayed[0]).toMatchObject({ id: 't1', parts: [], done: false });
  });

  it('ignores the frames that are not transcript events', () => {
    const items = play(
      START,
      {
        type: 'connected',
        workspaceId: 'default',
        protocolVersion: 2,
        sessionKey: 'web:1',
        serverTimeMs: 0,
        lastSeq: 0,
      },
      { type: 'pong', serverTimeMs: 0 },
      {
        type: 'session.status',
        workspaceId: 'default',
        sessionKey: 'web:1',
        busy: true,
        queueDepth: 0,
      },
      { type: 'message.queued', sessionKey: 'web:1', queueDepth: 1 },
      { type: 'tools.changed', tools: [] },
      {
        type: 'notification',
        id: 'n1',
        title: 't',
        body: 'b',
        level: 'info',
        createdAtMs: 0,
      },
    );

    expect(items).toHaveLength(1);
  });
});

describe('the mid-stream reload', () => {
  it('rebuilds a turn from deltas whose turn.start fell outside the buffer', () => {
    // What a resume actually delivers when the ring has rolled past the start:
    // the deltas, with no `turn.start` in front of them.
    const items = play(
      { type: 'assistant.delta', turnId: 't1', text: 'half an ' },
      { type: 'assistant.delta', turnId: 't1', text: 'answer' },
      {
        type: 'tool.call',
        turnId: 't1',
        callId: 'c1',
        name: 'read',
        args: {},
        risk: 'safe',
      },
    );

    expect(turnOf(items)).toMatchObject({ id: 't1', done: false });
    expect(turnOf(items).parts.map((part) => part.kind)).toEqual([
      'text',
      'tool',
    ]);
  });
});

describe('the nonce envelope', () => {
  const NONCE = 'a1b2c3d4e5f60718';

  it('is stripped from stored tool output', () => {
    const wrapped = `<tool_output_${NONCE} name="exec">\nthe real output\n</tool_output_${NONCE}>`;

    // The delimiters are a defence aimed at the model. Showing them to a reader
    // would be showing them the machinery instead of the answer.
    expect(unwrapToolOutput(wrapped)).toBe('the real output');
  });

  it('restores a delimiter the wrapper escaped', () => {
    const wrapped = `<tool_output_${NONCE} name="cat">\nsee <\\tool_output_${NONCE}> here\n</tool_output_${NONCE}>`;

    expect(unwrapToolOutput(wrapped)).toBe(`see <tool_output_${NONCE}> here`);
  });

  it('leaves content that is not an envelope untouched', () => {
    expect(unwrapToolOutput('plain output')).toBe('plain output');
    expect(
      unwrapToolOutput('<tool_output_short name="x">\na\n</tool_output_short>'),
    ).toBe('<tool_output_short name="x">\na\n</tool_output_short>');
  });
});

describe('a stored history', () => {
  it('groups a turn and pairs its tool results by id', () => {
    const items = fromStoredMessages([
      stored('m1', 't1', {
        role: 'user',
        content: [{ type: 'text', text: 'run it' }],
      }),
      stored('m2', 't1', {
        role: 'assistant',
        content: [{ type: 'text', text: 'running' }],
        reasoning: 'I should run it',
        toolCalls: [
          { id: 'c1', name: 'exec', argumentsJson: '{"command":"ls"}' },
        ],
      }),
      stored('m3', 't1', {
        role: 'tool',
        toolCallId: 'c1',
        name: 'exec',
        content: 'a.txt',
        isError: false,
        truncated: false,
      }),
    ]);

    expect(items).toHaveLength(2);
    expect(items[0]).toMatchObject({ kind: 'user', text: 'run it' });

    const turn = turnOf(items);
    expect(turn.done).toBe(true);
    expect(turn.parts.map((part) => part.kind)).toEqual([
      'reasoning',
      'text',
      'tool',
    ]);
    expect(toolsOf(items)[0]).toMatchObject({
      name: 'exec',
      args: { command: 'ls' },
      status: 'ok',
      content: 'a.txt',
    });
  });

  it('shows why a turn failed, for a turn that answered nothing', () => {
    // The shape a failed turn actually leaves behind: the question, and nothing
    // else. An error is never appended to history — it would be replayed into
    // every later provider request — so the reason travels beside the rows.
    const items = fromStoredMessages(
      [
        stored(
          'm1',
          't1',
          { role: 'user', content: [{ type: 'text', text: 'hi' }] },
          7,
        ),
      ],
      {},
      { t1: 'No container runtime is reachable.' },
    );

    // One turn, not two, and it sits after the question it belongs to.
    expect(items).toHaveLength(2);
    expect(items[0]).toMatchObject({ kind: 'user', text: 'hi', seq: 7 });
    expect(turnOf(items)).toMatchObject({
      id: 't1',
      done: true,
      failure: { message: 'No container runtime is reachable.' },
      // The address Regenerate re-runs from. Without it the rebuilt turn is
      // exactly the un-runnable one this whole change is about.
      firstSeq: 7,
    });
  });

  it('does not open a second turn when a failed turn also answered', () => {
    const items = fromStoredMessages(
      [
        stored('m1', 't1', {
          role: 'user',
          content: [{ type: 'text', text: 'hi' }],
        }),
        stored('m2', 't1', {
          role: 'assistant',
          content: [{ type: 'text', text: 'partly' }],
          toolCalls: [],
        }),
      ],
      {},
      { t1: 'the stream ended early' },
    );

    expect(items.filter((item) => item.kind === 'turn')).toHaveLength(1);
    expect(turnOf(items)).toMatchObject({
      failure: { message: 'the stream ended early' },
    });
  });

  it('keeps malformed tool arguments as the string the model emitted', () => {
    const items = fromStoredMessages([
      stored('m1', 't1', {
        role: 'assistant',
        content: [],
        toolCalls: [{ id: 'c1', name: 'exec', argumentsJson: '{"command": ' }],
      }),
    ]);

    // Models emit invalid JSON often enough that a parse failure cannot be an
    // exception here — the card renders whichever it turned out to be.
    expect(toolsOf(items)[0]?.args).toBe('{"command": ');
  });

  it('never renders a system message', () => {
    const items = fromStoredMessages([
      stored('m0', undefined, { role: 'system', content: 'you are an agent' }),
      stored('m1', undefined, {
        role: 'user',
        content: [{ type: 'text', text: 'hi' }],
      }),
    ]);

    expect(items).toHaveLength(1);
  });

  it('carries file attachments through as chips, name and size intact', () => {
    // The reload assertion. Before attachments were workspace files the stored
    // form was a signed URL with a ten-minute life, and `name` and `sizeBytes`
    // were not stored at all -- so a transcript reopened the next morning drew
    // a chip labelled `image/png` pointing at a dead link.
    const items = fromStoredMessages([
      stored('m1', undefined, {
        role: 'user',
        content: [
          { type: 'text', text: 'look' },
          {
            type: 'file',
            mimeType: 'image/png',
            path: 'uploads/ab12cd34-shot.png',
            name: 'shot.png',
            sizeBytes: 2048,
          },
        ],
      }),
    ]);

    expect(items[0]).toMatchObject({
      text: 'look',
      attachments: [
        {
          mimeType: 'image/png',
          path: 'uploads/ab12cd34-shot.png',
          name: 'shot.png',
          sizeBytes: 2048,
        },
      ],
    });
  });

  it('shows no chip for a legacy image part', () => {
    // Both stored forms are unusable now: inline bytes are a megabyte to draw a
    // chip that says "image", and a `/api/media/` token expired long ago and
    // would render as a broken picture. Nothing is better than either.
    const items = fromStoredMessages([
      stored('m1', undefined, {
        role: 'user',
        content: [
          { type: 'text', text: 'look' },
          { type: 'image', mimeType: 'image/png', url: '/api/media/abc' },
          { type: 'image', mimeType: 'image/png', data: 'AAAA' },
        ],
      }),
    ]);

    expect(items[0]).toMatchObject({ text: 'look', attachments: [] });
  });

  it('renders a message with no turnId as a turn of one', () => {
    const items = fromStoredMessages([
      stored('m1', undefined, {
        role: 'assistant',
        content: [{ type: 'text', text: 'a' }],
        toolCalls: [],
      }),
      stored('m2', undefined, {
        role: 'assistant',
        content: [{ type: 'text', text: 'b' }],
        toolCalls: [],
      }),
    ]);

    expect(items).toHaveLength(2);
  });

  it('ignores a tool result whose call is in no turn', () => {
    const items = fromStoredMessages([
      stored('m1', 't1', {
        role: 'tool',
        toolCallId: 'missing',
        name: 'exec',
        content: 'output',
        isError: false,
        truncated: false,
      }),
    ]);

    expect(items).toEqual([]);
  });

  it('marks a failed stored tool call as an error', () => {
    const items = fromStoredMessages([
      stored('m1', 't1', {
        role: 'assistant',
        content: [],
        toolCalls: [{ id: 'c1', name: 'read', argumentsJson: '{}' }],
      }),
      stored('m2', 't1', {
        role: 'tool',
        toolCallId: 'c1',
        name: 'read',
        content: 'ENOENT',
        isError: true,
        truncated: true,
      }),
    ]);

    expect(toolsOf(items)[0]).toMatchObject({
      status: 'error',
      truncated: true,
    });
  });
});

describe('merging a fetched history', () => {
  const row = stored('row-1', 't1', {
    role: 'user',
    content: [{ type: 'text', text: 'the question' }],
  });

  it('puts storage underneath what the socket has already built', () => {
    const live = play(START, {
      type: 'assistant.delta',
      turnId: 't1',
      text: 'half an answer',
    });

    const merged = mergeStoredHistory(live, [row]);

    // Replacing would discard the turn the user is watching; appending would
    // render the conversation twice.
    expect(merged.map((item) => item.kind)).toEqual(['user', 'turn']);
    expect(turnOf(merged).parts).toHaveLength(1);
  });

  it('keeps a watched turn whole against a history fetched before it ended', () => {
    // The request went out when the socket minted the session key, and answered
    // after the turn finished — so it carries the tool row and not the answer
    // that followed it. Taking it as the base deleted text that was already on
    // screen, and nothing fetched again: the invalidation that would have
    // refetched fires while this request is still open and is deduplicated into
    // it. Only a reload put the answer back.
    const live = play(
      START,
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
        content: 'notes.md',
        truncated: false,
        durationMs: 1,
      },
      { type: 'assistant.delta', turnId: 't1', text: 'the answer' },
    );

    const merged = mergeStoredHistory(live, [
      row,
      stored('m2', 't1', {
        role: 'assistant',
        content: [],
        toolCalls: [{ id: 'c1', name: 'ls', argumentsJson: '{}' }],
      }),
      stored('m3', 't1', {
        role: 'tool',
        toolCallId: 'c1',
        name: 'ls',
        content: 'notes.md',
        isError: false,
        truncated: false,
      }),
    ]);

    expect(turnOf(merged).parts).toHaveLength(2);
    expect(turnOf(merged).parts.at(-1)).toMatchObject({
      kind: 'text',
      text: 'the answer',
    });
  });

  it('takes storage once it holds everything the socket showed', () => {
    // The other direction, and the reason the test is on what each side *has*
    // rather than on whether the turn is running: a rebuild that has caught up
    // is the better base, because it carries the row ids that make a finished
    // turn editable and `withLiveRiskBands` puts back the detail rows cannot
    // hold.
    const live = play(START, {
      type: 'assistant.delta',
      turnId: 't1',
      text: 'the answer',
    });

    const merged = mergeStoredHistory(live, [
      row,
      stored('m2', 't1', {
        role: 'assistant',
        content: [{ type: 'text', text: 'the answer, as stored' }],
        toolCalls: [],
      }),
    ]);

    expect(turnOf(merged).parts).toMatchObject([
      { kind: 'text', text: 'the answer, as stored' },
    ]);
  });

  it('keeps a replayed turn whole against the stored half of itself', () => {
    // The refetch that follows an incomplete replay lands mid-turn, and storage
    // holds the iterations that finished — as a turn marked `done`, under the
    // same id. Left to win, it froze the turn: the frames that followed were
    // dropped as belonging to a turn that had ended.
    const replayed = applyServerMessage([], {
      type: 'session.replay',
      seq: 1,
      sessionKey: 'web:1',
      messages: [row],
      complete: false,
      resumingTurnId: 't1',
    });
    const live = [
      { ...START, seq: 2 },
      { type: 'assistant.delta', seq: 3, turnId: 't1', text: 'still going' },
    ].reduce<Transcript>(
      (items, frame) => applyServerMessage(items, frame as ServerMessage),
      replayed,
    );

    const merged = mergeStoredHistory(live, [
      row,
      stored('row-2', 't1', {
        role: 'assistant',
        content: [{ type: 'text', text: 'an iteration that finished' }],
        toolCalls: [],
      }),
    ]);

    expect(merged.map((item) => item.kind)).toEqual(['user', 'turn']);
    expect(turnOf(merged)).toMatchObject({
      done: false,
      parts: [{ kind: 'text', text: 'still going' }],
    });
  });

  it('keeps a replayed turn storage has no row for at all', () => {
    // The common shape of the reported bug: the model delegates on its first
    // iteration, so by the time the tab reloads the parent has written nothing
    // but the question. There is no stored turn to replace — the live one falls
    // through to the append, and it has to land after the question rather than
    // be dropped for having no counterpart.
    const replayed = applyServerMessage([], {
      type: 'session.replay',
      seq: 1,
      sessionKey: 'web:1',
      messages: [row],
      complete: false,
      resumingTurnId: 't1',
    });
    const live = applyServerMessage(replayed, { ...START, seq: 2 });

    const merged = mergeStoredHistory(live, [row]);

    expect(merged.map((item) => item.kind)).toEqual(['user', 'turn']);
    expect(turnOf(merged)).toMatchObject({ id: 't1', done: false });
  });

  it('still lets storage win for a turn nothing has replayed', () => {
    // The narrowing, and the reason the flag exists rather than "live wins".
    // Without a replay behind it a live turn is a *tail*, and storage holds the
    // prefix — preferring the tail would show less than the rebuild does.
    const live = play(START, {
      type: 'assistant.delta',
      turnId: 't1',
      text: 'the second half',
    });

    const merged = mergeStoredHistory(live, [
      row,
      stored('row-2', 't1', {
        role: 'assistant',
        content: [{ type: 'text', text: 'the first half' }],
        toolCalls: [],
      }),
    ]);

    expect(turnOf(merged).parts).toEqual([
      { kind: 'text', id: 't1#0', text: 'the first half' },
    ]);
  });

  it("keeps a subagent's run, which storage does not record either", () => {
    // The history query is invalidated on `turn.end`, so this rebuild happens
    // moments after a delegation finishes. Without carrying it across, the run
    // the operator was just watching disappears in front of them — and the
    // nested transcript lives in the child's session, so nothing here can
    // rebuild it from the parent's rows.
    const live = play(
      START,
      {
        type: 'tool.call',
        turnId: 't1',
        callId: 'c1',
        name: 'ask_researcher',
        args: {},
        risk: 'safe',
      },
      {
        type: 'subagent.event',
        turnId: 't1',
        parentSessionKey: 'web:1',
        parentCallId: 'c1',
        agentId: 'researcher',
        label: 'Researcher',
        sessionKey: 'sub-1',
        depth: 1,
        event: { type: 'assistant.delta', turnId: 't2', text: 'Found it.' },
      },
    );

    const merged = mergeStoredHistory(live, [
      row,
      stored('row-2', 't1', {
        role: 'assistant',
        content: [],
        toolCalls: [{ id: 'c1', name: 'ask_researcher', argumentsJson: '{}' }],
      }),
    ]);

    expect(toolsOf(merged)[0]?.subagent?.parts).toEqual([
      { kind: 'text', id: 'sub-1#0', text: 'Found it.' },
    ]);
  });

  it('keeps the risk band the socket reported, which storage does not record', () => {
    const live = play(START, {
      type: 'tool.call',
      turnId: 't1',
      callId: 'c1',
      name: 'exec',
      args: { argv: ['node', '--version'] },
      risk: 'exec',
    });

    const merged = mergeStoredHistory(live, [
      row,
      stored('row-2', 't1', {
        role: 'assistant',
        content: [],
        toolCalls: [
          {
            id: 'c1',
            name: 'exec',
            argumentsJson: '{"argv":["node","--version"]}',
          },
        ],
      }),
    ]);

    // The row cannot say what band the call was in — that was the registry's
    // answer at call time — so the stored form fills in `safe`. Letting it win
    // would relabel an `exec` the user was asked to approve as `read`, the
    // moment its turn lands in the database.
    expect(toolsOf(merged)[0]).toMatchObject({ name: 'exec', risk: 'exec' });
  });

  it('recognises an acked bubble as the message storage just returned', () => {
    const acked = applyServerMessage(
      appendPendingUserMessage([], {
        clientMessageId: 'c-1',
        text: 'the question',
      }),
      {
        type: 'message.ack',
        seq: 1,
        sessionKey: 'web:1',
        messageId: 't1',
        clientMessageId: 'c-1',
      },
    );

    const merged = mergeStoredHistory(acked, [row]);

    expect(merged).toHaveLength(1);
    expect(merged[0]).toMatchObject({ id: 'row-1' });
  });

  it('keeps a bubble that has not been acked yet', () => {
    const pending = appendPendingUserMessage([], {
      clientMessageId: 'c-2',
      text: 'a second one',
    });

    const merged = mergeStoredHistory(pending, [row]);

    // No turn id yet, so nothing says it is the same message — and it is not.
    expect(merged).toHaveLength(2);
  });

  it('is idempotent, because the history query can resolve more than once', () => {
    const live = play(START, {
      type: 'assistant.delta',
      turnId: 't1',
      text: 'answer',
    });

    const once = mergeStoredHistory(live, [row]);
    const twice = mergeStoredHistory(once, [row]);

    expect(twice).toEqual(once);
  });
});

/**
 * `seq` defaults to a monotonic counter rather than a constant.
 *
 * Storage hands out increasing sequence numbers, and turn spans are derived
 * from them — fixtures that all shared one seq would collapse every turn's
 * span onto the same number and make the derivation untestable. Only the
 * ordering matters here, so the absolute values are left to the counter and
 * spelled out only where a test asserts one.
 */
let nextSeq = 1;

/** The one tool card a built transcript holds. */
function toolOf(transcript: Transcript): ToolPart | undefined {
  for (const item of transcript) {
    if (item.kind !== 'turn') continue;
    const part = item.parts.find((candidate) => candidate.kind === 'tool');
    if (part !== undefined) return part;
  }
  return undefined;
}

function stored(
  id: string,
  turnId: string | undefined,
  message: StoredMessage['message'],
  seq: number = nextSeq++,
): StoredMessage {
  return {
    id,
    sessionKey: 'web:1',
    seq,
    createdAtMs: 0,
    ...(turnId === undefined ? {} : { turnId }),
    message,
  };
}

describe('truncation', () => {
  it('rebuilds from the tail a session.truncated carries', () => {
    const before = fromStoredMessages([
      stored(
        'm1',
        't1',
        { role: 'user', content: [{ type: 'text', text: 'first' }] },
        1,
      ),
      stored(
        'm2',
        't1',
        {
          role: 'assistant',
          content: [{ type: 'text', text: 'answer' }],
          toolCalls: [],
        },
        2,
      ),
    ]);

    const after = applyServerMessage(before, {
      type: 'session.truncated',
      seq: 9,
      sessionKey: 'web:1',
      upToSeq: 1,
      messages: [
        stored(
          'm1',
          't1',
          { role: 'user', content: [{ type: 'text', text: 'first' }] },
          1,
        ),
      ],
    });

    // A rebuild, not a splice: the frame is the surviving history.
    expect(after).toHaveLength(1);
    expect(after[0]?.kind).toBe('user');
  });

  it('empties the transcript when everything was cut', () => {
    const before = fromStoredMessages([
      stored(
        'm1',
        't1',
        { role: 'user', content: [{ type: 'text', text: 'first' }] },
        1,
      ),
    ]);

    expect(
      applyServerMessage(before, {
        type: 'session.truncated',
        seq: 9,
        sessionKey: 'web:1',
        upToSeq: 0,
        messages: [],
      }),
    ).toEqual([]);
  });
});

describe('truncateTranscriptAfter', () => {
  const items = fromStoredMessages([
    stored(
      'm1',
      't1',
      { role: 'user', content: [{ type: 'text', text: 'one' }] },
      1,
    ),
    stored(
      'm2',
      't1',
      {
        role: 'assistant',
        content: [{ type: 'text', text: 'answer' }],
        toolCalls: [],
      },
      2,
    ),
    stored(
      'm3',
      't2',
      { role: 'user', content: [{ type: 'text', text: 'two' }] },
      3,
    ),
  ]);

  it('keeps everything at or below the cut', () => {
    expect(truncateTranscriptAfter(items, 2)).toHaveLength(2);
  });

  it('is a no-op past the end', () => {
    expect(truncateTranscriptAfter(items, 99)).toEqual(items);
  });

  it('drops everything when cut at zero', () => {
    expect(truncateTranscriptAfter(items, 0)).toEqual([]);
  });

  it('stops at the first item it cannot address', () => {
    // An optimistic bubble has no seq, so nothing after it can be shown to be
    // on the surviving side of a cut.
    const withPending = appendPendingUserMessage(items, {
      clientMessageId: 'c1',
      text: 'three',
    });
    expect(truncateTranscriptAfter(withPending, 99)).toHaveLength(4);
    expect(truncateTranscriptAfter(withPending, 2)).toHaveLength(2);
  });
});

describe('turn.end reporting', () => {
  it('records the timing and the seqs the turn spanned', () => {
    const started = applyServerMessage([], {
      type: 'turn.start',
      agentId: 'default',
      seq: 1,
      sessionKey: 'web:1',
      turnId: 't1',
      model: 'm',
      provider: 'p',
    });

    const [turn] = applyServerMessage(started, {
      type: 'turn.end',
      seq: 2,
      turnId: 't1',
      stopReason: 'complete',
      iterations: 1,
      elapsedMs: 1200,
      firstSeq: 3,
      lastSeq: 6,
    });

    expect(turn).toMatchObject({
      kind: 'turn',
      elapsedMs: 1200,
      firstSeq: 3,
      lastSeq: 6,
    });
  });

  it('gives the message that started the turn its storage address', () => {
    // The reason `firstSeq` is on the wire: without it, a bubble this tab drew
    // optimistically could not be edited or branched until a refetch — and a
    // refetch would also replace the live turn's tool timings.
    const sent = appendPendingUserMessage([], {
      clientMessageId: 'c1',
      text: 'hello',
    });
    const acked = applyServerMessage(sent, {
      type: 'message.ack',
      seq: 1,
      sessionKey: 'web:1',
      messageId: 't1',
      clientMessageId: 'c1',
    });

    const after = applyServerMessage(acked, {
      type: 'turn.end',
      seq: 2,
      turnId: 't1',
      stopReason: 'complete',
      iterations: 1,
      firstSeq: 4,
    });

    expect(after[0]).toMatchObject({ kind: 'user', seq: 4 });
  });
});

describe('a turn that failed', () => {
  it('keeps the address it was started with, so it can be re-run', () => {
    // The bug this exists for: `turn.end` carried `firstSeq` and a turn that
    // threw never reached its end, so a failed turn had no storage address —
    // the failure line advised sending the message again and the footer offered
    // no button to do it with.
    const items = play(
      { ...START, firstSeq: 7 },
      {
        type: 'error',
        code: 'internal',
        message: 'the sandbox is unreachable',
        retryable: true,
        turnId: 't1',
      },
      { type: 'turn.end', turnId: 't1', stopReason: 'error', iterations: 0 },
    );

    const turn = turnOf(items);
    expect(turn.failure?.message).toContain('sandbox');
    expect(turn.firstSeq).toBe(7);
  });

  it('stamps the optimistic user bubble at turn.start', () => {
    // Regenerate addresses the *user* message, so the bubble needs its seq even
    // when nothing after the start ever arrives.
    const pending = appendPendingUserMessage([], {
      clientMessageId: 'c-1',
      text: 'search',
    });
    const acked = applyServerMessage(pending, {
      type: 'message.ack',
      seq: 1,
      sessionKey: 'web:1',
      messageId: 't1',
      clientMessageId: 'c-1',
    });
    const started = applyServerMessage(acked, {
      ...START,
      seq: 2,
      firstSeq: 7,
    });

    const bubble = started.find((item) => item.kind === 'user');
    expect(bubble?.kind === 'user' && bubble.seq).toBe(7);
  });

  it('does not renumber a bubble a fetch already placed', () => {
    // `turn.end` stamps too, and a replayed `turn.start` must not move a seq
    // that storage has already supplied.
    const items = play(
      { ...START, firstSeq: 7 },
      {
        type: 'turn.end',
        turnId: 't1',
        stopReason: 'complete',
        iterations: 1,
        firstSeq: 9,
      },
    );

    expect(turnOf(items).firstSeq).toBe(9);
  });

  it('tolerates a server that does not report firstSeq on the start', () => {
    const items = play(START);
    expect(turnOf(items).firstSeq).toBeUndefined();
  });
});

describe('subagents', () => {
  /** One nested frame, addressed as the loop addresses it. */
  const nest = (
    event: Extract<ServerMessage, { type: 'subagent.event' }>['event'],
    over: Partial<
      Omit<Extract<ServerMessage, { type: 'subagent.event' }>, 'type' | 'seq'>
    > = {},
  ): Unsequenced<ServerMessage> => ({
    type: 'subagent.event',
    turnId: 't1',
    parentSessionKey: 'web:1',
    parentCallId: 'c1',
    agentId: 'researcher',
    label: 'Researcher',
    sessionKey: 'sub-1',
    depth: 1,
    event,
    ...over,
  });

  const DELEGATE = {
    type: 'tool.call',
    turnId: 't1',
    callId: 'c1',
    name: 'ask_researcher',
    args: { task: 'go' },
    risk: 'safe',
  } as const;

  const CHILD_START = {
    type: 'turn.start',
    agentId: 'researcher',
    sessionKey: 'sub-1',
    turnId: 't2',
    model: 'child-model',
    provider: 'p',
  } as const;

  it('hangs the run off the delegating call rather than off the turn', () => {
    const items = play(
      START,
      DELEGATE,
      nest(CHILD_START),
      nest({ type: 'assistant.delta', turnId: 't2', text: 'Found it.' }),
    );

    // One part on the turn: the card. The subagent is inside it.
    expect(turnOf(items).parts).toHaveLength(1);

    const run = toolsOf(items)[0]?.subagent;
    expect(run).toMatchObject({
      agentId: 'researcher',
      label: 'Researcher',
      sessionKey: 'sub-1',
      model: 'child-model',
      done: false,
      loaded: true,
    });
    expect(run?.parts).toEqual([
      { kind: 'text', id: 'sub-1#0', text: 'Found it.' },
    ]);
  });

  it("renders a subagent's tool call with the same shape as its caller's", () => {
    const items = play(
      START,
      DELEGATE,
      nest(CHILD_START),
      nest({
        type: 'tool.call',
        turnId: 't2',
        callId: 'n1',
        name: 'ls',
        args: { path: 'src' },
        risk: 'safe',
      }),
      nest({
        type: 'tool.result',
        turnId: 't2',
        callId: 'n1',
        ok: true,
        content: 'a.ts',
        truncated: false,
        durationMs: 12,
      }),
    );

    const nested = toolsOf(items)[0]?.subagent?.parts[0];
    expect(nested).toMatchObject({
      kind: 'tool',
      id: 'n1',
      name: 'ls',
      status: 'ok',
      content: 'a.ts',
      durationMs: 12,
    });
  });

  it('closes the run on turn.end, and keeps what it cost', () => {
    const items = play(
      START,
      DELEGATE,
      nest(CHILD_START),
      nest({
        type: 'turn.end',
        turnId: 't2',
        stopReason: 'complete',
        iterations: 3,
        elapsedMs: 4100,
        usage: { promptTokens: 1200, completionTokens: 340, totalTokens: 1540 },
      }),
    );

    expect(toolsOf(items)[0]?.subagent).toMatchObject({
      done: true,
      stopReason: 'complete',
      iterations: 3,
      elapsedMs: 4100,
    });
  });

  it('keeps a parent and a subagent call with the same id apart', () => {
    const items = play(
      START,
      // The caller's own call, and the subagent's, share an id — legal, since a
      // call id is only unique within one assistant message.
      { ...DELEGATE, callId: 'x' },
      nest(CHILD_START, { parentCallId: 'x' }),
      nest(
        {
          type: 'tool.call',
          turnId: 't2',
          callId: 'x',
          name: 'echo',
          args: {},
          risk: 'safe',
        },
        {
          parentCallId: 'x',
        },
      ),
      nest(
        {
          type: 'tool.result',
          turnId: 't2',
          callId: 'x',
          ok: true,
          content: 'inner',
          truncated: false,
          durationMs: 1,
        },
        { parentCallId: 'x' },
      ),
      // …and the caller's own result, which must not land on the nested card.
      {
        type: 'tool.result',
        turnId: 't1',
        callId: 'x',
        ok: true,
        content: 'outer',
        truncated: false,
        durationMs: 9,
      },
    );

    const outer = toolsOf(items)[0];
    expect(outer?.content).toBe('outer');
    expect(outer?.subagent?.parts).toHaveLength(1);
    expect(outer?.subagent?.parts[0]).toMatchObject({
      id: 'x',
      content: 'inner',
    });
  });

  it('nests a subagent of a subagent inside it', () => {
    const items = play(
      START,
      DELEGATE,
      nest(CHILD_START),
      // The middle agent's own delegating call, inside its run.
      nest({
        type: 'tool.call',
        turnId: 't2',
        callId: 'm1',
        name: 'ask_summariser',
        args: {},
        risk: 'safe',
      }),
      // The grandchild, addressed to the middle agent's call in its session.
      nest(
        { type: 'assistant.delta', turnId: 't3', text: 'Short.' },
        {
          parentSessionKey: 'sub-1',
          parentCallId: 'm1',
          agentId: 'summariser',
          label: 'Summariser',
          sessionKey: 'sub-2',
          depth: 2,
        },
      ),
    );

    const middle = toolsOf(items)[0]?.subagent;
    const grandchildCard = middle?.parts[0];
    expect(grandchildCard).toMatchObject({ kind: 'tool', id: 'm1' });
    expect(
      grandchildCard?.kind === 'tool' ? grandchildCard.subagent : undefined,
    ).toMatchObject({
      agentId: 'summariser',
      sessionKey: 'sub-2',
    });
  });

  it('builds the delegating card for a call it never saw', () => {
    // A reload that landed mid-delegation, which the 512-frame ring makes the
    // normal outcome rather than a corner case: every nested delta is a frame,
    // so a subagent of any length pushes its own `tool.call` out of the buffer.
    // Dropping these left the run invisible until the turn ended. The wrapper
    // carries `agentId`, and the tool a delegation runs under is derivable from
    // it, so the card can name itself rather than saying "tool".
    const items = play(START, nest(CHILD_START));

    const card = turnOf(items).parts[0];
    expect(card).toMatchObject({
      kind: 'tool',
      id: 'c1',
      name: 'ask_researcher',
    });
    expect(card?.kind === 'tool' ? card.subagent : undefined).toMatchObject({
      agentId: 'researcher',
      sessionKey: 'sub-1',
      // The half this tab caught, not the run. `withLiveRiskBands` reads this
      // and stands aside for the stored copy once the turn ends.
      partial: true,
    });
  });

  it('yields a partial run to storage once the turn has ended', () => {
    // The other end of the reload above. The card was invented from the tail of
    // a delegation; by `turn.end` the parent's rows exist and `subagentRuns`
    // names the child session, so the whole run is fetchable. Keeping the tail
    // would freeze whatever this tab happened to catch, permanently.
    const live = play(START, nest(CHILD_START));

    const merged = mergeStoredHistory(
      live,
      [
        stored('row-1', 't1', {
          role: 'user',
          content: [{ type: 'text', text: 'the question' }],
        }),
        stored('row-2', 't1', {
          role: 'assistant',
          content: [],
          toolCalls: [
            { id: 'c1', name: 'ask_researcher', argumentsJson: '{}' },
          ],
        }),
      ],
      {
        c1: { sessionKey: 'sub-1', agentId: 'researcher', label: 'Researcher' },
      },
    );

    expect(toolsOf(merged)[0]?.subagent).toMatchObject({
      sessionKey: 'sub-1',
      loaded: false,
      partial: false,
    });
  });

  it('keeps a partial run when storage has no pointer to stand aside for', () => {
    // The pointer is written best-effort — `rememberSubagentRun` logs a failed
    // write and carries on rather than failing the turn — so `subagentRuns` can
    // be missing an entry for a run that happened. Standing aside for nothing
    // made the card vanish at the moment the delegation finished, which is
    // worse than the half it was showing.
    const live = play(START, nest(CHILD_START));

    const merged = mergeStoredHistory(live, [
      stored('row-1', 't1', {
        role: 'user',
        content: [{ type: 'text', text: 'the question' }],
      }),
      stored('row-2', 't1', {
        role: 'assistant',
        content: [],
        toolCalls: [{ id: 'c1', name: 'ask_researcher', argumentsJson: '{}' }],
      }),
    ]);

    expect(toolsOf(merged)[0]?.subagent).toMatchObject({
      sessionKey: 'sub-1',
      partial: true,
    });
  });

  it("answers a subagent's approval prompt, which is drawn in the nested card", () => {
    const items = play(
      START,
      DELEGATE,
      nest(CHILD_START),
      nest({
        type: 'tool.call',
        turnId: 't2',
        callId: 'n1',
        name: 'exec',
        args: {},
        risk: 'exec',
      }),
      nest({
        type: 'tool.approvalRequest',
        turnId: 't2',
        callId: 'n1',
        name: 'exec',
        args: {},
        risk: 'exec',
        expiresAtMs: 1_700_000_300_000,
      }),
    );

    // The wire carries only the call id, so this has to find it wherever it is.
    const answered = markApprovalAnswered(items, 'n1', 'approved');
    const nested = toolsOf(answered)[0]?.subagent?.parts[0];
    expect(nested).toMatchObject({
      status: 'awaiting-approval',
      approval: { answered: 'approved' },
    });
  });

  it('does not re-run a delegation onto a turn that already finished', () => {
    const items = play(
      START,
      DELEGATE,
      nest(CHILD_START),
      nest({ type: 'assistant.delta', turnId: 't2', text: 'once' }),
      { type: 'turn.end', turnId: 't1', stopReason: 'complete', iterations: 1 },
      // The ring re-sends on a resume; the turn is closed, so this is dropped.
      nest({ type: 'assistant.delta', turnId: 't2', text: 'twice' }),
    );

    expect(toolsOf(items)[0]?.subagent?.parts).toEqual([
      { kind: 'text', id: 'sub-1#0', text: 'once' },
    ]);
  });
});
