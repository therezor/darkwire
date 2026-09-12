//! The id every generated thing in GhostAI is named by.
//!
//! UUIDv7 rather than v4, and the difference is the first 48 bits: a v7 is a
//! millisecond timestamp followed by randomness, so ids sort in creation order
//! as plain strings. Both sides mint them — the server names a session on
//! `POST /api/sessions`, the browser names one before its first message is
//! sent — so the layout is one definition here rather than two that agree
//! until they do not.
//!
//! **What the ordering is worth.** Every listing orders on its own timestamp
//! column and reaches the id only to break a tie inside one millisecond. A v4
//! broke those ties at random; a v7 breaks them in creation order. That is the
//! whole benefit, and deliberately a small one: nothing was migrated, so the
//! tables hold both versions and the id is never the primary sort.
//!
//! **Monotonicity within one millisecond is not guaranteed**, and RFC 9562's
//! counter method is deliberately not implemented. Two ids minted in the same
//! millisecond differ in 74 random bits, which settles collision; which of the
//! two sorts first, no caller asks.
//!
//! The clock and the randomness are arguments rather than reached for, which
//! is what lets the fixture pin every byte of the layout.

use std::fmt::Write as _;

/// The random half of a v7 id: the ten bytes after the 48-bit timestamp.
pub type UuidRandom = [u8; 10];

/// A new UUIDv7 for the given instant and randomness, canonically formatted.
///
/// Byte 6's high nibble is the version and byte 8's two high bits are the
/// variant. Both are masked into the random byte rather than replacing it, so
/// the 12 and 62 bits either side of them stay random.
pub fn new_uuid(now_ms: u64, random: &UuidRandom) -> String {
    let mut bytes = [0u8; 16];
    // Bytes 0–5: the timestamp, big-endian. Only the low 48 bits fit, which
    // covers about ten thousand years of milliseconds.
    bytes[..6].copy_from_slice(&now_ms.to_be_bytes()[2..]);
    bytes[6..].copy_from_slice(random);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let mut out = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        let _ = write!(out, "{byte:02x}");
    }
    out
}
