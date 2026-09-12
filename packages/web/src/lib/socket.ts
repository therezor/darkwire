/**
 * The WebSocket, with the three things a raw one does not have.
 *
 *  1. **Every inbound frame is `safeParse`d** against the same
 *     `ServerMessageSchema` the server builds its frames from. A frame that does
 *     not match is dropped and reported rather than handed to a reducer that
 *     would then write `undefined` into the transcript — and, since the socket
 *     is the one place untrusted bytes enter the app, it is the only sensible
 *     place for that check.
 *  2. **Reconnect is a schedule, not a loop.** Fixed backoff with jitter, and
 *     the attempt counter resets on the first frame of a connection rather than
 *     on `open` — a server that accepts a socket and immediately drops it would
 *     otherwise be reconnected to as fast as the browser can dial.
 *  3. **Outbound frames buffer while the socket is down.** A message typed
 *     during a two-second reconnect is a message the user expects to have sent.
 *     Replaying it is safe because `user.message` carries a `clientMessageId`
 *     and the hub is idempotent on it; a `turn.stop` that arrives late still
 *     stops the turn, which kept running while the socket was gone.
 *
 * Deliberately knows nothing about the transcript, the store or React. What it
 * emits is parsed frames and a status; `connection.ts` is what wires those into
 * the application. That split is what makes the reconnect behaviour testable
 * with a fake socket and no DOM.
 */

import {
  ServerMessageSchema,
  type ClientMessage,
  type ServerMessage,
} from '@ghostwire/protocol';

export type ConnectionStatus =
  'connecting' | 'open' | 'reconnecting' | 'closed';

/**
 * The delays between reconnect attempts, in milliseconds; the last one repeats.
 *
 * The first is short because the overwhelmingly common cause is a server that
 * restarted under `ghostai serve --watch`, and a user staring at "Reconnecting"
 * for five seconds after a one-second outage assumes the app broke. The tail is
 * long because the other common cause is a laptop that closed its lid.
 */
const RECONNECT_DELAYS_MS: readonly number[] = [
  400, 1_000, 2_500, 5_000, 10_000, 20_000,
];

/**
 * Outbound frames held while the socket is down.
 *
 * A bound rather than a courtesy: without one, a tab left open on a dead server
 * with a key stuck under a finger grows an unbounded array. Past the cap the
 * oldest goes, because the newest frame is the one the user just produced.
 */
const MAX_BUFFERED_FRAMES = 32;

/** The parts of `WebSocket` this uses, so a test can supply a fake. */
export interface SocketLike {
  send(data: string): void;
  close(): void;
  onopen: (() => void) | null;
  onclose: (() => void) | null;
  onerror: (() => void) | null;
  onmessage: ((event: { data: unknown }) => void) | null;
}

export interface ReconnectingSocketOptions {
  /**
   * The URL to dial, asked for on every attempt.
   *
   * A function rather than a string because the session the socket should open
   * on can change between attempts — switching conversations while offline is
   * a reconnect to a different query.
   */
  readonly url: () => string;
  readonly onMessage: (message: ServerMessage) => void;
  readonly onStatus: (status: ConnectionStatus) => void;
  /**
   * Called after the socket opens and before buffered frames flush, so a
   * handshake — `session.resume { lastSeq }` — leads the queue rather than
   * arriving behind the messages it was supposed to contextualise.
   */
  readonly onOpen?: (send: (message: ClientMessage) => void) => void;
  /** A frame the schema refused. Reported rather than thrown: the socket lives. */
  readonly onInvalidFrame?: (reason: string, raw: unknown) => void;
  /** Injected by tests. Defaults to the global `WebSocket`. */
  readonly create?: (url: string) => SocketLike;
  /** Injected by tests. `[0, 1)`, multiplied into the delay as ±25%. */
  readonly random?: () => number;
  readonly delays?: readonly number[];
}

export class ReconnectingSocket {
  private readonly options: ReconnectingSocketOptions;
  private readonly delays: readonly number[];
  private readonly buffer: ClientMessage[] = [];

  private socket: SocketLike | undefined;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private attempt = 0;
  private currentStatus: ConnectionStatus = 'closed';
  /** True from `close()` until the next `open()`, and no reconnect happens in it. */
  private stopped = true;

  constructor(options: ReconnectingSocketOptions) {
    this.options = options;
    this.delays = options.delays ?? RECONNECT_DELAYS_MS;
  }

  get status(): ConnectionStatus {
    return this.currentStatus;
  }

  /** Frames waiting for a socket. Read by the tests and by nothing else. */
  get buffered(): number {
    return this.buffer.length;
  }

  /** Opens, or does nothing if a socket is already open or dialling. */
  open(): void {
    if (!this.stopped) return;
    this.stopped = false;
    this.attempt = 0;
    this.dial('connecting');
  }

  /**
   * Closes and stays closed. Buffered frames are dropped — they were queued
   * against a connection the caller has just said it does not want.
   */
  close(): void {
    this.stopped = true;
    this.clearTimer();
    this.buffer.length = 0;
    this.teardown();
    this.setStatus('closed');
  }

  /**
   * Reconnects now, whatever the schedule said.
   *
   * The one caller is a session switch that has to re-dial rather than send
   * `session.switch` — a socket that is `reconnecting` has no connection to
   * send the switch on, and waiting twenty seconds to honour a click is worse
   * than dialling again.
   */
  reconnectNow(): void {
    if (this.stopped) return;
    this.clearTimer();
    this.teardown();
    this.attempt = 0;
    this.dial('reconnecting');
  }

