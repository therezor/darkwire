//! Which wire protocols this process can actually speak.
//!
//! [`WIRE_PROTOCOLS`](crate::registry::WIRE_PROTOCOLS) is the closed
//! vocabulary a `ProviderSpec` may name; this is the subset an adapter exists
//! for. The two were one thing until an extension needed to supply the
//! missing half, and separating them is what turns "not implemented yet" from
//! a hard-coded `if` into a lookup that something can fill.
//!
//! `openai-chat` is the only entry that ships. The factory refuses the other
//! three loudly, and says how to fix it.

use std::collections::HashMap;
use std::sync::Arc;

use crate::openai_chat::create_openai_chat_provider;
use crate::registry::WireProtocol;
use crate::types::WireAdapter;

/// Wire adapters keyed by the protocol they speak.
pub type WireAdapters = HashMap<WireProtocol, WireAdapter>;

/// The wires compiled into this build.
pub const BUILTIN_WIRES: [WireProtocol; 1] = [WireProtocol::OpenaiChat];

/// The adapter compiled in for `wire`, if any.
pub fn builtin_wire(wire: WireProtocol) -> Option<WireAdapter> {
    match wire {
        WireProtocol::OpenaiChat => Some(Arc::new(create_openai_chat_provider)),
        WireProtocol::AnthropicMessages
        | WireProtocol::GeminiGenerate
        | WireProtocol::OpenaiResponses => None,
    }
}

/// The adapter for one wire, with an extension's table layered over the
/// built-in one.
///
/// Layered rather than merged so that an extension supplying
/// `anthropic-messages` adds a wire without being able to *replace*
/// `openai-chat`: the one every local provider in the registry speaks, and
/// the one an operator would have no way to notice had been swapped.
pub fn wire_adapter_for(wire: WireProtocol, extra: Option<&WireAdapters>) -> Option<WireAdapter> {
    builtin_wire(wire).or_else(|| extra.and_then(|extra| extra.get(&wire).cloned()))
}
