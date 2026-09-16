//! The generated document, checked for the properties a validator cannot know
//! about.
//!
//! Parity with the TypeScript document is deliberately **not** a whole-document
//! diff: two documents can differ in key order and description wording and
//! still describe the same API, and a gate that fails on those is one nobody
//! can keep green. What is enforced instead is the protocol crate's per-schema
//! drift test — which is where a shape can actually go wrong — plus the three
//! properties below, which are about this document's own consistency.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_server::manifest::{ROUTE_MANIFEST, RouteAuth};
use darkwire_server::openapi::{ROUTE_DOCS, openapi_document};
use darkwire_server::version::SERVER_VERSION;
use serde_json::Value;

fn document() -> Value {
    openapi_document()
}

/// The manifest's `:key` is the document's `{key}`.
fn document_path(path: &str) -> String {
    path.split('/')
        .map(|segment| match segment.strip_prefix(':') {
            Some(name) => format!("{{{name}}}"),
            None => segment.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn refs_in(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                refs_in(item, found);
            }
        }
        Value::Object(object) => {
            for (key, item) in object {
                if key == "$ref"
                    && let Some(reference) = item.as_str()
                {
                    found.push(reference.to_owned());
                } else {
                    refs_in(item, found);
                }
            }
        }
        _ => {}
    }
}

#[test]
fn it_declares_openapi_three_one_and_this_build_s_version() {
    let doc = document();
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(doc["info"]["version"], SERVER_VERSION);
    assert_eq!(doc["info"]["title"], "DarkWire");
}

#[test]
fn it_describes_every_manifest_route_exactly_once() {
    let doc = document();
    let paths = doc["paths"].as_object().expect("paths");

    let mut operations: Vec<String> = Vec::new();
    for methods in paths.values() {
        for operation in methods.as_object().expect("an operation map").values() {
            operations.push(
                operation["operationId"]
                    .as_str()
                    .expect("every operation names itself")
                    .to_owned(),
            );
        }
    }
    operations.sort();

    let mut expected: Vec<String> = ROUTE_DOCS
        .iter()
        .map(|doc| doc.id.as_str().to_owned())
        .collect();
    expected.sort();
    assert_eq!(operations, expected);
}

#[test]
fn every_documented_route_is_in_the_manifest() {
    for doc in ROUTE_DOCS {
        assert!(
            ROUTE_MANIFEST.iter().any(|route| route.id == doc.id),
            "{} is documented but not served",
            doc.id.as_str()
        );
    }
}

#[test]
fn every_served_route_that_is_part_of_the_api_is_documented() {
    for route in ROUTE_MANIFEST {
        if route.path.starts_with("/api/_test/") {
            // The end-to-end seams are deliberately not part of the API a
            // client may rely on.
            continue;
        }
        assert!(
            ROUTE_DOCS.iter().any(|doc| doc.id == route.id),
            "{} is served but not documented",
            route.id.as_str()
        );
    }
}

#[test]
fn every_path_and_method_the_manifest_names_is_in_the_document() {
    let doc = document();
    for route in ROUTE_MANIFEST {
        if route.path.starts_with("/api/_test/") {
            continue;
        }
        let key = document_path(route.path);
        let method = route.method.as_str().to_lowercase();
        assert!(
            doc["paths"][&key][&method].is_object(),
            "{} {} is missing",
            route.method.as_str(),
            key
        );
    }
}

#[test]
fn every_reference_resolves_against_the_component_pool() {
    let doc = document();
    let names: Vec<String> = doc["components"]["schemas"]
        .as_object()
        .expect("a component pool")
        .keys()
        .cloned()
        .collect();

    let mut found = Vec::new();
    refs_in(&doc["paths"], &mut found);
    assert!(!found.is_empty(), "a document with no references at all");

    let dangling: Vec<&String> = found
        .iter()
        .filter(|reference| {
            let name = reference.trim_start_matches("#/components/schemas/");
            !names.iter().any(|known| known == name)
        })
        .collect();
    assert!(dangling.is_empty(), "dangling references: {dangling:?}");
}

#[test]
fn a_required_route_accepts_either_carrier() {
    let doc = document();
    // A list of two alternatives is what "either satisfies it" means in
    // OpenAPI.
    let security = &doc["paths"]["/api/status"]["get"]["security"];
    assert_eq!(
        security,
        &serde_json::json!([{"cookieAuth": []}, {"bearerAuth": []}])
    );
}