  /**
   * Sends, or buffers until there is something to send on.
   *
   * Returns whether it went out now, which the composer uses for nothing and a
   * test uses for everything.
   */
  send(message: ClientMessage): boolean {
    if (this.socket !== undefined && this.currentStatus === 'open') {
      this.write(this.socket, message);
      return true;
    }

    this.buffer.push(message);
    if (this.buffer.length > MAX_BUFFERED_FRAMES) this.buffer.shift();
    return false;
  }

  private dial(status: ConnectionStatus): void {
    this.setStatus(status);

    const create = this.options.create ?? defaultCreate;
    let socket: SocketLike;
    try {
      socket = create(this.options.url());
    } catch {
      // A URL the browser refuses to dial — a bad protocol, a blocked origin.
      // It is a failed attempt like any other, so it goes on the schedule
      // rather than leaving the socket wedged in `connecting` forever.
      this.scheduleReconnect();
      return;
    }

    this.socket = socket;

    socket.onopen = (): void => {
      if (this.socket !== socket) return;
      this.setStatus('open');
      this.options.onOpen?.((message) => {
        this.write(socket, message);
      });
      this.flush(socket);
    };

    socket.onmessage = (event): void => {
      if (this.socket !== socket) return;
      // The connection has proved itself: the next drop starts at the top of
      // the schedule rather than wherever this attempt left the counter.
      this.attempt = 0;
      this.receive(event.data);
    };

    // `error` is always followed by `close` in every browser, so reconnecting
    // from both would dial twice for one failure.
    socket.onerror = null;

    socket.onclose = (): void => {
      if (this.socket !== socket) return;
      this.socket = undefined;
      if (this.stopped) return;
      this.scheduleReconnect();
    };
  }

  private receive(data: unknown): void {
    let value: unknown = data;
    if (typeof data === 'string') {
      try {
        value = JSON.parse(data);
      } catch {
        this.options.onInvalidFrame?.('Frame is not valid JSON', data);
        return;
      }
    }

    const parsed = ServerMessageSchema.safeParse(value);
    if (!parsed.success) {
      // A server this client cannot read is a bug worth surfacing, not a frame
      // worth guessing at. The connection survives: the next frame may be fine,
      // and dropping the socket would lose the ones that are.
      this.options.onInvalidFrame?.(describe(parsed.error.issues), value);
      return;
    }

    this.options.onMessage(parsed.data);
  }

  private flush(socket: SocketLike): void {
    for (const message of this.buffer.splice(0)) this.write(socket, message);
  }

  private write(socket: SocketLike, message: ClientMessage): void {
    try {
      socket.send(JSON.stringify(message));
    } catch {
      // The socket died between the readyState check and the write. Buffer it
      // for the next connection rather than losing what the user typed.
      this.buffer.push(message);
      if (this.buffer.length > MAX_BUFFERED_FRAMES) this.buffer.shift();
    }
  }

  private scheduleReconnect(): void {
    this.setStatus('reconnecting');

    const index = Math.min(this.attempt, this.delays.length - 1);
    const base = this.delays[index] ?? 0;
    this.attempt += 1;

    // ±25%. Without it, every tab open on a server that restarted redials in
    // the same millisecond, which is a thundering herd the moment there is more
    // than one of them.
    const random = this.options.random ?? webCryptoUnitInterval;
    const delay = Math.round(base * (0.75 + random() * 0.5));

    this.timer = setTimeout(() => {
      this.timer = undefined;
      if (this.stopped) return;
      this.dial('reconnecting');
    }, delay);
  }

  private clearTimer(): void {
    if (this.timer === undefined) return;
    clearTimeout(this.timer);
    this.timer = undefined;
  }

  /** Drops the handlers before closing, so our own `close` is not a reconnect. */
  private teardown(): void {
    const socket = this.socket;
    if (socket === undefined) return;
    this.socket = undefined;
    socket.onopen = null;
    socket.onclose = null;
    socket.onerror = null;
    socket.onmessage = null;
    try {
      socket.close();
    } catch {
      // Closing an already-closing socket. Nothing to do and nothing to report.
    }
  }

  private setStatus(status: ConnectionStatus): void {
    if (this.currentStatus === status) return;
    this.currentStatus = status;
    this.options.onStatus(status);
  }
}

/**
 * The socket URL for a session, on the origin serving the page.
 *
 * Derived from `location` rather than configured: the UI is served by the same
 * process that owns `/ws`, and a configurable socket origin would be a setting
 * whose only correct value is the one this computes.
 */
export function socketUrl(
  sessionKey: string | undefined,
  location: Location,
): string {
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const base = `${protocol}//${location.host}/ws`;
  return sessionKey === undefined
    ? base
    : `${base}?session=${encodeURIComponent(sessionKey)}`;
}

function defaultCreate(url: string): SocketLike {
  return new WebSocket(url) as unknown as SocketLike;
}

/**
 * `[0, 1)` from the platform's CSPRNG.
 *
 * Not because reconnect jitter needs to be unpredictable, but because
 * `Math.random()` is banned repo-wide: it makes a test's outcome depend on a
 * global nobody can seed. Every caller that needs determinism injects `random`;
 * this is what the app uses when nobody does.
 */
function webCryptoUnitInterval(): number {
  const buffer = new Uint32Array(1);
  crypto.getRandomValues(buffer);
  return (buffer[0] ?? 0) / 2 ** 32;
}

function describe(
  issues: ReadonlyArray<{ path: PropertyKey[]; message: string }>,
): string {
  const first = issues[0];
  if (first === undefined) return 'Frame did not match any server message';
  const path = first.path.map(String).join('.');
  return path === '' ? first.message : `${path}: ${first.message}`;
}
