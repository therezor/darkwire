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
} from '@ghostwire/protocol';

import {
  appendPendingUserMessage,
  applyServerMessage,
  markApprovalAnswered,
  mergeStoredHistory,
  truncateTranscriptAfter,
  EMPTY_TRANSCRIPT,
  type Transcript,
} from './transcript.js';
import type { ConnectionStatus } from '@/lib/socket.js';

export type { ConnectionStatus };

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
  /** Messages accepted and not yet started. Shown beside the composer. */
  readonly queueDepth: number;
  /** The last server `seq` applied, which a reconnect resumes from. */
  readonly lastSeq: number;
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
  queueDepth: 0,
  lastSeq: 0,
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
    // transcript goes with it for the same reason.
    set((state) =>
      state.sessionKey === sessionKey
        ? {}
        : {
            sessionKey,
            lastSeq: 0,
            busy: false,
            queueDepth: 0,
            transcript: EMPTY_TRANSCRIPT,
          },
    );
  },
  applySeq: (seq) => {
    // Monotonic: a replayed frame that arrives after a live one must not move
    // the cursor backwards, or the next resume asks for events twice.
    set((state) => (seq > state.lastSeq ? { lastSeq: seq } : {}));
  },

  apply: (message) => {
    set((state) => {
      // Mutable because it is built up across a switch. `TurnState` is readonly
      // for its consumers, which is a different question from how a patch to it
      // is assembled.
      const next: { -readonly [K in keyof TurnState]?: TurnState[K] } = {};

      if ('seq' in message && message.seq > state.lastSeq) {
        next.lastSeq = message.seq;
      }

      switch (message.type) {
        case 'connected':
          // The server names the session when the client did not — a fresh tab
          // gets its key here and nowhere else.
          next.sessionKey = message.sessionKey;
          next.workspaceId = message.workspaceId;
          break;
        case 'session.status':
          next.busy = message.busy;
          next.queueDepth = message.queueDepth;
          // Restated on every switch, which is the point: opening someone
          // else's session moves the UI to that session's workspace rather
          // than showing its transcript beside another workspace's files.
          next.workspaceId = message.workspaceId;
          break;
        case 'turn.start':
          next.busy = true;
          break;
        default:
          break;
      }

      const transcript = applyServerMessage(state.transcript, message);
      if (transcript !== state.transcript) next.transcript = transcript;

      return next;
    });
  },

  appendPending: (input) => {
    set((state) => ({
      transcript: appendPendingUserMessage(state.transcript, input),
    }));
  },

  truncateAfter: (seq) => {
    set((state) => ({
      transcript: truncateTranscriptAfter(state.transcript, seq),
    }));
  },

  answerApproval: (callId, answered) => {
    set((state) => ({
      transcript: markApprovalAnswered(state.transcript, callId, answered),
    }));
  },

  mergeHistory: (messages, subagentRuns, failures) => {
    set((state) => ({
      transcript: mergeStoredHistory(
        state.transcript,
        messages,
        subagentRuns,
        failures,
      ),
    }));
  },

  setTranscript: (transcript) => {
    set({ transcript });
  },

  reset: () => {
    set(INITIAL);
  },
}));
