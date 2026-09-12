/**
 * The wiring between the socket and the rest of the app.
 *
 * `socket.test.ts` owns the transport and `transcript.test.ts` owns what a frame
 * means. What is left — and what is only testable here — is the handful of
 * decisions this module makes on the way between them: which frame a switch
 * sends, when the cursor is written, when a dropped connection is worth
 * interrupting the user over, and what happens to a message typed while the
 * socket is down.
 */

import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { ClientMessage, ServerMessage } from '@ghostwire/protocol';

import { useToastStore } from '@/components/ui/toast.js';
import { useTurnStore } from '@/state/turn.js';
import { readCursor, writeCursor } from '@/lib/cursor.js';
import {
  approveTool,
  closeConnection,
  regenerateTurn,
  editMessage,
  onServerMessage,
  openConnection,
  sendUserMessage,
  steerTurn,
  stopTurn,
  switchSession,
} from '@/lib/connection.js';

class FakeSocket {
  static readonly opened: FakeSocket[] = [];

  readonly sent: ClientMessage[] = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;

  constructor(readonly url: string) {
    FakeSocket.opened.push(this);
  }

  send(data: string): void {
    this.sent.push(JSON.parse(data) as ClientMessage);
  }

  close(): void {
    this.onclose?.();
  }
}

const socket = (): FakeSocket => {
  const instance = FakeSocket.opened.at(-1);
  if (instance === undefined) throw new Error('nothing dialled');
  return instance;
};

/** Distributes over the union, which a bare `Omit` would collapse. */
type Unsequenced<T> = T extends unknown ? Omit<T, 'seq'> : never;

let seq = 0;

function deliver(frame: Unsequenced<ServerMessage>): void {
  const stamped =
    frame.type === 'connected' ||
    frame.type === 'pong' ||
    frame.type === 'error'
      ? frame
      : { ...frame, seq: (seq += 1) };
  socket().onmessage?.({ data: JSON.stringify(stamped) });
}

function open(sessionKey: string | undefined = 'web:1'): void {
  openConnection(sessionKey);
  socket().onopen?.();
}

beforeEach(() => {
  seq = 0;
  FakeSocket.opened.length = 0;
  vi.stubGlobal('WebSocket', FakeSocket);
});

describe('opening', () => {
  it('dials the session the URL named, on the page’s own origin', () => {
    open('web:7');

    expect(socket().url).toBe(
      `ws://${globalThis.location.host}/ws?session=web%3A7`,
    );
    expect(useTurnStore.getState().sessionKey).toBe('web:7');
  });

  it('sends no resume for a session this tab has never rendered', () => {
    open('web:1');

    // A cursor of zero means the REST history is the whole story; resuming
    // from it would replay the same session a second time.
    expect(socket().sent).toEqual([]);
  });

  it('resumes from the cursor a previous load left behind', () => {
    writeCursor('web:1', 12);

    open('web:1');

    expect(socket().sent).toEqual([
      { type: 'session.resume', sessionKey: 'web:1', lastSeq: 12 },
    ]);
  });

  it('takes the session the server minted when the URL named none', () => {
    open(undefined);
    deliver({
      type: 'connected',
      workspaceId: 'default',
      protocolVersion: 2,
      sessionKey: 'web:minted',
      serverTimeMs: 0,
      lastSeq: 0,
    });

    expect(useTurnStore.getState().sessionKey).toBe('web:minted');
  });
});

describe('switching', () => {
  it('resumes rather than switches, so a live turn is picked up', () => {
    open('web:1');
    writeCursor('web:2', 5);

    switchSession('web:2');

    // The hub's resume *is* a switch that also replays. `session.switch` would
    // arrive at a running turn and show nothing until the next token.
    expect(socket().sent.at(-1)).toEqual({
      type: 'session.resume',
      sessionKey: 'web:2',
      lastSeq: 5,
    });
    expect(useTurnStore.getState().sessionKey).toBe('web:2');
  });

  it('does nothing when it is already there', () => {
    open('web:1');

    switchSession('web:1');

    expect(socket().sent).toEqual([]);
  });

  it('does nothing when the URL catches up with a session the server minted', () => {
    // A tab opened without `?session=` is attached to a key the server chose,
    // and the route then writes that key into the URL. Reading that as a switch
    // would resume a session already being watched, and the ring would
    // re-deliver frames this client had just applied.
    open(undefined);
    deliver({
      type: 'connected',
      workspaceId: 'default',
      protocolVersion: 2,
      sessionKey: 'web:minted',
      serverTimeMs: 0,
      lastSeq: 0,
    });

    switchSession('web:minted');

    expect(socket().sent).toEqual([]);
  });

  it('redials when the socket is down rather than buffering the switch', () => {
    open('web:1');
    socket().onclose?.();

    switchSession('web:2');

    // Buffering it would leave the UI showing a session the server is not
    // sending events for.
    expect(FakeSocket.opened).toHaveLength(2);
    expect(socket().url).toContain('web%3A2');
  });
});

