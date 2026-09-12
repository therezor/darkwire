//! Token estimation.
//!
//! One question: **"roughly how big is this?"** The degradation ladder asks it
//! when deciding how many turns to drop after a context-length rejection. It
//! runs on the failure path, needs no precision, and must not make the failure
//! path slower than the success path, so this is a character heuristic with no
//! tables and no allocation.
//!
//! There is deliberately no exact tokenizer here. A per-provider tokenizer
//! would mean shipping megabytes of vocabulary per provider to improve an
//! estimate that exists to decide *whether* to truncate, not exactly where;
//! nothing in the tree sizes against a hard limit, so nothing needs one.

/// Average characters per token across English prose, code and JSON.
///
/// Prose runs closer to 4.5 and minified JSON closer to 3, so 4 sits between
/// them and errs slightly high on prose, the safe direction when the number is
/// used to decide how much history to drop.
const CHARS_PER_TOKEN: usize = 4;

/// A table-free estimate: `ceil(length / 4)`, where length is in UTF-16 code
/// units so the figure matches what the browser's context inspector shows for
/// the same text.
pub fn estimate_tokens(text: &str) -> usize {
    text.encode_utf16().count().div_ceil(CHARS_PER_TOKEN)
}
