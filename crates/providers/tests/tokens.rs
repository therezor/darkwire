//! The character heuristic.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_providers::estimate_tokens;

#[test]
fn is_proportional_to_length_and_never_fractional() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("abcd"), 1);
    assert_eq!(estimate_tokens("abcde"), 2);
}

#[test]
fn counts_utf16_units_like_the_browser_does() {
    // Four astral characters are eight units, so two tokens, not one.
    assert_eq!(estimate_tokens("😀😀😀😀"), 2);
}
