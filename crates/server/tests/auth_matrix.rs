//! Every route in the manifest, reached without a credential.
//!
//! This is the test the manifest exists for. "Remembered to add the auth check"
//! is not a property a codebase can hold onto across sixty-odd routes; walking
//! the same table the router was built from, and asserting the answer matches
//! the class the table declares, is.
//!
//! It deliberately does not care what a route *does* — a `Required` route may
//! answer 404, 409 or 500 once it has a session, and none of that is this
//! test's business. It cares about exactly one bit: whether the door opened.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ghostai_server::manifest::{ROUTE_MANIFEST, Route, RouteAuth, RouteMethod};
use ghostai_server::testkit::{TestServer, TestServerOptions, start_test_server};
use tower::ServiceExt as _;

fn server() -> TestServer {
    start_test_server(TestServerOptions::default()).expect("a test server")
}

/// A path a request can actually be sent to: every `:param` filled with a value
/// that names nothing.
///
/// Naming nothing is the point. A `Required` route must refuse before it looks
/// anything up, so a path that resolves and a path that does not have to give
/// the same answer — and the one that does not is the one a test can construct
/// without seeding a row for all sixty-three.
fn concrete_path(route: &Route) -> String {
    route
        .path
        .split('/')
        .map(|segment| {
            if segment.starts_with(':') {
                "no-such-thing"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn method_of(method: RouteMethod) -> axum::http::Method {
    match method {
        RouteMethod::GET => axum::http::Method::GET,
        RouteMethod::POST => axum::http::Method::POST,
        RouteMethod::PATCH => axum::http::Method::PATCH,
        RouteMethod::PUT => axum::http::Method::PUT,
        RouteMethod::DELETE => axum::http::Method::DELETE,
    }
}

async fn status_of(test: &TestServer, route: &Route, token: Option<&str>) -> StatusCode {
    let mut builder = Request::builder()
        .method(method_of(route.method))
        .uri(concrete_path(route))
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = builder
        .body(Body::from("{}"))
        .expect("a well-formed request");
    test.router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered")
        .status()
}

#[tokio::test]
async fn every_required_route_refuses_an_unauthenticated_caller() {
    let test = server();
    for route in ROUTE_MANIFEST {
        if route.auth != RouteAuth::Required {
            continue;
        }
        let status = status_of(&test, route, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{} {} answered {status} without a credential",
            route.method.as_str(),
            route.path
        );
    }
}

#[tokio::test]
async fn every_public_route_answers_an_unauthenticated_caller() {
    let test = server();
    for route in ROUTE_MANIFEST {
        if route.auth != RouteAuth::Public {
            continue;
        }
        let status = status_of(&test, route, None).await;
        assert_ne!(
            status,
            StatusCode::UNAUTHORIZED,
            "{} {} refused a caller it is supposed to serve",
            route.method.as_str(),
            route.path
        );
    }
}

#[tokio::test]
async fn the_signed_route_refuses_a_session_as_well_as_no_credential() {
    let test = server();
    let media = ROUTE_MANIFEST
        .iter()
        .find(|route| route.auth == RouteAuth::Signed)
        .expect("one signed route");

    // A session is *not* accepted there and a signature is not accepted
    // anywhere else, so neither carrier widens the other's reach.
    assert_eq!(
        status_of(&test, media, None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status_of(&test, media, Some(&test.token)).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_valid_session_opens_every_required_route() {
    let test = server();
    for route in ROUTE_MANIFEST {
        if route.auth != RouteAuth::Required {
            continue;
        }
        // A fresh session per route, because one of the routes under test is
        // the logout — which revokes the credential it was presented, exactly
        // as it should. Reusing one token would make every route after it in
        // manifest order look like an authentication failure.
        let token = test.auth.issue("matrix").expect("a session").token;
        let status = status_of(&test, route, Some(&token)).await;
        assert_ne!(
            status,
            StatusCode::UNAUTHORIZED,
            "{} {} refused a valid session",
            route.method.as_str(),
            route.path
        );
    }
}

#[tokio::test]
async fn a_token_that_is_not_a_session_is_refused() {
    let test = server();
    let status = status_of(
        &test,
        ROUTE_MANIFEST
            .iter()
            .find(|route| route.path == "/api/status")
            .expect("the status route"),
        Some("not-a-real-token"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_refusal_carries_the_one_error_envelope() {
    let test = server();
    let request = Request::builder()
        .method(axum::http::Method::GET)
        .uri("/api/status")
        .body(Body::empty())
        .expect("a well-formed request");
    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("a body");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("a JSON envelope");
    assert_eq!(body["error"]["code"], "unauthorized");
    assert!(body["error"]["message"].is_string());
}

#[tokio::test]
async fn no_route_is_served_that_is_not_in_the_manifest() {
    let test = server();
    // The router is built from the manifest and from nothing else, so an API
    // path the manifest does not carry has to be a JSON 404 rather than a route
    // somebody registered beside the table.
    for path in [
        "/api/nope",
        "/api/sessions/x/nope",
        "/api/admin",
        "/api/settings/secret",
    ] {
        let request = Request::builder()
            .uri(path)
            .header("authorization", format!("Bearer {}", test.token))
            .body(Body::empty())
            .expect("a well-formed request");
        let response = test
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("the router answered");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn authentication_disabled_opens_the_required_routes_and_not_the_signed_one() {
    let mut config = ghostai_protocol::config::Config::default();
    config.server.auth.enabled = false;
    let test = start_test_server(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    })
    .expect("a test server");

    let status = ROUTE_MANIFEST
        .iter()
        .find(|route| route.path == "/api/status")
        .expect("the status route");
    assert_ne!(
        status_of(&test, status, None).await,
        StatusCode::UNAUTHORIZED
    );

    // The signature is a different carrier with a different rule: switching
    // authentication off does not mint one.
    let media = ROUTE_MANIFEST
        .iter()
        .find(|route| route.auth == RouteAuth::Signed)
        .expect("one signed route");
    assert_eq!(
        status_of(&test, media, None).await,
        StatusCode::UNAUTHORIZED
    );
}