#[test]
fn a_signed_route_lists_no_scheme_at_all() {
    let doc = document();
    let media = ROUTE_MANIFEST
        .iter()
        .find(|route| route.auth == RouteAuth::Signed)
        .expect("one signed route");
    // Its credential is the path, and a document that named a scheme here would
    // tell a client to attach one that is not accepted.
    assert_eq!(
        doc["paths"][document_path(media.path)]["get"]["security"],
        serde_json::json!([])
    );
}

#[test]
fn a_public_route_lists_no_scheme_either() {
    let doc = document();
    assert_eq!(
        doc["paths"]["/api/health"]["get"]["security"],
        serde_json::json!([])
    );
}

#[test]
fn both_security_schemes_are_declared() {
    let doc = document();
    let schemes = &doc["components"]["securitySchemes"];
    assert_eq!(schemes["cookieAuth"]["type"], "apiKey");
    assert_eq!(schemes["cookieAuth"]["in"], "cookie");
    assert_eq!(
        schemes["cookieAuth"]["name"],
        darkwire_server::auth::SESSION_COOKIE
    );
    assert_eq!(schemes["bearerAuth"]["type"], "http");
    assert_eq!(schemes["bearerAuth"]["scheme"], "bearer");
}

#[test]
fn it_documents_a_query_parameter_the_route_actually_enforces() {
    let doc = document();
    let parameters = doc["paths"]["/api/sessions"]["get"]["parameters"]
        .as_array()
        .expect("the session listing takes parameters");
    let limit = parameters
        .iter()
        .find(|parameter| parameter["name"] == "limit")
        .expect("a limit parameter");
    // Coerced from a string at the door and documented as the integer it
    // becomes — with the cap, so a client knows before it is refused.
    assert_eq!(limit["in"], "query");
    assert_eq!(limit["schema"]["type"], "integer");
    assert_eq!(limit["schema"]["maximum"], 200);
    assert_eq!(limit["schema"]["default"], 50);
}

#[test]
fn a_path_parameter_is_required_by_definition() {
    let doc = document();
    let parameters = doc["paths"]["/api/sessions/{key}"]["get"]["parameters"]
        .as_array()
        .expect("the session route takes a key");
    let key = parameters
        .iter()
        .find(|parameter| parameter["name"] == "key")
        .expect("a key parameter");
    assert_eq!(key["in"], "path");
    assert_eq!(key["required"], true);
}

#[test]
fn a_request_body_points_at_the_pool() {
    let doc = document();
    assert_eq!(
        doc["paths"]["/api/auth/login"]["post"]["requestBody"]["content"]["application/json"]["schema"]
            ["$ref"],
        "#/components/schemas/LoginRequest"
    );
}

#[test]
fn every_operation_carries_an_error_response() {
    let doc = document();
    for (path, methods) in doc["paths"].as_object().expect("paths") {
        for (method, operation) in methods.as_object().expect("operations") {
            assert!(
                operation["responses"]["default"].is_object(),
                "{method} {path} documents no error shape"
            );
        }
    }
}

#[test]
fn a_route_that_answers_with_no_body_still_documents_a_response() {
    let doc = document();
    // Every operation must carry at least one response, and a route that
    // answers with no body still answers.
    assert!(doc["paths"]["/api/auth/logout"]["post"]["responses"]["200"].is_object());
}

#[test]
fn the_component_pool_holds_every_protocol_schema() {
    let doc = document();
    let pool = doc["components"]["schemas"]
        .as_object()
        .expect("a component pool");
    assert_eq!(pool.len(), darkwire_protocol::PROTOCOL_SCHEMAS.len());
    for entry in darkwire_protocol::PROTOCOL_SCHEMAS {
        assert!(pool.contains_key(entry.name), "{} is missing", entry.name);
    }
}

#[test]
fn no_component_carries_a_dialect_declaration() {
    let doc = document();
    for (name, schema) in doc["components"]["schemas"].as_object().expect("pool") {
        // `$schema` is meaningful at the root of a document and noise inside
        // `components`.
        assert!(schema.get("$schema").is_none(), "{name} declares a dialect");
    }
}
