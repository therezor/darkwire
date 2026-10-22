/**
 * The live-turn store.
 *
 * Two of these four actions carry a rule that the socket depends on and
 * that would be invisible if it broke: `lastSeq` must never move backwards, and
 * switching sessions must not carry a sequence number into a different replay
 * buffer. Both produce the same symptom — a reconnect that replays the wrong
 * events, or none — long after the mistake.
 */

import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import type { ServerMessage } from '@darkwire/protocol';

import { flushFrames, setFrameScheduler, useTurnStore } from '@/state/turn.js';

const state = () => useTurnStore.getState();

describe('the turn store', () => {
  it('starts closed, idle and unattached', () => {
    expect(state()).toMatchObject({
      connection: 'closed',
      busy: false,
      lastSeq: 0,
      sessionKey: undefined,
    });
  });

  it('tracks the socket and the turn independently', () => {
    state().setConnection('reconnecting');
    state().setBusy(true);

    expect(state().connection).toBe('reconnecting');
    // A dropped socket does not end the turn: the server is still running it,
    // and the replay buffer is what the reconnect is for.
    expect(state().busy).toBe(true);
  });

  it('only moves lastSeq forward', () => {
    state().applySeq(7);
    state().applySeq(3);

    // A replayed frame arriving after a live one must not rewind the cursor,
    // or the next resume asks the server for events it already applied.
    expect(state().lastSeq).toBe(7);

    state().applySeq(8);
    expect(state().lastSeq).toBe(8);
  });

  it('resets the cursor when attaching to a different session', () => {
    state().attach('web:1');
    state().applySeq(12);
    state().setBusy(true);

    state().attach('web:2');

    expect(state()).toMatchObject({
      sessionKey: 'web:2',
      lastSeq: 0,
      busy: false,
    });
  });

  it('names the running turn from its start until its own end', () => {
    state().attach('web:1');
    const start = (turnId: string, seq: number) => ({
      type: 'turn.start' as const,
      seq,
      sessionKey: 'web:1',
      turnId,
      agentId: 'default',
      model: 'm',
      provider: 'p',
    });

    state().apply(start('t1', 1));
    expect(state().runningTurnId).toBe('t1');

    // Another turn's end leaves it named.
    state().apply({
      type: 'turn.end',
      seq: 2,
      turnId: 't0',
      stopReason: 'complete',
      iterations: 1,
    });
    expect(state().runningTurnId).toBe('t1');

    state().apply({
      type: 'turn.end',
      seq: 3,
      turnId: 't1',
      stopReason: 'complete',
      iterations: 1,
    });
    expect(state().runningTurnId).toBeUndefined();

    state().apply(start('t2', 4));
    state().apply({
      type: 'session.status',
      seq: 5,
      sessionKey: 'web:1',
      workspaceId: 'default',
      busy: false,
      queueDepth: 0,
    });
    expect(state().runningTurnId).toBeUndefined();

    // A tab that attached mid-turn learns the id from the status instead.
    state().apply({
      type: 'session.status',
      seq: 6,
      sessionKey: 'web:1',
      workspaceId: 'default',
      busy: true,
      queueDepth: 0,
      turnId: 't3',
    });
    expect(state().runningTurnId).toBe('t3');
  });

  it('ignores a status older than one it already applied', () => {
    state().attach('web:1');
    const status = (seq: number, busy: boolean) =>
      ({
        type: 'session.status',
        seq,
        sessionKey: 'web:1',
        workspaceId: 'default',
        busy,
        queueDepth: 0,
        ...(busy ? { turnId: 't2' } : {}),
      }) as const;

    // A resume announces the session as it is now, then the ring re-sends
    // what came before, an idle status from the last turn's end among it.
    state().apply(status(10, true));
    state().apply(status(4, false));

    expect(state()).toMatchObject({ busy: true, runningTurnId: 't2' });
  });

  it('leaves everything alone when re-attaching to the same session', () => {
    state().attach('web:1');
    state().applySeq(12);
    state().setBusy(true);

    // A re-render or a second `attach` for the session already open must not
    // discard the cursor of a turn that is still streaming.
    state().attach('web:1');

    expect(state()).toMatchObject({
      sessionKey: 'web:1',
      lastSeq: 12,
      busy: true,
    });
  });
});

describe('streamed frames', () => {
  const start = {
    type: 'turn.start',
    seq: 1,
    sessionKey: 'web:1',
    turnId: 't1',
    agentId: 'default',
    model: 'm',
    provider: 'p',
  } as const satisfies ServerMessage;
  const delta = (seq: number, text: string): ServerMessage => ({
    type: 'assistant.delta',
    seq,
    turnId: 't1',
    text,
  });
  const textOf = (): string => {
    const turn = state().transcript.find((item) => item.kind === 'turn');
    if (turn?.kind !== 'turn') return '';
    return turn.parts
      .map((part) => (part.kind === 'text' ? part.text : ''))
      .join('');
  };

  let queued = 0;
  beforeEach(() => {
    queued = 0;
    setFrameScheduler(() => {
      queued += 1;
    });
    state().attach('web:1');
    state().apply(start);
  });
  afterEach(() => {
    setFrameScheduler(undefined);
  });

  it('applies a run of deltas in one update', () => {
    let updates = 0;
    const unsubscribe = useTurnStore.subscribe((next, previous) => {
      if (next.transcript !== previous.transcript) updates += 1;
    });
    for (let seq = 2; seq <= 101; seq += 1) state().apply(delta(seq, 'x'));

    expect(textOf()).toBe('');
    expect(queued).toBe(1);
    flushFrames();
    unsubscribe();

    expect(textOf()).toBe('x'.repeat(100));
    expect(updates).toBe(1);
  });

  it('keeps the cursor exact while frames are held', () => {
    state().apply(delta(2, 'a'));
    state().apply(delta(3, 'b'));

    // A reconnect resumes from here, so it cannot wait for the flush.
    expect(state().lastSeq).toBe(3);
  });

  it('applies held frames before the next frame that is not streamed', () => {
    state().apply(delta(2, 'the answer'));
    state().apply({
      type: 'turn.end',
      seq: 3,
      turnId: 't1',
      stopReason: 'complete',
      iterations: 1,
    });

    // `turn.end` finishes the turn, and a finished turn drops any growth, so
    // the delta has to land first.
    expect(textOf()).toBe('the answer');
  });

  it('applies held frames before a change made on this tab', () => {
    state().apply(delta(2, 'streamed'));
    state().appendPending({ clientMessageId: 'c1', text: 'next' });

    expect(textOf()).toBe('streamed');
    expect(state().transcript.at(-1)).toMatchObject({ kind: 'user' });
  });

  it('drops held frames when moving to another session', () => {
    state().apply(delta(2, 'from the old session'));
    state().attach('web:2');
    flushFrames();

    expect(state().transcript).toEqual([]);
  });
});
