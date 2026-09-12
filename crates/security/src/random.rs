//! Injectable randomness.
//!
//! Two things in this crate need unpredictable bytes — the per-turn tool-output
//! nonce and the vault's AES-GCM initialisation vectors — and both are worthless
//! if the values are guessable. So the source is always the operating system's
//! generator, never a seeded one, and it is a parameter rather than a global so
//! tests can pin it. The thread-local generator is clippy-denied repo-wide for
//! the same reason `Math.random()` was lint-banned.
//!
//! A test that injects a counter is asserting on *wrapping and escaping*, which
//! is the part that has to be right. Nothing may inject a fixed source in
//! production: a tool-output nonce from a constant is the same as having no
//! delimiter at all, because tool output could then close its own envelope.

use rand::TryRng;
use rand::rngs::SysRng;

/// A source of unpredictable bytes.
pub trait RandomSource: Send + Sync {
    /// Fills `buf` entirely with random bytes.
    fn fill(&self, buf: &mut [u8]);
}

/// Lowercase hex, the spelling nonces and digests use.
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The operating system's generator.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsRandom;

impl RandomSource for OsRandom {
    fn fill(&self, buf: &mut [u8]) {
        // A generator that cannot answer has no safe substitute: a zeroed IV
        // would silently destroy the confidentiality of every credential
        // encrypted under it, and a predictable nonce would make every envelope
        // forgeable. Aborting is the only outcome that cannot be exploited.
        assert!(
            SysRng.try_fill_bytes(buf).is_ok(),
            "the operating system's random generator failed"
        );
    }
}
