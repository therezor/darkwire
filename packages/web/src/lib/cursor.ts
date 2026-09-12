/**
 * Where this tab had got to, across a reload.
 *
 * The replay buffer is the only thing that can rebuild a turn that has not
 * finished — storage holds no half-written assistant message — and the protocol
 * asks for one number to do it. The number is *not* the last `seq` this tab
 * rendered: after a reload the transcript is gone, so resuming from there asks
 * the ring for the frames after the ones just forgotten, gets none, and loses
 * the turn. What is stored is the boundary between the two sources — the last
 * `seq` at which everything before it is in storage, which is to say the last
 * moment this tab was not mid-turn. `connection.ts` is where it is computed.
 *
 * That number lives in memory, and a reload is precisely the event that destroys memory. So
 * it is written where a reload does not reach: `sessionStorage`, which is
 * per-tab and survives F5, and is therefore exactly the granularity of the thing
 * being recovered from. `localStorage` would be wrong — two tabs on two
 * conversations would overwrite each other's cursor.
 *
 * It is written on every frame rather than on a flush schedule. A `setItem` of
 * fifty bytes is a few microseconds against an in-memory map, and the
 * alternative — a timer, plus `pagehide`, plus the knowledge that a crash
 * between flushes loses the turn — is more machinery protecting less.
 *
 * One session at a time, deliberately. A tab watches one conversation; a map
 * keyed by session would grow for the life of the tab to answer a question
 * nobody asks.
 */

const KEY = 'ghostai.cursor';

interface Cursor {
  readonly sessionKey: string;
  readonly lastSeq: number;
}

/**
 * The stored cursor for this session, or `undefined` when there is not one.
 *
 * `undefined` for a different session, for a corrupt entry, and for storage
 * that throws — which it does in a cross-origin iframe and under Safari's
 * private mode.
 *
 * Not `0`, and the difference is load-bearing. A stored `0` means "this tab has
 * rendered this conversation, and nothing in it is in storage yet" — which is
 * exactly a reload during the session's *first* turn, and the case that most
 * needs the ring. No entry at all means this tab has never been here, where
 * replaying the ring would duplicate the history REST is already fetching.
 * Collapsing the two onto one number makes the first turn the one turn a reload
 * cannot recover.
 */
export function readCursor(
  sessionKey: string,
  storage: Storage | undefined = safeStorage(),
): number | undefined {
  try {
    const raw = storage?.getItem(KEY);
    if (raw === null || raw === undefined) return undefined;

    const parsed: unknown = JSON.parse(raw);
    if (!isCursor(parsed) || parsed.sessionKey !== sessionKey) return undefined;
    return parsed.lastSeq;
  } catch {
    return undefined;
  }
}

export function writeCursor(
  sessionKey: string,
  lastSeq: number,
  storage: Storage | undefined = safeStorage(),
): void {
  try {
    storage?.setItem(
      KEY,
      JSON.stringify({ sessionKey, lastSeq } satisfies Cursor),
    );
  } catch {
    // A tab that cannot rebuild its in-flight turn after a reload beats a tab
    // that throws while rendering one.
  }
}

export function clearCursor(
  storage: Storage | undefined = safeStorage(),
): void {
  try {
    storage?.removeItem(KEY);
  } catch {
    // See above.
  }
}

function isCursor(value: unknown): value is Cursor {
  if (typeof value !== 'object' || value === null) return false;
  const candidate = value as Partial<Cursor>;
  return (
    typeof candidate.sessionKey === 'string' &&
    typeof candidate.lastSeq === 'number' &&
    Number.isInteger(candidate.lastSeq) &&
    candidate.lastSeq >= 0
  );
}

function safeStorage(): Storage | undefined {
  try {
    return globalThis.sessionStorage;
  } catch {
    return undefined;
  }
}
