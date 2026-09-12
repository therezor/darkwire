/**
 * The transport, driven with a fake socket.
 *
 * Everything worth testing here is a *schedule* or a *decision about a frame*,
 * and both are exactly what a real WebSocket makes untestable: a server that
 * drops a connection at the right moment, a delay that is supposed to grow, a
 * frame the schema should refuse. So `create` and `random` are injected, the
 * timers are fake, and the assertions are about behaviour that would otherwise
 * only be observable by watching a browser for twenty seconds.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ServerMessage } from '@ghostwire/protocol';

import {
  ReconnectingSocket,
  socketUrl,
  type ReconnectingSocketOptions,
  type SocketLike,
} from '@/lib/socket.js';

/** A socket that does exactly what it is told and remembers what it was sent. */
class FakeSocket implements SocketLike {
  static readonly opened: FakeSocket[] = [];

  readonly sent: string[] = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;
  closed = false;

  constructor(readonly url: string) {
    FakeSocket.opened.push(this);
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.closed = true;
  }

  /** What a browser does when the handshake succeeds. */
  open(): void {
    this.onopen?.();
  }

  deliver(message: unknown): void {
    this.onmessage?.({
      data: typeof message === 'string' ? message : JSON.stringify(message),
    });
  }

  drop(): void {
    this.closed = true;
    this.onclose?.();
  }
}

const CONNECTED: ServerMessage = {
  type: 'connected',
  workspaceId: 'default',
  protocolVersion: 2,
  sessionKey: 'web:1',
  serverTimeMs: 0,
  lastSeq: 0,
};

interface Harness {
  readonly socket: ReconnectingSocket;
  readonly messages: ServerMessage[];
  readonly statuses: string[];
  readonly invalid: string[];
}

function harness(
  options: { readonly onOpen?: ReconnectingSocketOptions['onOpen'] } = {},
): Harness {
  const messages: ServerMessage[] = [];
  const statuses: string[] = [];
  const invalid: string[] = [];

  const socket = new ReconnectingSocket({
    url: () => 'ws://host/ws',
    onMessage: (message) => messages.push(message),
    onStatus: (status) => statuses.push(status),
    onInvalidFrame: (reason) => invalid.push(reason),
    create: (url) => new FakeSocket(url),
    // Dead centre of the ±25% band, so the assertions are on the schedule
    // rather than on a random number.
    random: () => 0.5,
    delays: [100, 400],
    ...(options.onOpen ? { onOpen: options.onOpen } : {}),
  });

  return { socket, messages, statuses, invalid };
}

const latest = (): FakeSocket => {
  const socket = FakeSocket.opened.at(-1);
  if (socket === undefined) throw new Error('No socket was opened');
  return socket;
};

