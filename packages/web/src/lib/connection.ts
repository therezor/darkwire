/**
 * The one socket, and the functions that speak on it.
 *
 * Plain module functions over a singleton rather than a hook, for the same
 * reason `toast()` is: the callers are not all components. The composer sends a
 * message, a tool card answers an approval, a keyboard shortcut stops a turn,
 * and the shell owns the lifecycle — a `useConnection()` returning a `send`
 * would have to be threaded through every one of them, and would hand each a
 * different closure to hold stale state in.
 *
 * The lifecycle is `useConnection` in `chat/use-connection.ts`; this file is
 * what it drives. The split matters because the socket must outlive the chat
 * route: navigating to Settings and back should not drop a turn in flight, and
 * the shell — the router's root — is the only component that never remounts.
 *
 * ## Who resumes
 *
 * `open()` dials without a `session.resume`. A first connection has nothing to
 * resume: `lastSeq` is 0, and asking to replay from 0 would ask for the whole
 * ring, which is the same conversation the REST history is already loading.
 * Every *re*-connection sends one, because that is the case the ring exists
 * for — a tab that reloaded mid-turn rebuilds the in-flight answer from it.
 */

import {
  newUuid,
  type ApprovalScope,
  type Attachment,
  type ServerMessage,
} from '@ghostwire/protocol';

import { useTurnStore } from '@/state/turn.js';
import { toast } from '@/components/ui/toast.js';
import { readCursor, writeCursor } from './cursor.js';
import {
  ReconnectingSocket,
  socketUrl,
  type ConnectionStatus,
} from './socket.js';

type Listener = (message: ServerMessage) => void;

const listeners = new Set<Listener>();

let socket: ReconnectingSocket | undefined;
/** The session the *URL* asked for, which is not always the one attached yet. */
let requested: string | undefined;
/**
 * The last `seq` at which everything before it was in storage.
 *
 * Frozen for the length of a turn, because a turn in flight is precisely the
 * state storage does not hold. See `handleMessage`, which is the only writer,
 * and `cursor.ts`, which is where it goes.
 */
let durableSeq = 0;
/**
 * What a *page load* resumes from, as opposed to a reconnect.
 *
 * `undefined` means this tab has no record of the session it just attached to,
 * which is a first visit and not a reload.
 */
let resumeFloor: number | undefined;
/** Whether the socket has been open before. See `onOpen`. */
let reconnecting = false;

/**
 * Subscribe to every parsed frame.
 *
 * The store already applies them; this is for the effects that are not state —
 * invalidating the session list when a turn ends, raising a toast for a
 * notification. Returns the unsubscribe.
 */