describe('speaking', () => {
  it('sends a message with an idempotency key and shows it immediately', () => {
    open('web:1');

    sendUserMessage('hello', [
      {
        mimeType: 'image/png',
        path: 'uploads/ab12cd34-shot.png',
        name: 'shot.png',
      },
    ]);

    const sent = socket().sent[0];
    expect(sent).toMatchObject({
      type: 'user.message',
      sessionKey: 'web:1',
      content: 'hello',
      // The workspace path, not a signed URL: the server reads the bytes off
      // disk when it builds the request, long after a token would have expired.
      attachments: [
        {
          mimeType: 'image/png',
          path: 'uploads/ab12cd34-shot.png',
          name: 'shot.png',
        },
      ],
      clientMessageId: expect.any(String),
    });
    expect(useTurnStore.getState().transcript[0]).toMatchObject({
      kind: 'user',
      pending: true,
    });
  });

  it('refuses to send before there is a session to send on', () => {
    openConnection(undefined);

    sendUserMessage('hello');
    stopTurn();
    steerTurn('be brief');

    expect(socket().sent).toEqual([]);
    expect(useTurnStore.getState().transcript).toEqual([]);
  });

  it('stops and steers the running turn', () => {
    open('web:1');

    stopTurn();
    steerTurn('be brief');

    expect(socket().sent).toEqual([
      { type: 'turn.stop', sessionKey: 'web:1' },
      { type: 'turn.steer', sessionKey: 'web:1', content: 'be brief' },
    ]);
  });

  it('regenerates the last turn, or one that is named', () => {
    open('web:1');

    regenerateTurn();
    regenerateTurn(3);

    expect(socket().sent).toEqual([
      { type: 'turn.regenerate', sessionKey: 'web:1' },
      { type: 'turn.regenerate', sessionKey: 'web:1', seq: 3 },
    ]);
  });

  it('keeps the question on screen while the turn it started is re-run', () => {
    open('web:1');
    // The stored question and the answer it produced, as a fetch would supply
    // them. Regenerating deletes both server-side, so anything still on screen
    // afterwards is there because this client put it there.
    deliver({
      type: 'session.replay',
      sessionKey: 'web:1',
      complete: false,
      messages: [
        {
          id: 'm1',
          sessionKey: 'web:1',
          seq: 3,
          createdAtMs: 0,
          turnId: 't1',
          message: {
            role: 'user',
            content: [{ type: 'text', text: 'why is the sky blue?' }],
          },
        },
      ],
    });

    regenerateTurn(3);

    const frame = socket().sent.at(-1);
    expect(frame).toMatchObject({
      type: 'turn.regenerate',
      sessionKey: 'web:1',
      seq: 3,
    });
    expect(frame).toHaveProperty('clientMessageId');

    // Still there, and pending — the row it came from is about to be deleted.
    expect(useTurnStore.getState().transcript).toMatchObject([
      { kind: 'user', text: 'why is the sky blue?', pending: true },
    ]);
  });

  it('does not lose the question to the truncation the server announces', () => {
    open('web:1');
    deliver({
      type: 'session.replay',
      sessionKey: 'web:1',
      complete: false,
      messages: [
        {
          id: 'm1',
          sessionKey: 'web:1',
          seq: 3,
          createdAtMs: 0,
          turnId: 't1',
          message: {
            role: 'user',
            content: [{ type: 'text', text: 'why is the sky blue?' }],
          },
        },
      ],
    });

    regenerateTurn(3);
    // What `#rewind` broadcasts: the question is deleted, and mid-regenerate the
    // surviving tail is empty. Rebuilding from it is what used to lose the
    // message — storage never lost it, the client did.
    deliver({
      type: 'session.truncated',
      sessionKey: 'web:1',
      upToSeq: 2,
      messages: [],
    });

    expect(useTurnStore.getState().transcript).toMatchObject([
      { kind: 'user', text: 'why is the sky blue?', pending: true },
    ]);
  });

  it('edits a message and shows the replacement before the server answers', () => {
    open('web:1');

    editMessage(3, 'a better question');

    const [frame] = socket().sent;
    expect(frame).toMatchObject({
      type: 'user.edit',
      sessionKey: 'web:1',
      seq: 3,
      content: 'a better question',
    });
    // The optimistic bubble, so the composer clearing is not the only feedback.
    expect(useTurnStore.getState().transcript).toMatchObject([
      { kind: 'user', text: 'a better question', pending: true },
    ]);
  });

  it('records an approval answer locally before the round trip', () => {
    open('web:1');
    deliver({
      type: 'turn.start',
      sessionKey: 'web:1',
      turnId: 't1',
      agentId: 'default',
      model: 'm',
      provider: 'p',
    });
    deliver({
      type: 'tool.call',
      turnId: 't1',
      callId: 'c1',
      name: 'exec',
      args: {},
      risk: 'exec',
    });
    deliver({
      type: 'tool.approvalRequest',
      turnId: 't1',
      callId: 'c1',
      name: 'exec',
      args: {},
      risk: 'exec',
      expiresAtMs: 1_000,
    });

    approveTool('c1', true, 'always');

    expect(socket().sent.at(-1)).toEqual({
      type: 'tool.approve',
      callId: 'c1',
      approved: true,
      scope: 'always',
    });
    // The gate is the server's; the acknowledgement is ours, and it happens on
    // the click rather than on the echo.
    const turn = useTurnStore
      .getState()
      .transcript.find((item) => item.kind === 'turn');
    const tool =
      turn?.kind === 'turn'
        ? turn.parts.find((part) => part.kind === 'tool')
        : undefined;
    expect(tool?.kind === 'tool' ? tool.approval?.answered : undefined).toBe(
      'approved',
    );
  });
});