beforeEach(() => {
  FakeSocket.opened.length = 0;
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('socketUrl', () => {
  it('follows the page from http to ws and https to wss', () => {
    expect(
      socketUrl(undefined, { protocol: 'http:', host: 'a:3000' } as Location),
    ).toBe('ws://a:3000/ws');
    expect(
      socketUrl(undefined, { protocol: 'https:', host: 'a' } as Location),
    ).toBe('wss://a/ws');
  });

  it('encodes the session, because a key contains a colon', () => {
    expect(
      socketUrl('web:1', { protocol: 'http:', host: 'a' } as Location),
    ).toBe('ws://a/ws?session=web%3A1');
  });
});

describe('the reconnecting socket', () => {
  it('reports connecting, then open, and parses what arrives', () => {
    const { socket, messages, statuses } = harness();
    socket.open();

    expect(statuses).toEqual(['connecting']);
    latest().open();
    latest().deliver(CONNECTED);

    expect(statuses).toEqual(['connecting', 'open']);
    expect(messages).toEqual([CONNECTED]);
  });

  it('drops a frame the schema refuses and keeps the connection', () => {
    const { socket, messages, invalid } = harness();
    socket.open();
    latest().open();

    latest().deliver({ type: 'assistant.delta' });
    latest().deliver('{not json');
    latest().deliver(CONNECTED);

    // A server this client cannot read is a bug worth reporting, not a reason
    // to lose the frames that are fine.
    expect(invalid).toHaveLength(2);
    expect(messages).toEqual([CONNECTED]);
    expect(socket.status).toBe('open');
  });

  it('backs off on a schedule, and repeats the last delay', () => {
    const { socket, statuses } = harness();
    socket.open();
    latest().open();

    latest().drop();
    expect(statuses.at(-1)).toBe('reconnecting');
    expect(FakeSocket.opened).toHaveLength(1);

    vi.advanceTimersByTime(100);
    expect(FakeSocket.opened).toHaveLength(2);

    // The second attempt never opened, so the counter did not reset.
    latest().drop();
    vi.advanceTimersByTime(399);
    expect(FakeSocket.opened).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(FakeSocket.opened).toHaveLength(3);

    // Past the end of the table the last delay repeats rather than growing.
    latest().drop();
    vi.advanceTimersByTime(400);
    expect(FakeSocket.opened).toHaveLength(4);
  });

  it('resets the schedule on a frame, not on the handshake', () => {
    const { socket } = harness();
    socket.open();

    // A server that accepts a socket and immediately drops it. Resetting on
    // `open` would redial this at full speed forever.
    latest().open();
    latest().drop();
    vi.advanceTimersByTime(100);
    latest().open();
    latest().drop();
    vi.advanceTimersByTime(100);
    expect(FakeSocket.opened).toHaveLength(2);

    vi.advanceTimersByTime(300);
    expect(FakeSocket.opened).toHaveLength(3);

    // A frame proves the connection works, so the next drop starts over.
    latest().open();
    latest().deliver(CONNECTED);
    latest().drop();
    vi.advanceTimersByTime(100);
    expect(FakeSocket.opened).toHaveLength(4);
  });

  it('buffers what is sent while it is down, and flushes in order', () => {
    const { socket } = harness();
    socket.open();

    expect(socket.send({ type: 'ping' })).toBe(false);
    expect(socket.send({ type: 'turn.stop', sessionKey: 'web:1' })).toBe(false);
    expect(socket.buffered).toBe(2);

    latest().open();

    expect(
      latest().sent.map(
        (frame) => (JSON.parse(frame) as { type: string }).type,
      ),
    ).toEqual(['ping', 'turn.stop']);
    expect(socket.buffered).toBe(0);
  });

  it('drops the oldest once the buffer is full', () => {
    const { socket } = harness();
    socket.open();

    for (let index = 0; index < 40; index += 1) {
      socket.send({
        type: 'turn.steer',
        sessionKey: 'web:1',
        content: String(index),
      });
    }

    expect(socket.buffered).toBe(32);
    latest().open();

    const first = JSON.parse(latest().sent[0] ?? '{}') as { content: string };
    // The newest frame is the one the user just produced; the oldest goes.
    expect(first.content).toBe('8');
  });

  it('lets the handshake jump the queue', () => {
    const { socket } = harness({
      onOpen: (send) => {
        send({ type: 'session.resume', sessionKey: 'web:1', lastSeq: 4 });
      },
    });
    socket.open();
    socket.send({ type: 'ping' });
    latest().open();

    // A resume that arrived behind the messages it was meant to contextualise
    // would be a resume the server answers after replaying nothing.
    expect(
      latest().sent.map(
        (frame) => (JSON.parse(frame) as { type: string }).type,
      ),
    ).toEqual(['session.resume', 'ping']);
  });

  it('stays closed after close(), and drops what was buffered', () => {
    const { socket, statuses } = harness();
    socket.open();
    latest().open();
    socket.send({ type: 'ping' });

    socket.close();

    expect(latest().closed).toBe(true);
    expect(socket.buffered).toBe(0);
    expect(statuses.at(-1)).toBe('closed');

    // Our own close must not look like a dropped connection.
    vi.advanceTimersByTime(10_000);
    expect(FakeSocket.opened).toHaveLength(1);
  });

  it('redials immediately on request, without waiting for the schedule', () => {
    const { socket } = harness();
    socket.open();
    latest().open();
    latest().drop();

    socket.reconnectNow();
    expect(FakeSocket.opened).toHaveLength(2);
  });

  it('treats a URL the browser refuses as a failed attempt', () => {
    const statuses: string[] = [];
    const socket = new ReconnectingSocket({
      url: () => 'not a url',
      onMessage: () => undefined,
      onStatus: (status) => statuses.push(status),
      create: () => {
        throw new TypeError('bad url');
      },
      random: () => 0.5,
      delays: [50],
    });

    socket.open();

    // On the schedule rather than wedged in `connecting` forever.
    expect(statuses).toEqual(['connecting', 'reconnecting']);
    socket.close();
  });

  it('re-buffers a frame the socket rejected mid-write', () => {
    const { socket } = harness();
    socket.open();
    latest().open();

    const dying = latest();
    vi.spyOn(dying, 'send').mockImplementation(() => {
      throw new Error('socket closed');
    });

    expect(socket.send({ type: 'ping' })).toBe(true);
    // Held for the next connection rather than lost between two checks.
    expect(socket.buffered).toBe(1);
  });

  it('ignores a late event from a socket it has already replaced', () => {
    const { socket, messages } = harness();
    socket.open();
    const first = latest();
    first.open();

    socket.reconnectNow();
    first.deliver(CONNECTED);

    expect(messages).toEqual([]);
  });
});
