//! How big a request would be, priced on the body rather than on the record.
//!
//! The context inspector answers "will this fit", and until these functions
//! existed it answered it by serialising the objects this project stores.
//! Those are not what a provider receives. An assistant record keeps the
//! model's `reasoning` beside its answer and the wire has never carried it; a
//! `ToolDefinition` carries `risk` and `source`, which drive an approval prompt
//! and a badge and have never left this process. Both were being billed to a
//! bar that says how much room is left.
//!
//! So the measurement runs through `wire_encode`, the same functions the
//! transport encodes with, not a description of them. When a field stops
//! being sent, it stops being counted in the same commit, because there is
//! one place that decides.
//!
//! **What is still an estimate, and will stay one.** The figure is
//! [`estimate_tokens`] over the JSON body, so structural characters (keys,
//! quotes, escaping) are counted as the model's tokenizer would not count
//! them, and [`estimate_tokens`] is characters over four. The claim here is
//! narrower than accuracy: this is the *right input* to that heuristic, where
//! the stored object was the wrong one.

use ghostai_protocol::{ChatMessage, ToolDefinition};

use crate::tokens::estimate_tokens;
use crate::wire_encode::{encode_message, encode_tools};

/// One message, as the request body would carry it.
pub fn estimate_message_tokens(message: &ChatMessage) -> usize {
    let encoded = encode_message(message);
    estimate_tokens(&serde_json::to_string(&encoded).unwrap_or_default())
}

/// The whole tool array, not a sum over single tools.
///
/// A body carries them as one JSON array, so the brackets and separators
/// between them are part of what is sent, and pricing each definition alone
/// would drop them.
pub fn estimate_tool_tokens(tools: &[ToolDefinition]) -> usize {
    let encoded = encode_tools(tools);
    estimate_tokens(&serde_json::to_string(&encoded).unwrap_or_default())
}
