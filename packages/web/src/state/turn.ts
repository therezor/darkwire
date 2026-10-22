/**
 * Live turn state.
 *
 * Zustand rather than TanStack Query, because the two model opposite things.
 * Query owns *fetched* state: a request, a cache entry, a staleness rule. A
 * streaming turn is none of those — it arrives as hundreds of deltas that are
 * accumulated, not replaced, and there is no key to invalidate and no request
 * to retry. Modelling it as a query means either `setQueryData` on every token
 * or a refetch that would ask the server to repeat a stream it is already
 * sending.
 *
 * The split holds for the rest of the app too: the session list, settings and
 * files are Query; the turn in flight is here.
 *
 * What is *not* here is any decision about what a frame means — that is
 * `transcript.ts`, which is pure and tested without a store. This file is the
 * holder: it owns the cursor, the connection status and the busy flag, and it
 * hands each frame to the reducer.
 */

import { create } from 'zustand';

import type {
  Attachment,
  ServerMessage,
  StoredMessage,
  SubagentRunRef,
} from '@darkwire/protocol';

import {
  appendPendingUserMessage,
  applyServerMessage,
  markApprovalAnswered,
  markTurnRunning,
  mergeStoredHistory,
  truncateTranscriptAfter,
  EMPTY_TRANSCRIPT,
  type Transcript,
} from './transcript.js';
import type { ConnectionStatus } from '@/lib/socket.js';

export type { ConnectionStatus };

/**
 * Frames that only grow a turn already on screen. They arrive per token, so
 * they are held and applied together; every other frame applies at once, after
 * the ones held before it. None of them adds a user message or ends a step, so
 * code looking for those can read the store without flushing.
 */
const STREAMED: ReadonlySet<ServerMessage['type']> = new Set([
  'assistant.delta',
  'reasoning.delta',
  'tool.progress',
  'subagent.event',
]);

/**
 * How long a streamed frame may wait for company. One frame at 60 Hz: the
 * reader cannot see the difference, and a slow device renders once per batch
 * instead of once per token.
 */
const BATCH_MS = 16;

type Scheduler = (flush: () => void) => void;

const defaultScheduler: Scheduler = (flush) => {
  // A timer, not `requestAnimationFrame`: a hidden tab gets no frames at all,
  // and a held batch would grow there until the tab came back.
  setTimeout(flush, BATCH_MS);
};

let schedule: Scheduler = defaultScheduler;
let held: ServerMessage[] = [];
let flushQueued = false;

/** Test seam: run held frames synchronously, or under the test's control. */
export function setFrameScheduler(next: Scheduler | undefined): void {
  schedule = next ?? defaultScheduler;
  flushQueued = false;
}

/** Applies every held frame now. Anything that reads the transcript calls it. */
export function flushFrames(): void {
  flushQueued = false;
  if (held.length === 0) return;
  const batch = held;
  held = [];
  useTurnStore.setState((state) => {
    const transcript = reduceFrames(state.transcript, batch);
    return transcript === state.transcript ? state : { transcript };
  });
}

function reduceFrames(
  transcript: Transcript,
  frames: readonly ServerMessage[],
): Transcript {
  let next = transcript;
  for (const frame of frames) next = applyServerMessage(next, frame);
  return next;
}

/** Takes the held frames, for an update that applies them itself. */
function takeHeld(): readonly ServerMessage[] {
  const batch = held;
  held = [];
  return batch;
}

interface TurnState {
  /** The session the socket is attached to, or `undefined` before the first. */
  readonly sessionKey: string | undefined;
  /**
   * The workspace the server says this session is in.
   *
   * Reported rather than requested: switching to an existing session moves you
   * to *its* workspace, and this is how the UI finds out. `undefined` until the
   * first frame — the switcher shows its own choice in the meantime.
   */
  readonly workspaceId: string | undefined;
  readonly connection: ConnectionStatus;
  /** True between `turn.start` and `turn.end` — what drives send ⇄ stop. */
  readonly busy: boolean;
  /**
   * The turn Stop names, from its `turn.start` or a busy `session.status`
   * until its `turn.end`.
   *
   * Named so a click that lands as the turn ends cannot stop the queued one
   * that starts next.
   */
  readonly runningTurnId: string | undefined;
  /** Messages accepted and not yet started. Shown beside the composer. */
  readonly queueDepth: number;
  /** The last server `seq` applied, which a reconnect resumes from. */
  readonly lastSeq: number;
  /**
   * The server's clock minus this one's, taken at connect. An approval
   * deadline is in server time, and the two clocks need not agree.
   */
  readonly clockOffsetMs: number;
  readonly transcript: Transcript;

  readonly setConnection: (connection: ConnectionStatus) => void;
  readonly setBusy: (busy: boolean) => void;
  readonly attach: (sessionKey: string) => void;
  readonly applySeq: (seq: number) => void;
  /** One server frame: the cursor, the status flags and the transcript. */
  readonly apply: (message: ServerMessage) => void;
  /** The optimistic bubble, before the ack that confirms it. */
  readonly appendPending: (input: {
    readonly clientMessageId: string;
    readonly text: string;
    readonly attachments?: readonly Attachment[];
    /** Sent to the running turn rather than queued behind it. */
    readonly steering?: boolean;
  }) => void;
  /**
   * Drops everything after `seq`, while a regenerate or edit is in flight.
   *
   * Optimistic only. `session.truncated` rebuilds the transcript from the
   * stored tail a moment later, and that frame — not this — is the truth.
   */
  readonly truncateAfter: (seq: number) => void;
  /** This tab answered an approval prompt; the buttons go now, not on the echo. */
  readonly answerApproval: (
    callId: string,
    answered: 'approved' | 'denied',
  ) => void;
  /** The server refused this tab's answer; the prompt opens again. */
  readonly rejectApproval: (callId: string, message: string) => void;
  /** Puts a fetched history under whatever the socket has already built. */
  readonly mergeHistory: (
    messages: readonly StoredMessage[],
    subagentRuns?: Readonly<Record<string, SubagentRunRef>>,
    failures?: Readonly<Record<string, string>>,
  ) => void;
  readonly setTranscript: (transcript: Transcript) => void;
  readonly reset: () => void;
}

