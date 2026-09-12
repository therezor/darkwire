//! The shared plumbing: literals, coercion, nullability and the trim rule.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_protocol::json::{MAX_SAFE_INTEGER, Nullable, True, coerce_f64, coerce_u64, js_trim};
use ghostai_protocol::{ContentPart, TextTag, protocol_generator};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct Coerced {
    #[serde(deserialize_with = "coerce_u64")]
    count: u64,
    #[serde(deserialize_with = "coerce_f64")]
    ratio: f64,
}

fn coerced(value: serde_json::Value) -> Result<Coerced, serde_json::Error> {
    serde_json::from_value(value)
}

#[test]
fn numbers_arrive_as_numbers_strings_booleans_or_null() {
    let c = coerced(json!({"count": 3, "ratio": 1.5})).unwrap();
    assert_eq!((c.count, c.ratio), (3, 1.5));
    let c = coerced(json!({"count": " 42 ", "ratio": "0.25"})).unwrap();
    assert_eq!((c.count, c.ratio), (42, 0.25));
    let c = coerced(json!({"count": true, "ratio": null})).unwrap();
    assert_eq!((c.count, c.ratio), (1, 0.0));
    let c = coerced(json!({"count": "", "ratio": false})).unwrap();
    assert_eq!((c.count, c.ratio), (0, 0.0));
}

#[test]
fn coercion_refuses_what_is_not_a_number() {
    assert!(coerced(json!({"count": "many", "ratio": 1})).is_err());
    assert!(coerced(json!({"count": 1.5, "ratio": 1})).is_err());
    assert!(coerced(json!({"count": -1, "ratio": 1})).is_err());
    assert!(coerced(json!({"count": [], "ratio": 1})).is_err());
    assert!(coerced(json!({"count": "Infinity", "ratio": 1})).is_err());
    let too_big = format!("{}", MAX_SAFE_INTEGER + 2);
    assert!(coerced(json!({"count": too_big, "ratio": 1})).is_err());
}

#[test]
fn a_literal_accepts_only_its_value() {
    assert!(serde_json::from_value::<TextTag>(json!("text")).is_ok());
    let error = serde_json::from_value::<TextTag>(json!("image")).unwrap_err();
    assert!(error.to_string().contains("text"), "{error}");
    assert_eq!(serde_json::to_value(TextTag).unwrap(), json!("text"));
    assert_eq!(TextTag::VALUE, "text");
}

#[test]
fn true_accepts_only_true() {
    assert!(serde_json::from_value::<True>(json!(true)).is_ok());
    assert!(serde_json::from_value::<True>(json!(false)).is_err());
    assert_eq!(serde_json::to_value(True).unwrap(), json!(true));
    let schema = protocol_generator().root_schema_for::<True>();
    assert_eq!(schema.get("const"), Some(&json!(true)));
}

#[test]
fn nullable_is_spelled_as_a_one_of() {
    let schema = protocol_generator().root_schema_for::<Nullable<String>>();
    let members = schema
        .get("oneOf")
        .and_then(serde_json::Value::as_array)
        .unwrap();
    assert_eq!(members.len(), 2);
    assert!(members.iter().any(|m| m == &json!({"type": "null"})));
}

#[test]
fn a_union_names_the_discriminator_it_did_not_find() {
    let missing = serde_json::from_value::<ContentPart>(json!({"text": "x"})).unwrap_err();
    assert!(missing.to_string().contains("`type`"), "{missing}");
    let unknown = serde_json::from_value::<ContentPart>(json!({"type": "audio"})).unwrap_err();
    assert!(unknown.to_string().contains("audio"), "{unknown}");
    let wrong = serde_json::from_value::<ContentPart>(json!({"type": "text"})).unwrap_err();
    assert!(wrong.to_string().contains("text"), "{wrong}");
    assert_eq!(ContentPart::TAG, "type");
    assert_eq!(ContentPart::VALUES, &["text", "image", "file"]);
}

#[test]
fn trim_follows_the_browser() {
    assert_eq!(js_trim("\u{FEFF} x \u{00A0}\r\n"), "x");
    assert_eq!(js_trim("\u{200B}x"), "\u{200B}x");
}
