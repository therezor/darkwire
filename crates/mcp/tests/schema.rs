//! A remote input schema, normalised and validated.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_core::ErrorKind;
use darkwire_mcp::{ArgValidator, compile_validator, normalise_schema};
use darkwire_protocol::json::Object;
use serde_json::{Value, json};

fn well_formed() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": { "type": "string", "description": "What to repeat." },
            "times": { "type": "integer", "minimum": 1, "description": "How many times." }
        },
        "required": ["text"],
        "additionalProperties": false
    })
}

fn as_value(parameters: &Object) -> Value {
    serde_json::to_value(parameters).unwrap()
}

#[test]
fn passes_a_well_formed_schema_through_with_no_issues() {
    let normalised = normalise_schema("echo", &well_formed()).unwrap();
    assert!(normalised.issues.is_empty());
    assert_eq!(as_value(&normalised.parameters), well_formed());
}

#[test]
fn strips_schema_keyword_which_providers_take_as_a_parameter_object() {
    let mut raw = well_formed();
    raw["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    let normalised = normalise_schema("echo", &raw).unwrap();
    assert!(normalised.parameters.get("$schema").is_none());
}

#[test]
fn seals_a_schema_that_said_nothing_about_extra_keys() {
    // Most servers simply omit it, and this repo's position is that a model
    // adding an undeclared key has misunderstood the tool.
    let normalised = normalise_schema(
        "echo",
        &json!({ "type": "object", "properties": { "text": { "type": "string", "description": "x" } } }),
    )
    .unwrap();
    assert_eq!(
        normalised.parameters.get("additionalProperties"),
        Some(&json!(false))
    );
}

#[test]
fn leaves_an_explicitly_open_schema_open_and_says_so() {
    let normalised = normalise_schema(
        "anything",
        &json!({ "type": "object", "properties": {}, "additionalProperties": true }),
    )
    .unwrap();
    assert_eq!(
        normalised.parameters.get("additionalProperties"),
        Some(&json!(true))
    );
    assert!(
        normalised.issues[0]
            .message
            .contains("undeclared arguments")
    );
}

#[test]
fn reports_an_undescribed_argument_rather_than_inventing_a_description() {
    let normalised = normalise_schema(
        "echo",
        &json!({ "type": "object", "properties": { "text": { "type": "string" } } }),
    )
    .unwrap();
    assert_eq!(normalised.issues.len(), 1);
    assert!(normalised.issues[0].message.contains("no description"));
    assert_eq!(normalised.issues[0].tool, "echo");
}

#[test]
fn json_round_trips_because_the_object_travels() {
    let normalised = normalise_schema("echo", &well_formed()).unwrap();
    let text = serde_json::to_string(&normalised.parameters).unwrap();
    let back: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(back, as_value(&normalised.parameters));
}

#[test]
fn refuses_a_schema_that_is_not_an_object_schema() {
    for raw in [
        json!(null),
        json!("nope"),
        json!({ "type": "string" }),
        json!([]),
        json!({}),
    ] {
        let error = normalise_schema("echo", &raw).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
    }
    let error = normalise_schema("echo", &json!({ "type": "string" })).unwrap_err();
    assert!(error.message.contains("must take an object, not string"));
}

fn validator() -> ArgValidator {
    let normalised = normalise_schema("echo", &well_formed()).unwrap();
    compile_validator("mcp_x_echo", &normalised.parameters).unwrap()
}

fn compile(name: &str, raw: &Value) -> ArgValidator {
    let normalised = normalise_schema(name, raw).unwrap();
    compile_validator(&format!("mcp_x_{name}"), &normalised.parameters).unwrap()
}

#[test]
fn accepts_a_valid_call() {
    let args = validator()
        .parse(Some(json!({ "text": "hi", "times": 2 })))
        .unwrap();
    assert_eq!(as_value(&args), json!({ "text": "hi", "times": 2 }));
}

#[test]
fn treats_an_absent_argument_object_as_an_empty_one() {
    // A model calling a no-argument tool emits nothing at all; the schema's own
    // `required` list is the right thing to judge that.
    let optional = compile("ping", &json!({ "type": "object", "properties": {} }));
    assert!(optional.parse(None).is_ok());
    assert!(optional.parse(Some(Value::Null)).is_ok());
    // But a required argument is still required.
    assert!(validator().parse(None).is_err());
}

#[test]
fn refuses_a_non_object_in_place_of_an_argument_object() {
    for raw in [json!("nope"), json!(42), json!([]), json!(true)] {
        let failure = validator().parse(Some(raw)).unwrap_err();
        assert!(failure.message.contains("expected an object of arguments"));
        assert_eq!(failure.issues[0].path, "");
    }
    let failure = validator().parse(Some(json!([]))).unwrap_err();
    assert!(failure.message.contains("an array"));
}

#[test]
fn refuses_an_unknown_argument_rather_than_stripping_it() {
    let failure = validator()
        .parse(Some(json!({ "text": "hi", "nope": 1 })))
        .unwrap_err();
    // The message names the tool, so a model reading it knows which call failed.
    assert!(failure.message.contains("mcp_x_echo"));
    assert!(
        failure
            .message
            .contains("nope: is not an argument of this tool")
    );
}

#[test]
fn keeps_an_unknown_argument_when_the_server_said_it_accepts_them() {
    let open = compile(
        "anything",
        &json!({ "type": "object", "properties": {}, "additionalProperties": true }),
    );
    let args = open.parse(Some(json!({ "whatever": 1 }))).unwrap();
    assert_eq!(as_value(&args), json!({ "whatever": 1 }));
}

#[test]
fn refuses_a_call_missing_a_required_argument() {
    let failure = validator().parse(Some(json!({ "times": 1 }))).unwrap_err();
    assert_eq!(failure.issues[0].path, "text");
    assert_eq!(failure.issues[0].message, "is required");
}

#[test]
fn coerces_the_string_form_of_a_number_as_models_emit_it() {
    let args = validator()
        .parse(Some(json!({ "text": "hi", "times": "3" })))
        .unwrap();
    assert_eq!(args.get("times"), Some(&json!(3)));
    let fractional = compile(
        "num",
        &json!({ "type": "object", "properties": { "n": { "type": "number", "description": "n" } } }),
    );
    let args = fractional.parse(Some(json!({ "n": "1.5" }))).unwrap();
    assert_eq!(args.get("n"), Some(&json!(1.5)));
}

#[test]
fn does_not_coerce_a_fractional_string_into_an_integer() {
    assert!(
        validator()
            .parse(Some(json!({ "text": "hi", "times": "1.5" })))
            .is_err()
    );
    assert!(
        validator()
            .parse(Some(json!({ "text": "hi", "times": "" })))
            .is_err()
    );
    assert!(
        validator()
            .parse(Some(json!({ "text": "hi", "times": "lots" })))
            .is_err()
    );
}

#[test]
fn does_not_coerce_in_the_other_direction() {
    // A number where a string was asked for is a model that has misunderstood
    // the tool, and quietly stringifying it would hide that.
    let failure = validator().parse(Some(json!({ "text": 1 }))).unwrap_err();
    assert_eq!(failure.issues[0].path, "text");
    assert_eq!(
        failure.issues[0].message,
        "expected string, received number"
    );
}

#[test]
fn honours_an_enum() {
    let pick = compile(
        "pick",
        &json!({ "type": "object", "properties": { "mode": { "type": "string", "enum": ["a", "b"], "description": "Which." } } }),
    );
    assert!(pick.parse(Some(json!({ "mode": "a" }))).is_ok());
    let refused = pick.parse(Some(json!({ "mode": "c" }))).unwrap_err();
    assert!(refused.message.contains("must be one of \"a\", \"b\""));
}

#[test]
fn accepts_a_union_of_declared_types() {
    let either = compile(
        "either",
        &json!({ "type": "object", "properties": { "value": { "type": ["string", "number"], "description": "Either." } } }),
    );
    assert!(either.parse(Some(json!({ "value": "a" }))).is_ok());
    assert!(either.parse(Some(json!({ "value": 1 }))).is_ok());
    let refused = either.parse(Some(json!({ "value": true }))).unwrap_err();
    assert!(
        refused
            .message
            .contains("expected string or number, received boolean")
    );
}

#[test]
fn honours_a_nested_constraint_the_server_declared() {
    let nested = compile(
        "nested",
        &json!({ "type": "object", "properties": { "filter": { "type": "object", "description": "Whatever the server means by this.", "properties": { "deep": { "type": "boolean" } } } } }),
    );
    assert!(
        nested
            .parse(Some(json!({ "filter": { "deeply": { "nested": true } } })))
            .is_ok()
    );
    let refused = nested
        .parse(Some(json!({ "filter": { "deep": "no" } })))
        .unwrap_err();
    assert_eq!(refused.issues[0].path, "filter.deep");
}

#[test]
fn honours_a_minimum_which_is_the_servers_word_on_its_own_arguments() {
    let failure = validator()
        .parse(Some(json!({ "text": "hi", "times": 0 })))
        .unwrap_err();
    assert_eq!(failure.issues[0].path, "times");
}

#[test]
fn reports_every_problem_at_once_not_just_the_first() {
    let failure = validator()
        .parse(Some(json!({ "text": 1, "nope": true })))
        .unwrap_err();
    assert_eq!(failure.issues.len(), 2);
}

#[test]
fn a_failure_becomes_an_invalid_input_error_with_the_issues_attached() {
    let failure = validator().parse(Some(json!({ "times": 1 }))).unwrap_err();
    let error = darkwire_core::WireError::from(failure);
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(
        error
            .details
            .get("issues")
            .and_then(Value::as_array)
            .is_some()
    );
}

#[test]
fn refuses_a_schema_that_reaches_outside_itself() {
    // A `$ref` to a URL is data from a socket asking this process to fetch
    // something; the validator refuses to compile rather than reaching out.
    let normalised = normalise_schema(
        "remote",
        &json!({ "type": "object", "properties": { "x": { "$ref": "https://example.test/schema.json", "description": "x" } } }),
    )
    .unwrap();
    let error = compile_validator("mcp_x_remote", &normalised.parameters).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("cannot compile"));
}