const INITIAL = {
  sessionKey: undefined,
  workspaceId: undefined,
  connection: 'closed' as ConnectionStatus,
  busy: false,
  runningTurnId: undefined,
  queueDepth: 0,
  lastSeq: 0,
  clockOffsetMs: 0,
  transcript: EMPTY_TRANSCRIPT,
};

export const useTurnStore = create<TurnState>((set) => ({
  ...INITIAL,

  setConnection: (connection) => {
    set({ connection });
  },
  setBusy: (busy) => {
    set({ busy });
  },
  attach: (sessionKey) => {
    // A different session is a different replay buffer: carrying `lastSeq`
    // across would resume from a sequence number that means nothing there. The
    // transcript goes with it for the same reason, and so do its held frames.
    if (useTurnStore.getState().sessionKey !== sessionKey) held = [];
    set((state) =>
      state.sessionKey === sessionKey
        ? {}
        : {
            sessionKey,
            lastSeq: 0,
            busy: false,
            runningTurnId: undefined,
            queueDepth: 0,
            transcript: EMPTY_TRANSCRIPT,
          },
    );
  },
  applySeq: (seq) => {
    // Monotonic: a replayed frame that arrives after a live one must not move
    // the cursor backwards, or the next resume asks for events twice.
    set((state) => (seq > state.lastSeq ? { lastSeq: seq } : state));
  },

  apply: (message) => {
    if (STREAMED.has(message.type)) {
      held.push(message);
      // The cursor stays exact per frame. The socket reads it to resume.
      if ('seq' in message) useTurnStore.getState().applySeq(message.seq);
      if (!flushQueued) {
        flushQueued = true;
        schedule(flushFrames);
      }
      return;
    }

    const batch = takeHeld();
    set((state) => {
      // Mutable because it is built up across a switch. `TurnState` is readonly
      // for its consumers, which is a different question from how a patch to it
      // is assembled.
      const next: { -readonly [K in keyof TurnState]?: TurnState[K] } = {};

      if ('seq' in message && message.seq > state.lastSeq) {
        next.lastSeq = message.seq;
      }
      // Older than a frame already applied: the ring re-sending history after
      // a resume announced the session as it is now. Its transcript still
      // counts. What it says about whether a turn is running no longer does.
      const stale = 'seq' in message && message.seq < state.lastSeq;

      switch (message.type) {
        case 'connected':
          // The server names the session when the client did not — a fresh tab
          // gets its key here and nowhere else.
          next.sessionKey = message.sessionKey;
          next.workspaceId = message.workspaceId;
          next.clockOffsetMs = message.serverTimeMs - Date.now();
          // A server that restarted counts from zero again. Keeping the higher
          // number would resume from a seq it never emitted.
          if (message.lastSeq < state.lastSeq) next.lastSeq = message.lastSeq;
          break;
        case 'session.status':
          if (stale) break;
          next.busy = message.busy;
          // A tab that attached mid-turn never saw its `turn.start`.
          if (!message.busy) {
            next.runningTurnId = undefined;
          } else if (message.turnId !== undefined) {
            next.runningTurnId = message.turnId;
          }
          next.queueDepth = message.queueDepth;
          // Restated on every switch, which is the point: opening someone
          // else's session moves the UI to that session's workspace rather
          // than showing its transcript beside another workspace's files.
          next.workspaceId = message.workspaceId;
          break;
        case 'turn.start':
          if (stale) break;
          next.busy = true;
          next.runningTurnId = message.turnId;
          break;
        case 'turn.end':
          if (!stale && message.turnId === state.runningTurnId) {
            next.runningTurnId = undefined;
          }
          break;
        default:
          break;
      }

      let transcript = applyServerMessage(
        reduceFrames(state.transcript, batch),
        message,
      );
      const running =
        'runningTurnId' in next ? next.runningTurnId : state.runningTurnId;
      if (running !== undefined) {
        transcript = markTurnRunning(transcript, running);
      }
      if (transcript !== state.transcript) next.transcript = transcript;

      return next;
    });
  },

  appendPending: (input) => {
    flushFrames();
    set((state) => ({
      transcript: appendPendingUserMessage(state.transcript, input),
    }));
  },

  truncateAfter: (seq) => {
    flushFrames();
    set((state) => ({
      transcript: truncateTranscriptAfter(state.transcript, seq),
    }));
  },

  answerApproval: (callId, answered) => {
    flushFrames();
    set((state) => ({
      transcript: markApprovalAnswered(state.transcript, callId, answered),
    }));
  },

  rejectApproval: (callId, message) => {
    flushFrames();
    set((state) => ({
      transcript: markApprovalAnswered(
        state.transcript,
        callId,
        undefined,
        message,
      ),
    }));
  },

  mergeHistory: (messages, subagentRuns, failures) => {
    flushFrames();
    set((state) => {
      const merged = mergeStoredHistory(
        state.transcript,
        messages,
        subagentRuns,
        failures,
      );
      return {
        transcript:
          state.runningTurnId === undefined
            ? merged
            : markTurnRunning(merged, state.runningTurnId),
      };
    });
  },

  setTranscript: (transcript) => {
    held = [];
    set({ transcript });
  },

  reset: () => {
    held = [];
    flushQueued = false;
    set(INITIAL);
  },
}));