export function onServerMessage(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Opens the socket on `sessionKey`, or on a server-minted one when absent. */
export function openConnection(sessionKey: string | undefined): void {
  requested = sessionKey;
  if (sessionKey !== undefined) attachWithCursor(sessionKey);

  socket ??= new ReconnectingSocket({
    url: () =>
      socketUrl(
        requested ?? useTurnStore.getState().sessionKey,
        globalThis.location,
      ),
    onStatus: handleStatus,
    onMessage: handleMessage,
    onOpen: (send) => {
      const { sessionKey: attached, lastSeq } = useTurnStore.getState();
      if (attached === undefined) return;

      // Two openings, two different questions, and the same frame answers both.
      //
      // A *reconnect* keeps its page: the transcript is still on screen and the
      // gap is only what arrived while the socket was down, which is `lastSeq`.
      // Asking for anything earlier would re-deliver frames already applied,
      // and a delta applied twice is text rendered twice.
      //
      // A *page load* has no transcript at all, so the gap is everything
      // storage cannot supply — the boundary `handleMessage` has been keeping,
      // read back out of `sessionStorage`. Absent means this tab has never
      // rendered this conversation, and there the REST history is the whole
      // story: replaying a ring full of completed turns on top of it would
      // render each of them twice.
      if (reconnecting) {
        if (lastSeq > 0) {
          send({ type: 'session.resume', sessionKey: attached, lastSeq });
        }
      } else if (resumeFloor !== undefined) {
        send({
          type: 'session.resume',
          sessionKey: attached,
          lastSeq: resumeFloor,
        });
      }
      reconnecting = true;
    },
    onInvalidFrame: (reason) => {
      // A server this client cannot read is a version skew, and the honest
      // report names it rather than letting the UI quietly miss events.
      toast.error('Unreadable message from the server', reason);
    },
  });

  socket.open();
}

export function closeConnection(): void {
  socket?.close();
  socket = undefined;
  requested = undefined;
}

/**
 * Points the connection at another conversation.
 *
 * `session.resume` rather than `session.switch`, and not by accident: the hub's
 * resume *is* a switch that also replays, so one frame both moves the
 * connection and picks up a turn that is already running on the session being
 * opened. `session.switch` would arrive at a live conversation and show
 * nothing until the next token.
 *
 * A re-dial when the socket is down, because the frame needs a connection to
 * travel on and buffering it would leave the UI showing a session the server is
 * not sending events for.
 */
export function switchSession(sessionKey: string): void {
  // The condition is "is the connection already on this conversation", and it
  // has to read the *store*, not just what was last requested. A tab that
  // opened without a `?session=` is attached to a key the server minted, and
  // the URL only catches up a moment later — asking to switch to the session
  // already being watched would resume it, and the ring would re-deliver
  // frames this client had already applied.
  if (
    requested === sessionKey ||
    useTurnStore.getState().sessionKey === sessionKey
  ) {
    requested = sessionKey;
    return;
  }
  requested = sessionKey;
  const lastSeq = attachWithCursor(sessionKey);

  if (socket === undefined) return;
  if (socket.status === 'open') {
    socket.send({ type: 'session.resume', sessionKey, lastSeq });
    return;
  }
  socket.reconnectNow();
}

/**
 * Attaches to a session and restores the cursor this tab last had on it.
 *
 * The cursor is what makes a reload different from a fresh visit: without it,
 * `lastSeq` is 0 and the in-flight turn the user was watching is unrecoverable,
 * because storage does not hold a half-written answer.
 */
function attachWithCursor(sessionKey: string): number {
  const store = useTurnStore.getState();
  store.attach(sessionKey);

  const cursor = readCursor(sessionKey);
  store.applySeq(cursor ?? 0);
  // The stored cursor is this tab's new floor for the session it just attached
  // to. Carrying the previous session's number across would freeze the wrong
  // boundary — and for a session with no stored cursor, zero is the honest
  // answer: nothing here is recoverable from storage yet.
  durableSeq = cursor ?? 0;
  resumeFloor = cursor;
  return useTurnStore.getState().lastSeq;
}

/**
 * Starts a conversation, and hands back the key it will have.
 *
 * **Nothing is persisted here, and that is the point.** A row created the
 * moment someone presses New session is a row that survives them changing their
 * mind, so a sidebar fills with empty conversations nobody had. The hub only
 * moves this connection; `AgentLoop.run` calls `ensureSession` when the first
 * message lands, which is the first moment there is a conversation to save.
 *
 * The key is minted here rather than by the server because the click has to
 * navigate to it, and `session.new` answers asynchronously. The frame still
 * carries it so the hub attaches the connection — and carries the workspace,
 * which is the one thing this frame can do that `session.switch` cannot: it
 * re-points `connection.workspaceId`, and that is what decides where the
 * conversation is created when it finally is.
 *
 * The agent rides along for the same reason and with the same timing: a session
 * is bound to one when it is created, and after that the binding is the stored
 * row's — moving it is an explicit `PATCH /api/sessions/:key`, not a frame.
 */
export function newSession(workspaceId?: string, agentId?: string): string {
  const sessionKey = newUuid();
  socket?.send({
    type: 'session.new',
    sessionKey,
    ...(workspaceId === undefined ? {} : { workspaceId }),
    ...(agentId === undefined ? {} : { agentId }),
  });
  return sessionKey;
}

/**
 * Sends a message, and says which agent it should run on.
 *
 * The agent is carried on every message rather than only on the first, because
 * the socket may have minted this session key without a `session.new` frame —
 * a fresh tab attaches and the user simply types. Sending it always is safe:
 * the server ignores it for a session that already has a row, deliberately, so
 * a history cannot drift onto another agent's prompt and tools by accident.
 */
export function sendUserMessage(
  text: string,
  attachments: readonly Attachment[] = [],
  agentId?: string,
): void {
  const store = useTurnStore.getState();
  const sessionKey = store.sessionKey;
  if (sessionKey === undefined || socket === undefined) return;

  // The idempotency key. A retry after a dropped socket — which the buffer in
  // `socket.ts` makes routine — is acked with the id the first attempt got,
  // and no second turn is queued.
  const clientMessageId = newUuid();

  store.appendPending({ clientMessageId, text, attachments });
  socket.send({
    type: 'user.message',
    sessionKey,
    content: text,
    attachments: [...attachments],
    clientMessageId,
    ...(agentId === undefined ? {} : { agentId }),
  });
}

export function stopTurn(): void {
  const sessionKey = useTurnStore.getState().sessionKey;
  if (sessionKey === undefined) return;
  socket?.send({ type: 'turn.stop', sessionKey });
}

export function steerTurn(content: string): void {
  const sessionKey = useTurnStore.getState().sessionKey;
  if (sessionKey === undefined) return;
  socket?.send({ type: 'turn.steer', sessionKey, content });
}

/**
 * Re-runs a turn, discarding the answer it produced.
 *
 * Cuts *below* the question and re-adds it as a pending bubble, which is the
 * same shape as `editMessage` and for the same reason. The server deletes that
 * row — `#rewind` truncates to `seq - 1`, because the loop appends the question
 * again at the top of every turn — and announces the deletion with a
 * `session.truncated` carrying the surviving tail. Keeping the stored bubble on
 * screen across that frame is not possible: the row it came from is gone, and
 * the rebuild from the tail drops it. An optimistic bubble is the only kind that
 * survives the gap, and `message.ack` reconciles it against the rewritten row a
 * moment later.
 */
export function regenerateTurn(seq?: number): void {
  const store = useTurnStore.getState();
  const sessionKey = store.sessionKey;
  if (sessionKey === undefined || socket === undefined) return;

  // Absent `seq` means "the most recent turn", which the server resolves — there
  // is nothing named here to stand in for. A `seq` that matches nothing is the
  // same case: send the frame and let the server refuse it if it is wrong.
  const question =
    seq === undefined
      ? undefined
      : store.transcript.find(
          (item) => item.kind === 'user' && item.seq === seq,
        );

  const clientMessageId = newUuid();
  if (seq !== undefined && question?.kind === 'user') {
    store.truncateAfter(seq - 1);
    store.appendPending({
      clientMessageId,
      text: question.text,
      attachments: question.attachments,
    });
  }

  socket.send({
    type: 'turn.regenerate',
    sessionKey,
    ...(seq === undefined ? {} : { seq }),
    ...(question === undefined ? {} : { clientMessageId }),
  });
}

/** Replaces a message and re-runs from it. */
export function editMessage(
  seq: number,
  text: string,
  attachments: readonly Attachment[] = [],
): void {
  const store = useTurnStore.getState();
  const sessionKey = store.sessionKey;
  if (sessionKey === undefined || socket === undefined) return;

  const clientMessageId = newUuid();
  // Below the edited message, because the replacement is appended next: cutting
  // at `seq` would leave the old wording above the new one.
  store.truncateAfter(seq - 1);
  store.appendPending({ clientMessageId, text, attachments });
  socket.send({
    type: 'user.edit',
    sessionKey,
    seq,
    content: text,
    attachments: [...attachments],
    clientMessageId,
  });
}

export function approveTool(
  callId: string,
  approved: boolean,
  scope: ApprovalScope,
): void {
  // Recorded locally first, so the buttons go on the click rather than on the
  // round trip. The gate is the server's; the acknowledgement is ours.
  useTurnStore
    .getState()
    .answerApproval(callId, approved ? 'approved' : 'denied');
  socket?.send({ type: 'tool.approve', callId, approved, scope });
}

/** Test seam: the socket is module state, and a suite needs it absent per case. */
export function resetConnection(): void {
  socket?.close();
  socket = undefined;
  requested = undefined;
  durableSeq = 0;
  resumeFloor = undefined;
  reconnecting = false;
  listeners.clear();
}

function handleStatus(status: ConnectionStatus): void {
  const previous = useTurnStore.getState().connection;
  useTurnStore.getState().setConnection(status);

  // Only on the transition, and only downwards: a socket that reconnects six
  // times over a lunch break should not leave six toasts stacked up.
  if (status === 'reconnecting' && previous === 'open') {
    toast({
      title: 'Connection lost',
      description: 'Reconnecting. The turn keeps running on the server.',
      role: 'warning',
      durationMs: 4000,
    });
  }
}

function handleMessage(message: ServerMessage): void {
  const store = useTurnStore.getState();
  store.apply(message);

  // Written here rather than on a schedule: the whole value of the cursor is
  // that it is correct at an arbitrary moment, because the moment it has to
  // survive — a reload — arrives without warning. See `cursor.ts`.
  //
  // The value is *not* `lastSeq`, and the difference is the whole of what makes
  // a reload recoverable. `lastSeq` means "the last frame I applied", which is
  // the right question for a reconnect: the page is still there, the transcript
  // is still in memory, and the gap is only what arrived while the socket was
  // down. A reload asks a different question — the transcript is gone, and
  // storage cannot supply the turn that has not finished. Resuming from
  // `lastSeq` there asks the ring for the frames *after* the ones this tab has
  // just forgotten, which is nothing, and the in-flight turn is lost.
  //
  // So what is persisted is the boundary between the two sources: the last
  // point at which everything before it is in storage. That is exactly "not
  // mid-turn", which the store already knows as `busy`.
  const { sessionKey, lastSeq, busy } = useTurnStore.getState();
  if (!busy) durableSeq = lastSeq;
  // Written even when the boundary is still zero. A zero *entry* is the record
  // that this tab has rendered this conversation, and the only case that
  // produces one is a turn that started before any other frame arrived — which
  // is a session's first turn, and the one a reload would otherwise lose
  // outright. `cursor.ts` is where the absence and the zero are told apart.
  if (sessionKey !== undefined) writeCursor(sessionKey, durableSeq);

  // A connection-scoped error has no turn to attach to, so it has nowhere to
  // render — a toast is the only place it can be seen at all.
  if (message.type === 'error' && message.turnId === undefined) {
    toast.error('Server error', message.message);
  }

  for (const listener of listeners) listener(message);
}
