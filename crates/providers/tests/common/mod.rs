//! Shared by every test file: fixtures, message constructors and specs.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "each test file uses a different subset of these helpers, and a fixture that cannot load is a failing test either way"
)]

use std::path::PathBuf;

use darkwire_core::messages::{
    AssistantOptions, ToolOptions, assistant_message, system_message, tool_message, user_message,
};
use darkwire_protocol::{ChatMessage, ContentPart, ToolCall, ToolDefinition, ToolRisk, ToolSource};
use darkwire_providers::{ProviderSpec, find_builtin};
use serde_json::Value;

/// The repository's `fixtures/` directory.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// A fixture file, parsed.
pub fn read_fixture(relative: &str) -> Value {
    let path = fixtures_dir().join(relative);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{relative} is not JSON: {error}"))
}

/// The `cases` array of a fixture.
pub fn cases(fixture: &Value) -> Vec<Value> {
    fixture["cases"]
        .as_array()
        .expect("fixture has cases")
        .clone()
}

/// A built-in spec, cloned.
pub fn spec_of(id: &str) -> ProviderSpec {
    find_builtin(id)
        .unwrap_or_else(|| panic!("no such provider: {id}"))
        .clone()
}

pub fn system(text: &str) -> ChatMessage {
    ChatMessage::System(system_message(text))
}

pub fn user(text: &str) -> ChatMessage {
    ChatMessage::User(user_message(text))
}

pub fn user_parts(parts: Vec<ContentPart>) -> ChatMessage {
    ChatMessage::User(user_message(parts))
}

pub fn assistant(text: &str) -> ChatMessage {
    ChatMessage::Assistant(assistant_message(text, AssistantOptions::default()))
}

pub fn assistant_with(
    text: &str,
    tool_calls: Vec<ToolCall>,
    reasoning: Option<&str>,
) -> ChatMessage {
    ChatMessage::Assistant(assistant_message(
        text,
        AssistantOptions {
            tool_calls,
            reasoning: reasoning.map(str::to_owned),
            reasoning_ms: None,
        },
    ))
}

pub fn tool(call_id: &str, name: &str, content: &str) -> ChatMessage {
    ChatMessage::Tool(tool_message(call_id, name, content, ToolOptions::default()))
}

pub fn call(id: &str, name: &str, arguments_json: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments_json: arguments_json.to_owned(),
    }
}

pub fn tool_definition(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters: serde_json::from_value(parameters).expect("object schema"),
        risk: ToolRisk::Safe,
        source: ToolSource::Builtin,
        annotations: None,
    }
}

/// A loopback port nothing listens on.
pub fn refused_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr").port()
}
