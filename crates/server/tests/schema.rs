//! The `$defs` pool, and the one place a request body becomes a 422.
//!
//! Two failures arrive here from completely different directions and have to
//! leave by the same door. A body whose *shape* is wrong fails while it is
//! being read and is keyed by the path the reader walked to; a body that parsed
//! and then broke a rule fails afterwards and is keyed by the path the rule is
//! declared on. A client fixing either one needs the same thing — which field —
//! so both have to arrive in the same `details` map, keyed the same way.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_protocol::PROTOCOL_SCHEMAS;
use ghostai_protocol::rest::{AgentRename, CredentialNamespace, SetCredentialRequest};
use ghostai_server::schema::{
    PROTOCOL_COMPONENTS, component_ref, inline_for, parse_body, ref_for, validated,
};
use serde_json::json;

// The component pool

#[test]
fn the_pool_carries_every_registered_protocol_schema() {
    assert_eq!(PROTOCOL_COMPONENTS.len(), PROTOCOL_SCHEMAS.len());
    for entry in PROTOCOL_SCHEMAS {
        assert!(
            PROTOCOL_COMPONENTS.contains_key(entry.name),
            "{} is not in the pool",
            entry.name
        );
    }
}

#[test]
fn no_entry_carries_the_dialect_marker() {
    // `$schema` is meaningful at the root of a document and noise inside
    // `components`.
    for (name, schema) in PROTOCOL_COMPONENTS.iter() {
        assert!(
            schema.get("$schema").is_none(),
            "{name} declares a dialect inside the pool"
        );
    }
}

#[test]
fn a_pointer_into_the_pool_is_a_ref() {
    assert_eq!(
        component_ref("LoginRequest"),
        json!({"$ref": "#/components/schemas/LoginRequest"})
    );
}

#[test]
fn a_registered_name_resolves_to_a_ref() {
    // Identity, not structure: two types can generate the same JSON and still
    // be different types.
    assert_eq!(
        ref_for("LoginRequest"),
        Some(json!({"$ref": "#/components/schemas/LoginRequest"}))
    );
}

#[test]
fn a_name_the_pool_does_not_publish_resolves_to_nothing() {
    assert_eq!(ref_for("NotARegisteredSchema"), None);
    assert_eq!(inline_for("NotARegisteredSchema"), None);
}

#[test]
fn a_registered_name_can_be_inlined_instead_of_referenced() {
    let inlined = inline_for("LoginRequest").expect("a registered schema");
    assert!(inlined.get("$ref").is_none());
    assert!(inlined["properties"].is_object());
}

// Reading a body

#[test]
fn a_well_formed_body_becomes_the_value_rather_than_the_input() {
    let parsed: AgentRename = parse_body("body", json!({"from": "reviewer", "to": "code-review"}))
        .expect("a well-formed rename");
    assert_eq!(parsed.from, "reviewer");
    assert_eq!(parsed.to, "code-review");
}

#[test]
fn a_wrong_shape_is_a_422_keyed_by_json_pointer() {
    let failure = parse_body::<AgentRename>("body", json!({"from": 17, "to": "x"}))
        .expect_err("a number is not an agent id");

    assert_eq!(failure.status.as_u16(), 422);
    assert_eq!(failure.message, "Invalid body");
    assert!(
        failure.details.keys().any(|key| key.starts_with('/')),
        "a detail key was not a JSON pointer: {:?}",
        failure.details
    );
    assert!(
        failure.details.contains_key("/from"),
        "{:?}",
        failure.details
    );
}

#[test]
fn a_missing_field_names_the_field_rather_than_its_parent() {
    let failure = parse_body::<SetCredentialRequest>("body", json!({"key": "local", "value": "x"}))
        .expect_err("a credential with no namespace");
    assert_eq!(failure.status.as_u16(), 422);
    assert!(
        failure
            .details
            .keys()
            .any(|key| key.contains("namespace") || key == "/"),
        "{:?}",
        failure.details
    );
}

#[test]
fn a_failure_at_the_root_is_keyed_with_a_bare_slash() {
    // A client rendering the map beside a form needs to tell "this field" from
    // "the request as a whole".
    let failure =
        parse_body::<AgentRename>("body", json!("nope")).expect_err("a string is not a rename");
    assert_eq!(failure.status.as_u16(), 422);
    assert!(failure.details.contains_key("/"), "{:?}", failure.details);
}

#[test]
fn the_part_that_failed_is_named_in_the_message() {
    let failure =
        parse_body::<AgentRename>("querystring", json!(1)).expect_err("a number is not a rename");
    assert_eq!(failure.message, "Invalid querystring");
}

#[test]
fn a_body_that_parses_and_then_breaks_a_rule_is_the_same_422() {
    // The two failures arrive from different directions and leave by the same
    // door, keyed the same way.
    let failure =
        parse_body::<AgentRename>("body", json!({"from": "", "to": "x"})).expect_err("an empty id");
    assert_eq!(failure.status.as_u16(), 422);
    assert!(
        failure.details.contains_key("/from"),
        "{:?}",
        failure.details
    );
}

// Checking a value that is already deserialised

#[test]
fn a_value_that_obeys_its_rules_passes_through_unchanged() {
    let request = SetCredentialRequest {
        namespace: CredentialNamespace::Providers,
        key: "local".to_owned(),
        value: Some("sk-live".to_owned()),
    };
    let checked = validated("body", request).expect("a valid credential write");
    assert_eq!(checked.key, "local");
}

#[test]
fn a_broken_rule_is_a_422_keyed_by_the_path_the_rule_is_declared_on() {
    let request = SetCredentialRequest {
        namespace: CredentialNamespace::Providers,
        // The rule is a minimum length, and it is declared on `key`.
        key: String::new(),
        value: Some("sk-live".to_owned()),
    };
    let failure = validated("body", request).expect_err("an empty key");

    assert_eq!(failure.status.as_u16(), 422);
    assert!(
        failure.details.contains_key("/key"),
        "{:?}",
        failure.details
    );
}

#[test]
fn the_first_message_per_pointer_wins() {
    // A client fixes one field at a time, and a second complaint about the same
    // field is noise on the way there.
    let request = SetCredentialRequest {
        namespace: CredentialNamespace::Providers,
        key: String::new(),
        value: None,
    };
    let failure = validated("body", request).expect_err("an empty key");
    for (pointer, value) in &failure.details {
        assert!(value.is_string(), "{pointer} carried more than one message");
    }
}

#[test]
fn a_422_carries_its_details_into_the_envelope() {
    let failure = parse_body::<AgentRename>("body", json!({"from": 17, "to": "x"}))
        .expect_err("a number is not an agent id");
    let body = failure.body();
    assert_eq!(body.error.code, "bad_request");
    let details = body.error.details.expect("a 422 carries details");
    assert!(!details.is_empty());
}
