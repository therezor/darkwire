//! Which wires ship, and how an extension adds one.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use ghostai_providers::testkit::ScriptedProvider;
use ghostai_providers::{
    BUILTIN_WIRES, ProviderSpec, WireAdapter, WireAdapters, WireProtocol, builtin_wire,
    wire_adapter_for,
};

fn scripted_adapter() -> WireAdapter {
    Arc::new(|options| Ok(ScriptedProvider::new(options.spec, Vec::new()) as _))
}

#[test]
fn only_openai_chat_ships() {
    assert_eq!(BUILTIN_WIRES, [WireProtocol::OpenaiChat]);
    assert!(builtin_wire(WireProtocol::OpenaiChat).is_some());
    assert!(builtin_wire(WireProtocol::AnthropicMessages).is_none());
    assert!(builtin_wire(WireProtocol::GeminiGenerate).is_none());
    assert!(builtin_wire(WireProtocol::OpenaiResponses).is_none());
}

#[test]
fn an_extension_fills_a_gap_but_cannot_replace_a_shipped_wire() {
    let mut extra: WireAdapters = WireAdapters::new();
    extra.insert(WireProtocol::AnthropicMessages, scripted_adapter());
    extra.insert(WireProtocol::OpenaiChat, scripted_adapter());

    assert!(wire_adapter_for(WireProtocol::AnthropicMessages, None).is_none());
    let filled = wire_adapter_for(WireProtocol::AnthropicMessages, Some(&extra)).unwrap();
    let spec = ProviderSpec::new("ext", "Ext", WireProtocol::AnthropicMessages, &[]);
    let provider = filled(ghostai_providers::WireAdapterOptions::new(spec)).unwrap();
    assert_eq!(provider.id(), "ext");

    // The built-in wins: a swap an operator cannot notice is not a capability.
    let kept = wire_adapter_for(WireProtocol::OpenaiChat, Some(&extra)).unwrap();
    let ollama = ghostai_providers::find_builtin("ollama").unwrap().clone();
    let provider = kept(ghostai_providers::WireAdapterOptions::new(ollama)).unwrap();
    assert_eq!(provider.spec().wire, WireProtocol::OpenaiChat);
}