describe('listening', () => {
  it('writes the cursor on every frame, because a reload arrives without warning', () => {
    open('web:1');

    deliver({
      type: 'session.status',
      workspaceId: 'default',
      sessionKey: 'web:1',
      busy: true,
      queueDepth: 0,
    });
    deliver({
      type: 'session.status',
      workspaceId: 'default',
      sessionKey: 'web:1',
      busy: false,
      queueDepth: 0,
    });

    expect(readCursor('web:1')).toBe(2);
  });

  it('hands every frame to its subscribers, and stops on unsubscribe', () => {
    const seen: string[] = [];
    open('web:1');
    const off = onServerMessage((message) => seen.push(message.type));

    deliver({ type: 'pong', serverTimeMs: 1 });
    off();
    deliver({ type: 'pong', serverTimeMs: 2 });

    expect(seen).toEqual(['pong']);
  });

  it('raises a toast for an error with no turn to render it on', () => {
    open('web:1');

    deliver({
      type: 'error',
      code: 'internal',
      message: 'it broke',
      retryable: false,
    });

    expect(useToastStore.getState().toasts).toHaveLength(1);
  });

  it('says nothing about an error that belongs to a turn', () => {
    open('web:1');
    deliver({
      type: 'turn.start',
      sessionKey: 'web:1',
      turnId: 't1',
      agentId: 'default',
      model: 'm',
      provider: 'p',
    });

    deliver({
      type: 'error',
      code: 'provider_error',
      message: 'upstream said no',
      retryable: true,
      turnId: 't1',
    });

    // It renders on the turn. A toast as well would be the same news twice.
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  it('reports an unreadable frame rather than silently missing events', () => {
    open('web:1');

    socket().onmessage?.({ data: '{"type":"assistant.delta"}' });

    expect(useToastStore.getState().toasts[0]).toMatchObject({
      title: 'Unreadable message from the server',
    });
  });
});

describe('losing the connection', () => {
  it('says so once, on the way down from open', () => {
    open('web:1');
    deliver({ type: 'pong', serverTimeMs: 1 });

    socket().onclose?.();

    expect(useToastStore.getState().toasts[0]).toMatchObject({
      title: 'Connection lost',
    });
    expect(useTurnStore.getState().connection).toBe('reconnecting');
  });

  it('says nothing when it never got there in the first place', () => {
    openConnection('web:1');
    socket().onclose?.();

    // A socket that reconnects six times over a lunch break should not leave
    // six toasts stacked up.
    expect(useToastStore.getState().toasts).toEqual([]);
  });

  it('goes quiet on close', () => {
    open('web:1');

    closeConnection();

    expect(useTurnStore.getState().connection).toBe('closed');
  });
});
