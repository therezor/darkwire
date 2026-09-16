//! The one suite here that binds a port.
//!
//! It binds `127.0.0.1:0`, and the alternative — asserting the handler by
//! calling a private function — would prove nothing about the thing that
//! matters: that a redirect arriving from a browser reaches the right pending
//! authorization.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::time::Duration;

use darkwire_core::ErrorKind;
use darkwire_mcp::{
    CALLBACK_PATH, CallbackListener, CallbackListenerOptions, DEFAULT_CALLBACK_PORT,
};
use darkwire_security::OsRandom;
use futures::FutureExt as _;

fn listener() -> CallbackListener {
    CallbackListener::new(CallbackListenerOptions {
        random: Arc::new(OsRandom),
        // Ephemeral, so a developer running the suite while a DarkWire is up
        // does not collide with its fixed port.
        port: Some(0),
    })
}

/// Fetches one redirect, and holds the handler to answering promptly.
///
/// The bound is the point of the helper as much as the request is: settling
/// the last authorization stops the listener, and a stop taken on the request's
/// own task waits for the request to finish while the request waits for the
/// stop — a browser left on an empty page until the shutdown grace expires.
/// A second is far longer than loopback needs and far shorter than that grace.
async fn visit(url: &str) -> (u16, String) {
    let response = tokio::time::timeout(Duration::from_secs(1), reqwest::get(url))
        .await
        .expect("the redirect is answered rather than held open")
        .unwrap();
    let status = response.status().as_u16();
    (status, response.text().await.unwrap())
}

#[tokio::test]
async fn binds_loopback_and_reports_where_a_redirect_should_go() {
    let subject = listener();
    let handle = subject.begin("github", 60_000).await.unwrap();

    assert!(handle.redirect_url.starts_with("http://127.0.0.1:"));
    assert!(handle.redirect_url.ends_with(CALLBACK_PATH));
    assert_eq!(subject.redirect_url().await, handle.redirect_url);
    assert_eq!(handle.state.len(), 24);
    assert!(handle.state.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(subject.pending().await, 1);
    handle.cancel("done").await;
    assert_eq!(subject.pending().await, 0);
    assert_eq!(DEFAULT_CALLBACK_PORT, 33_418);
}

#[tokio::test]
async fn hands_the_code_to_the_authorization_that_minted_the_state() {
    let subject = listener();
    let handle = subject.begin("github", 60_000).await.unwrap();

    let (status, body) = visit(&format!(
        "{}?code=the-code&state={}",
        handle.redirect_url, handle.state
    ))
    .await;
    assert_eq!(status, 200);
    assert!(body.contains("github"));
    assert_eq!(handle.code().await.unwrap(), "the-code");
}

#[tokio::test]
async fn routes_two_outstanding_authorizations_by_their_own_state() {
    let subject = listener();
    let first = subject.begin("github", 60_000).await.unwrap();
    let second = subject.begin("linear", 60_000).await.unwrap();
    assert_eq!(first.redirect_url, second.redirect_url);

    visit(&format!(
        "{}?code=second&state={}",
        second.redirect_url, second.state
    ))
    .await;
    assert_eq!(second.code().await.unwrap(), "second");

    visit(&format!(
        "{}?code=first&state={}",
        first.redirect_url, first.state
    ))
    .await;
    assert_eq!(first.code().await.unwrap(), "first");
}

#[tokio::test]
async fn refuses_a_state_it_is_not_waiting_for() {
    let subject = listener();
    let handle = subject.begin("github", 60_000).await.unwrap();

    let (status, _) = visit(&format!("{}?code=x&state=guessed", handle.redirect_url)).await;
    assert_eq!(status, 400);
    handle.cancel("done").await;
}

#[tokio::test]
async fn consumes_a_state_once() {
    let subject = listener();
    let handle = subject.begin("github", 60_000).await.unwrap();
    visit(&format!(
        "{}?code=one&state={}",
        handle.redirect_url, handle.state
    ))
    .await;
    assert_eq!(handle.code().await.unwrap(), "one");

    // The listener stops with the last outstanding authorization, so a replay
    // has nothing to reach — which is the same answer as an unknown state.
    let replay = reqwest::get(format!(
        "{}?code=two&state={}",
        handle.redirect_url, handle.state
    ))
    .await;
    assert!(replay.map_or(true, |r| r.status().as_u16() != 200));
    // And the code cannot be taken twice.
    assert_eq!(handle.code().await.unwrap_err().kind, ErrorKind::Conflict);
}

#[tokio::test]
async fn reports_a_refusal_from_the_authorization_server() {
    let subject = listener();
    let handle = subject.begin("github", 60_000).await.unwrap();

    let (status, body) = visit(&format!(
        "{}?error=access_denied&error_description=nope&state={}",
        handle.redirect_url, handle.state
    ))
    .await;
    assert_eq!(status, 200);
    assert!(body.contains("nope"));
    let error = handle.code().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::PermissionDenied);
    assert!(error.message.contains("refused"));
}

