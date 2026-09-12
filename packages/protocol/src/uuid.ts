/**
 * The id every generated thing in GhostAI is named by.
 *
 * UUIDv7 rather than v4, and the difference is the first 48 bits: a v7 is a
 * millisecond timestamp followed by randomness, so ids sort in creation order as
 * plain strings. `crypto.randomUUID` only mints v4, which is why this is written
 * out rather than delegated.
 *
 * It lives in `@ghostwire/protocol` for the reason `ids.ts` gives at its head:
 * both sides mint ids. The server names a session on `POST /api/sessions`, and
 * the browser names one in `lib/connection.ts` before the first message has been
 * sent. Two implementations of a rule whose whole job is that two things cannot
 * collide is not a rule.
 *
 * **What the ordering is actually worth here.** Every listing in this repo
 * orders on its own timestamp column — `updated_at_ms DESC, key ASC`,
 * `created_at_ms DESC, id ASC`, `started_at_ms DESC, id ASC` — and reaches the
 * id only to break a tie inside one millisecond. A v4 broke those ties at
 * random; a v7 breaks them in creation order. That is the whole benefit, and it
 * is deliberately a small one: **nothing was migrated**, so these tables hold
 * both versions and the id is never the primary sort. Anything that needed rows
 * in creation order was already reading a timestamp and still should be.
 *
 * **Monotonicity within one millisecond is not guaranteed**, and RFC 9562's
 * counter method is deliberately not implemented. Two ids minted in the same
 * millisecond differ in 74 random bits, which settles collision; what it does
 * not settle is which of the two sorts first, and no caller here asks. A counter
 * would need process-wide mutable state in a module that has none, to order a
 * pair of rows that every query already orders by other means.
 *
 * The one generator that stays on v4 is `providers/openai-chat.ts`, which
 * truncates to 16 hex characters. A v7 truncated that far is almost entirely
 * timestamp, so the entropy the truncation depends on would be gone.
 */

/** Milliseconds, as a 32-bit high half and a 16-bit low half. */
const LOW_HALF = 0x1_0000;

/**
 * A new UUIDv7, canonically formatted.
 *
 * Reads `globalThis.crypto`, which is present in Node 20+ and in every browser,
 * so this file imports nothing and runs unchanged on both sides. `DataView`
 * rather than index access because `noUncheckedIndexedAccess` types every
 * `bytes[i]` as possibly undefined, and a byte-layout function reads better
 * saying which offset it is writing than asserting each read is real.
 */
export function newUuid(): string {
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  const view = new DataView(bytes.buffer);

  // Bytes 0–5: the timestamp, big-endian, over the randomness already there.
  const ms = Date.now();
  view.setUint32(0, Math.floor(ms / LOW_HALF));
  view.setUint16(4, ms % LOW_HALF);

  // Byte 6's high nibble is the version, and byte 8's two high bits are the
  // variant. Both are masked into the random byte rather than replacing it, so
  // the 12 and 62 bits either side of them stay random.
  view.setUint8(6, (view.getUint8(6) & 0x0f) | 0x70);
  view.setUint8(8, (view.getUint8(8) & 0x3f) | 0x80);

  const hex = Array.from(bytes, (byte) =>
    byte.toString(16).padStart(2, '0'),
  ).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
