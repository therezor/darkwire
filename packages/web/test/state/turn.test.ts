/**
 * The live-turn store.
 *
 * Two of these four actions carry a rule that the socket depends on and
 * that would be invisible if it broke: `lastSeq` must never move backwards, and
 * switching sessions must not carry a sequence number into a different replay
 * buffer. Both produce the same symptom — a reconnect that replays the wrong
 * events, or none — long after the mistake.
 */

import { describe, expect, it } from 'vitest';

import { useTurnStore } from '@/state/turn.js';

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
