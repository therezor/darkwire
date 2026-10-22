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
//! them. The claim here is narrower than accuracy: this is the *right input*
//! to that heuristic, where the stored object was the wrong one. The meter
//! scales the result by the provider's own count of an earlier request, which
//! absorbs a steady error like this one.
//!
//! The exception is an image. Its base64 is what the body carries and nothing
//! like what the model reads: a 1 MB photo is a quarter of a million
//! "tokens" by characters and somewhere between a few hundred and a few
//! thousand to a vision model. It is priced at [`IMAGE_TOKENS`] instead.

use darkwire_protocol::{ChatMessage, ToolDefinition};

use crate::tokens::estimate_tokens;
use crate::wire_encode::{WireContent, WireContentPart, encode_message, encode_tools};

/// What one image costs a vision model, whatever its size.
///
/// Near what other harnesses use (Cline 1,600, Codex about 1,844, Claude Code
/// 2,000). Real figures run from 256 (Gemma 3) to several thousand for a large
/// image on a patch-based model. The provider's count corrects it after the
/// first request that carries the image.
pub const IMAGE_TOKENS: usize = 1_800;

/// One message, as the request body would carry it.
pub fn estimate_message_tokens(message: &ChatMessage) -> usize {
    let mut encoded = encode_message(message);
    let mut images = 0;
    if let Some(WireContent::Parts(parts)) = &mut encoded.content {
        for part in parts {
            if let WireContentPart::ImageUrl { image_url } = part {
                images += 1;
                image_url.url.clear();
            }
        }
    }
    estimate_tokens(&serde_json::to_string(&encoded).unwrap_or_default()) + images * IMAGE_TOKENS
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
