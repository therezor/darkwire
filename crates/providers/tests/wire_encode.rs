//! The `chat/completions` body, against `fixtures/wire/openai-chat.json`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use darkwire_core::messages::{FileDetails, ImageSource, file_part, image_part, text_part};
use darkwire_providers::wire_encode::{
    WireContent, WireContentPart, encode_content, encode_message, encode_part, encode_tools,
};
use darkwire_providers::{ChatRequest, build_body};
use serde_json::{Value, json};

#[test]
fn the_fixture_pins_every_body() {
    // `fixtures/wire/openai-chat.json`: the `chat/completions` body an
    // OpenAI-compatible request becomes, by the spec named in `input.spec`.
    let fixture = common::read_fixture("wire/openai-chat.json");
    let cases = common::cases(&fixture);
    assert_eq!(cases.len(), 21);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let spec = common::spec_of(case["input"]["spec"].as_str().unwrap());
        let stream = case["input"]["stream"].as_bool().unwrap_or(false);
        let request: ChatRequest = serde_json::from_value(case["input"]["request"].clone())
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let body = Value::Object(build_body(&spec, &request, stream));
        assert_eq!(body, case["output"], "{name}");
    }
}

#[test]
fn a_request_round_trips_through_json() {
    let request = ChatRequest::new("m", vec![common::user("hi")]);
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["model"], json!("m"));
    assert!(json.get("toolChoice").is_none());
    let back: ChatRequest = serde_json::from_value(json).unwrap();
    assert_eq!(back, request);
}

#[test]
fn parts_encode_to_the_wire_shapes() {
    assert_eq!(
        encode_part(&text_part("hi")),
        WireContentPart::Text { text: "hi".into() }
    );
    let file = encode_part(&file_part(
        "uploads/x.csv",
        "text/csv",
        FileDetails {
            name: Some("x.csv".into()),
            size_bytes: Some(3),
        },
    ));
    assert_eq!(
        file,
        WireContentPart::Text {
            text: "[attachment: uploads/x.csv · text/csv]".into()
        }
    );
    let by_url = encode_part(&image_part(
        "image/jpeg",
        ImageSource::Url("https://img.test/a.jpg".into()),
    ));
    assert_eq!(
        serde_json::to_value(by_url).unwrap(),
        json!({"type": "image_url", "image_url": {"url": "https://img.test/a.jpg"}})
    );
    // Text-only collapses; any image keeps the array form.
    assert_eq!(
        encode_content(&[text_part("a"), text_part("b")]),
        WireContent::Text("a\nb".into())
    );
    assert!(matches!(
        encode_content(&[text_part("a"), image_part("image/png", ImageSource::Data("AAA".into()))]),
        WireContent::Parts(parts) if parts.len() == 2
    ));
}

#[test]
fn messages_and_tools_drop_what_the_wire_does_not_carry() {
    let assistant = encode_message(&common::assistant_with(
        "",
        vec![common::call("c1", "read", "{not json")],
        Some("never sent"),
    ));
    let json = serde_json::to_value(&assistant).unwrap();
    assert_eq!(json["content"], Value::Null);
    assert_eq!(
        json["tool_calls"][0]["function"]["arguments"],
        json!("{not json")
    );
    assert!(json.get("reasoning").is_none());

    let tool = encode_message(&common::tool("c1", "read", "hi"));
    assert_eq!(tool.tool_call_id.as_deref(), Some("c1"));
    assert_eq!(tool.role, "tool");

    let system = encode_message(&common::system("rules"));
    assert_eq!(system.content, Some(WireContent::Text("rules".into())));

    let tools = encode_tools(&[common::tool_definition("t", "d", json!({"type": "object"}))]);
    assert_eq!(
        serde_json::to_value(&tools).unwrap(),
        json!([{"type": "function", "function": {"name": "t", "description": "d", "parameters": {"type": "object"}}}])
    );
}
