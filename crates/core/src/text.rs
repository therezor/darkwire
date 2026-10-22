//! Cutting text to a byte budget.

/// The longest prefix of `text` within `max_bytes` that ends on a character
/// boundary. The bytes are a budget, and a split multi-byte character would
/// corrupt whatever follows it.
#[must_use]
pub fn truncate_bytes(text: &str, max_bytes: usize) -> &str {
    &text[..text.floor_char_boundary(max_bytes)]
}
