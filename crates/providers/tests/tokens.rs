//! The character heuristic.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_providers::estimate_tokens;

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

#[test]
fn prices_han_kana_and_hangul_near_a_token_a_character() {
    // 1.4 characters a token: four Han characters are three tokens, not one.
    assert_eq!(estimate_tokens("你好世界"), 3);
    assert_eq!(estimate_tokens("こんにちは"), 4);
    assert_eq!(estimate_tokens("안녕하세요"), 4);
    // Latin beside them is still four to a token.
    assert_eq!(estimate_tokens("abcd你好"), 3);
}

#[test]
fn leaves_every_other_script_at_four_units_a_token() {
    assert_eq!(estimate_tokens("привіт світ!"), 3);
    assert_eq!(estimate_tokens(&"x".repeat(400)), 100);
}
