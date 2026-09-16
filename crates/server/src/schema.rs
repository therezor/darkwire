//! One schema per thing, two artefacts from it.
//!
//! A route names a protocol type for its body, query and response. The same
//! type is what deserialises and validates a request and what the generated
//! OpenAPI document describes, so the reference cannot describe a shape the
//! server does not enforce — the drift that makes a hand-maintained API
//! document worse than none.
//!
//! [`PROTOCOL_COMPONENTS`] is the `$defs` pool: every schema in
//! `darkwire_protocol::PROTOCOL_SCHEMAS`, generated once and published under
//! `components.schemas`. A route whose response *is* a registered protocol
//! schema emits a `$ref` to it; anything else inlines.
//!
//! Request bodies always inline. The pool is generated in the serialise
//! direction, and a request body that pointed at it would advertise every
//! defaulted field as required — telling a client it must send exactly the
//! values the schema exists to supply.

use std::sync::LazyLock;

use darkwire_protocol::schemas::{PROTOCOL_SCHEMAS, registered};
use garde::Validate;
use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::errors::HttpError;

/// The `$defs` pool: every protocol schema, generated once.
///
/// `$schema` is meaningful at the root of a document and noise inside
/// `components`, so it is stripped on the way in.
pub static PROTOCOL_COMPONENTS: LazyLock<IndexMap<String, Value>> = LazyLock::new(|| {
    PROTOCOL_SCHEMAS
        .iter()
        .map(|entry| {
            let mut schema = entry.generate().to_value();
            if let Some(object) = schema.as_object_mut() {
                object.shift_remove("$schema");
            }
            (entry.name.to_owned(), schema)
        })
        .collect()
});

/// A pointer into the pool, for a route whose response is a registered schema.
pub fn component_ref(name: &str) -> Value {
    serde_json::json!({ "$ref": format!("#/components/schemas/{name}") })
}

/// A `$ref` when the name is one the pool publishes, and `None` otherwise.
///
/// Identity, not structure: two types can generate the same JSON and still be
/// different types, and what decides whether a response "is the protocol's
/// `LoginResponse`" is whether the route named that schema.
pub fn ref_for(name: &str) -> Option<Value> {
    registered(name).map(|entry| component_ref(entry.name))
}

/// The schema for one registered name, inlined rather than referenced.
pub fn inline_for(name: &str) -> Option<Value> {
    PROTOCOL_COMPONENTS.get(name).cloned()
}

/// Reads a request body into a protocol type, reporting a 422 with field-level
/// detail.
///
/// Two failures, one answer. A body whose *shape* is wrong fails in serde and
/// is keyed by the JSON pointer serde walked to; a body that parsed and then
/// broke a rule fails in `garde` and is keyed by the path the rule is declared
/// on. A client fixing either one needs the same thing — which field — so both
/// arrive in the same `details` map.
pub fn parse_body<T>(what: &str, raw: Value) -> Result<T, HttpError>
where
    T: DeserializeOwned + Validate<Context = ()>,
{
    let value: T = serde_path_to_error::deserialize(raw).map_err(|error| {
        HttpError::unprocessable(format!("Invalid {what}")).with_detail(
            pointer_of(&error.path().to_string()),
            error.inner().to_string(),
        )
    })?;
    validated(what, value)
}

/// Checks a value that is already deserialised.
///
/// Split out because a query string arrives as pairs rather than as JSON, and
/// the validation half is the same for both.
pub fn validated<T>(what: &str, value: T) -> Result<T, HttpError>
where
    T: Validate<Context = ()>,
{
    match value.validate() {
        Ok(()) => Ok(value),
        Err(report) => {
            let mut details: IndexMap<String, Value> = IndexMap::new();
            for (path, error) in report.iter() {
                // The first message per pointer wins: a client fixes one field
                // at a time, and a second complaint about the same field is
                // noise on the way there.
                details
                    .entry(pointer_of(&path.to_string()))
                    .or_insert_with(|| Value::from(error.to_string()));
            }
            Err(HttpError::unprocessable(format!("Invalid {what}")).with_details(details))
        }
    }
}

/// A dotted path as the JSON pointer `ErrorResponse.details` is keyed by.
///
/// The root is `/`, which is what a client rendering the map beside a form
/// needs in order to tell "this field" from "the request as a whole".
fn pointer_of(path: &str) -> String {
    let trimmed = path.trim_matches('.');
    if trimmed.is_empty() {
        return "/".to_owned();
    }
    let mut out = String::new();
    for segment in trimmed.split('.') {
        for part in segment.split(['[', ']']).filter(|part| !part.is_empty()) {
            out.push('/');
            out.push_str(part);
        }
    }
    if out.is_empty() { "/".to_owned() } else { out }
}
