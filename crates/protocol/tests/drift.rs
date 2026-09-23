//! The schema drift gate.
//!
//! `packages/protocol/schema/<Name>.json` is what the browser's zod schemas
//! say, written by `pnpm --filter @darkwire/protocol schema:dump` in input
//! mode; this generates the same document from the Rust type registered under
//! the same name and compares the two after both are normalised. Anything that
//! survives normalisation is real drift: a field, a bound, a default or a
//! variant one side has and the other does not.
//!
//! The normaliser implements the rules the two schema generators are known to
//! spell differently without meaning anything different by it:
//!
//! - `$schema`, `$id`, `title`, `description` and `examples` are dropped; they
//!   annotate, they do not constrain.
//! - Every `$ref` is inlined (the Rust generator already inlines; this keeps
//!   the rule symmetric).
//! - A nullable type is written `anyOf: [T, {type: null}]` whichever way it
//!   arrived (`type: [T, "null"]`, `oneOf`, or `enum` carrying `null`).
//! - `oneOf` becomes `anyOf`; `required`, `enum` and union members are sorted
//!   (members by their canonical JSON), and so are object keys.
//! - A `format` only one generator emits — `int64`, `uint64`, `double`,
//!   `uint16` and the rest of Rust's width hints — is dropped.
//! - Keywords that constrain nothing are dropped: `{}` and `true` are the same
//!   schema, `additionalProperties: true` is the same as its absence,
//!   `propertyNames: {type: string}` says what JSON already guarantees, and an
//!   empty `properties` map names no property.
//! - Integral floats are integers: `2.0` is `2`.
//!
//! Those last two bullets go beyond the plan's list and are the vacuous cases:
//! each is a spelling one generator uses for a constraint the other leaves
//! implicit, never a difference in what a document accepts.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use crate::common;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use darkwire_protocol::PROTOCOL_SCHEMAS;
use serde_json::{Map, Value};

use common::{canonical_numbers, diff};

/// Schemas the two generators cannot be made to agree on, with the reason.
/// Empty is the goal; an entry here is a documented exception, not a fix.
const ALLOWED_DRIFT: &[(&str, &str)] = &[];

fn schema_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/protocol/schema")
}

fn typescript_schemas() -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for entry in fs::read_dir(schema_dir())
        .expect("packages/protocol/schema exists; run pnpm --filter @darkwire/protocol schema:dump")
    {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json") {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let text = fs::read_to_string(&path).unwrap();
            out.insert(name, serde_json::from_str(&text).unwrap());
        }
    }
    out
}

const FORMATS_ONE_SIDE_EMITS: &[&str] = &[
    "int8", "int16", "int32", "int64", "uint8", "uint16", "uint32", "uint64", "float", "double",
];

fn is_null_schema(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|o| o.len() == 1 && o.get("type") == Some(&Value::from("null")))
}

fn inline_refs(value: Value, defs: &Map<String, Value>) -> Value {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref") {
                let name = reference.rsplit('/').next().unwrap_or_default();
                let target = defs
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| panic!("unresolvable $ref {reference}"));
                return inline_refs(target, defs);
            }
            Value::Object(
                map.into_iter()
                    .filter(|(k, _)| k != "$defs" && k != "definitions")
                    .map(|(k, v)| (k, inline_refs(v, defs)))
                    .collect(),
            )
        }
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|v| inline_refs(v, defs)).collect())
        }
        other => other,
    }
}