#[tokio::test]
async fn refuses_a_redirect_that_carried_no_code() {
    let subject = listener();
    let handle = subject.begin("github", 60_000).await.unwrap();

    let (status, _) = visit(&format!("{}?state={}", handle.redirect_url, handle.state)).await;
    assert_eq!(status, 400);
    let error = handle.code().await.unwrap_err();
    assert!(error.message.contains("no code"));
}

#[tokio::test]
async fn answers_404_for_anything_but_the_callback_path() {
    let subject = listener();
    let handle = subject.begin("github", 60_000).await.unwrap();
    let base = handle.redirect_url.replace(CALLBACK_PATH, "");

    assert_eq!(visit(&format!("{base}/")).await.0, 404);
    handle.cancel("done").await;
}

#[tokio::test(start_paused = true)]
async fn gives_up_on_the_timer_rather_than_waiting_for_real_time() {
    let subject = listener();
    let handle = subject.begin("github", 30_000).await.unwrap();
    let mut code = handle.code();

    tokio::time::advance(Duration::from_millis(29_999)).await;
    tokio::task::yield_now().await;
    assert!((&mut code).now_or_never().is_none());

    tokio::time::advance(Duration::from_millis(1)).await;
    let error = code.await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert!(error.message.contains("not completed in time"));
}

#[tokio::test(start_paused = true)]
async fn bounds_an_authorization_that_asked_for_no_timeout() {
    // `0` is the schema's no-limit convention, and an authorization that can
    // never expire holds a listener open forever.
    let subject = listener();
    let handle = subject.begin("github", 0).await.unwrap();
    tokio::time::advance(Duration::from_mins(5)).await;
    let error = handle.code().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
}

#[tokio::test]
async fn stops_listening_once_nothing_is_outstanding() {
    let subject = listener();
    let handle = subject.begin("github", 60_000).await.unwrap();
    let url = handle.redirect_url.clone();
    handle.cancel("done").await;
    assert_eq!(handle.code().await.unwrap_err().kind, ErrorKind::Aborted);

    // An open port nobody is using is a surface with no purpose.
    assert!(reqwest::get(url).await.is_err());
    assert_eq!(subject.redirect_url().await, "");
}

#[tokio::test]
async fn refuses_everything_outstanding_when_it_closes() {
    let subject = listener();
    let handle = subject.begin("github", 60_000).await.unwrap();
    subject.close().await;
    let error = handle.code().await.unwrap_err();
    assert!(error.message.contains("shutting down"));
    // Closing twice is harmless.
    subject.close().await;
}

#[tokio::test]
async fn falls_back_to_an_ephemeral_port_when_the_fixed_one_is_taken() {
    let occupant = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = occupant.local_addr().unwrap().port();
    let subject = CallbackListener::new(CallbackListenerOptions {
        random: Arc::new(OsRandom),
        port: Some(port),
    });
    let handle = subject.begin("github", 60_000).await.unwrap();
    assert!(!handle.redirect_url.contains(&format!(":{port}/")));
    handle.cancel("done").await;
}
