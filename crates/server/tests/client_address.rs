//! The caller's address, with and without a trusted proxy in front.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "a request that cannot be built is a failing test either way"
)]

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, HeaderValue};
use axum::routing::get;
use darkwire_server::client_address::{TrustedProxies, client_of, resolve_client};
use tower::ServiceExt as _;

fn proxies(entries: &[&str]) -> TrustedProxies {
    TrustedProxies::new(&entries.iter().map(|e| (*e).to_owned()).collect::<Vec<_>>())
}

fn forwarded(values: &[&str]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for value in values {
        headers.append("x-forwarded-for", HeaderValue::from_str(value).unwrap());
    }
    headers
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

#[test]
fn an_untrusted_peer_is_the_caller_whatever_it_forwards() {
    let none = proxies(&[]);
    let headers = forwarded(&["203.0.113.9"]);
    assert_eq!(none.client(ip("10.0.0.1"), &headers), "10.0.0.1");
    let other = proxies(&["10.9.9.9"]);
    assert_eq!(other.client(ip("10.0.0.1"), &headers), "10.0.0.1");
}

#[test]
fn a_trusted_peer_names_the_hop_before_it() {
    let trusted = proxies(&["10.0.0.1"]);
    assert_eq!(
        trusted.client(ip("10.0.0.1"), &forwarded(&["203.0.113.9"])),
        "203.0.113.9"
    );
    // What the client wrote itself sits to the left and is never read.
    assert_eq!(
        trusted.client(ip("10.0.0.1"), &forwarded(&["6.6.6.6, 203.0.113.9"])),
        "203.0.113.9"
    );
    // Several headers read as one list, in order.
    assert_eq!(
        trusted.client(ip("10.0.0.1"), &forwarded(&["6.6.6.6", "203.0.113.9"])),
        "203.0.113.9"
    );
}

#[test]
fn trusted_hops_inside_a_block_are_skipped() {
    let chain = proxies(&["10.0.0.0/8", "not an address"]);
    assert_eq!(
        chain.client(
            ip("10.0.0.1"),
            &forwarded(&["203.0.113.9, 10.1.1.1, 10.2.2.2"])
        ),
        "203.0.113.9"
    );
    // Every hop trusted, or none forwarded: the proxy itself.
    assert_eq!(
        chain.client(ip("10.0.0.1"), &forwarded(&["10.3.3.3"])),
        "10.0.0.1"
    );
    assert_eq!(chain.client(ip("10.0.0.1"), &HeaderMap::new()), "10.0.0.1");
}

#[test]
fn an_unreadable_hop_stops_at_the_proxy() {
    let trusted = proxies(&["10.0.0.1"]);
    assert_eq!(
        trusted.client(ip("10.0.0.1"), &forwarded(&["203.0.113.9, unknown"])),
        "10.0.0.1"
    );
}

#[test]
fn a_mapped_address_counts_as_its_ipv4_form() {
    let trusted = proxies(&["10.0.0.1"]);
    assert_eq!(
        trusted.client(ip("::ffff:10.0.0.1"), &forwarded(&["203.0.113.9"])),
        "203.0.113.9"
    );
    assert_eq!(
        proxies(&[]).client(ip("::ffff:192.0.2.1"), &HeaderMap::new()),
        "192.0.2.1"
    );
}

#[tokio::test]
async fn the_layer_records_the_caller_for_every_handler_below_it() {
    let router = Router::new()
        .route(
            "/",
            get(|request: Request| async move { client_of(request.extensions()) }),
        )
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(proxies(&["10.0.0.1"])),
            resolve_client,
        ));

    let ask = |peer: Option<&str>| {
        let mut request = Request::builder()
            .uri("/")
            .header("x-forwarded-for", "203.0.113.9")
            .body(Body::empty())
            .unwrap();
        if let Some(peer) = peer {
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::new(ip(peer), 4000)));
        }
        router.clone().oneshot(request)
    };
    let text = |response: axum::response::Response| async move {
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    };

    assert_eq!(
        text(ask(Some("10.0.0.1")).await.unwrap()).await,
        "203.0.113.9"
    );
    assert_eq!(text(ask(Some("10.0.0.2")).await.unwrap()).await, "10.0.0.2");
    assert_eq!(text(ask(None).await.unwrap()).await, "local");
}
