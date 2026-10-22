//! Token estimation.
//!
//! One question: **"roughly how big is this?"** The context meter and the
//! history trimmer ask it about every request, and the degradation ladder asks
//! it after a context-length rejection. Neither needs it exact. After a request
//! the meter multiplies it by the provider's own count over this estimate of
//! the same request, so what matters is that the error stays steady from one
//! request to the next, not that it is small. So this is a character heuristic
//! with no tables and no allocation.
//!
//! There is deliberately no tokenizer here. A per-provider tokenizer would mean
//! shipping megabytes of vocabulary per provider to refine a figure the
//! provider corrects for free on every response.
//!
//! The one split it does make is by script. Han, kana and Hangul run at about
//! 1.2 to 1.7 characters per token on current tokenizers (o200k, Llama 3,
//! Qwen 3), so pricing them at four would undercount a Chinese or Japanese
//! conversation threefold. Everything else stays at four: prose runs nearer
//! five, code nearer four, and Cyrillic within a few percent of four.

/// Characters per token for every script but the wide ones below.
const CHARS_PER_TOKEN: usize = 4;

/// Han, kana and Hangul at 1.4 characters per token, kept as the fraction 7/5
/// so the sum stays in integers. With `CHARS_PER_TOKEN` the common
/// denominator is 28: a narrow unit is 7/28 of a token and a wide character
/// 20/28.
const NARROW_WEIGHT: usize = 7;
const WIDE_WEIGHT: usize = 20;
const DENOMINATOR: usize = 28;
const _: () = assert!(NARROW_WEIGHT * CHARS_PER_TOKEN == DENOMINATOR);

/// A table-free estimate: wide characters at 1.4 per token, everything else at
/// four UTF-16 units per token, rounded up.
///
/// Pure ASCII text comes out as `ceil(length / 4)`, the figure this function
/// has always returned for it.
pub fn estimate_tokens(text: &str) -> usize {
    let mut narrow = 0usize;
    let mut wide = 0usize;
    for character in text.chars() {
        if is_wide(character) {
            wide += 1;
        } else {
            narrow += character.len_utf16();
        }
    }
    (narrow * NARROW_WEIGHT + wide * WIDE_WEIGHT).div_ceil(DENOMINATOR)
}

/// The scripts a tokenizer spends close to a token per character on.
fn is_wide(character: char) -> bool {
    matches!(
        u32::from(character),
        0x1100..=0x11FF // Hangul Jamo
            | 0x3000..=0x30FF // CJK punctuation, hiragana, katakana
            | 0x3130..=0x318F // Hangul compatibility Jamo
            | 0x31F0..=0x31FF // katakana phonetic extensions
            | 0x3400..=0x4DBF // CJK extension A
            | 0x4E00..=0x9FFF // CJK unified ideographs
            | 0xAC00..=0xD7AF // Hangul syllables
            | 0xF900..=0xFAFF // CJK compatibility ideographs
            | 0xFF00..=0xFFEF // half- and full-width forms
            | 0x20000..=0x3134F // CJK extensions B to G
    )
}