fn normalise(value: Value) -> Value {
    let value = canonical_numbers(value);
    match value {
        Value::Bool(true) => Value::Object(Map::new()),
        Value::Object(mut map) => {
            for key in ["$schema", "$id", "title", "description", "examples"] {
                map.remove(key);
            }
            if map.get("additionalProperties") == Some(&Value::Bool(true))
                || map
                    .get("additionalProperties")
                    .is_some_and(|v| v.as_object().is_some_and(Map::is_empty))
            {
                map.remove("additionalProperties");
            }
            if map
                .get("propertyNames")
                .is_some_and(|v| v == &serde_json::json!({"type": "string"}))
            {
                map.remove("propertyNames");
            }
            if map
                .get("properties")
                .is_some_and(|v| v.as_object().is_some_and(Map::is_empty))
            {
                map.remove("properties");
            }
            if map
                .get("format")
                .and_then(Value::as_str)
                .is_some_and(|f| FORMATS_ONE_SIDE_EMITS.contains(&f))
            {
                map.remove("format");
            }
            if let Some(Value::Array(types)) = map.get("type").cloned()
                && types.iter().any(|t| t == "null")
            {
                let rest: Vec<Value> = types.into_iter().filter(|t| t != "null").collect();
                let mut inner = map.clone();
                if rest.len() == 1 {
                    inner.insert("type".into(), rest.into_iter().next().unwrap());
                } else {
                    inner.insert("type".into(), Value::Array(rest));
                }
                if let Some(Value::Array(values)) = inner.get_mut("enum") {
                    values.retain(|v| !v.is_null());
                }
                return normalise(
                    serde_json::json!({ "anyOf": [Value::Object(inner), {"type": "null"}] }),
                );
            }
            if let Some(members) = map.remove("oneOf") {
                map.insert("anyOf".into(), members);
            }
            let mut out = Map::new();
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            for key in keys {
                let value = map.remove(&key).unwrap();
                let value = match key.as_str() {
                    // Property names are data, not keywords: a field called
                    // `required` or `enum` must not be read as one.
                    "properties" => {
                        let Value::Object(props) = value else {
                            panic!("properties is not an object")
                        };
                        let sorted: BTreeMap<String, Value> =
                            props.into_iter().map(|(k, v)| (k, normalise(v))).collect();
                        Value::Object(sorted.into_iter().collect())
                    }
                    "required" | "enum" => {
                        let Value::Array(items) = value else {
                            panic!("{key} is not an array")
                        };
                        let mut items: Vec<Value> = items.into_iter().map(normalise).collect();
                        items.sort_by_key(std::string::ToString::to_string);
                        Value::Array(items)
                    }
                    "anyOf" => {
                        let Value::Array(items) = value else {
                            panic!("anyOf is not an array")
                        };
                        let mut items: Vec<Value> = items.into_iter().map(normalise).collect();
                        // A nullable union folds its null into one branch.
                        let had_null = items.iter().any(is_null_schema);
                        items.retain(|v| !is_null_schema(v));
                        if had_null {
                            items.push(serde_json::json!({"type": "null"}));
                        }
                        items.sort_by_key(std::string::ToString::to_string);
                        Value::Array(items)
                    }
                    "default" | "const" => value,
                    _ => normalise(value),
                };
                out.insert(key, value);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(normalise).collect()),
        other => other,
    }
}

fn normalise_root(value: Value) -> Value {
    let defs = value
        .get("$defs")
        .or_else(|| value.get("definitions"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    normalise(inline_refs(value, &defs))
}

#[test]
fn every_registered_schema_matches_the_browser() {
    let typescript = typescript_schemas();
    let mut report = String::new();
    let mut unexpected_pass = Vec::new();
    for entry in PROTOCOL_SCHEMAS {
        let Some(theirs) = typescript.get(entry.name) else {
            let _ = writeln!(
                report,
                "{}: no packages/protocol/schema/{}.json",
                entry.name, entry.name
            );
            continue;
        };
        let ours = serde_json::to_value(entry.generate()).unwrap();
        let (theirs, ours) = (normalise_root(theirs.clone()), normalise_root(ours));
        let allowed = ALLOWED_DRIFT.iter().find(|(name, _)| *name == entry.name);
        if theirs == ours {
            if allowed.is_some() {
                unexpected_pass.push(entry.name);
            }
            continue;
        }
        if let Some((_, reason)) = allowed {
            eprintln!("{}: allowed drift ({reason})", entry.name);
            continue;
        }
        let mut lines = String::new();
        diff("", &theirs, &ours, &mut lines);
        let _ = write!(report, "{}:\n{lines}", entry.name);
    }
    assert!(
        report.is_empty(),
        "schema drift between zod and serde:\n{report}"
    );
    assert!(
        unexpected_pass.is_empty(),
        "these schemas match now; remove them from ALLOWED_DRIFT: {unexpected_pass:?}"
    );
}

#[test]
fn every_browser_schema_has_a_rust_type() {
    let registered: Vec<&str> = PROTOCOL_SCHEMAS.iter().map(|e| e.name).collect();
    let missing: Vec<String> = typescript_schemas()
        .keys()
        .filter(|name| !registered.contains(&name.as_str()))
        .cloned()
        .collect();
    assert!(
        missing.is_empty(),
        "schemas the browser publishes and this crate does not: {missing:?}"
    );
}
