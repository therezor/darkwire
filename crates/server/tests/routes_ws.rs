//! The socket, over a real socket.
//!
//! An in-process router cannot upgrade a connection, so this is the one route
//! whose tests have to bind a port and speak WebSocket to it. What is being
//! checked is the *binding* rather than the hub — queueing, replay and fanout
//! belong to the hub's own tests — so: that the upgrade is authenticated, that
//! frames go both ways, that a session named in the query is the session that
//! runs, and that a request without the upgrade headers gets an answer a human
//! can read.
//!
//! Every assertion here is on a durable state — a frame that arrived, a session
//! the hub still holds — rather than on a moment the machine happened to be
//! slow enough to show.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a harness that cannot stand up is a failing test either way"
)]

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use darkwire_server::testkit::{TestServer, TestServerOptions, start_test_server};
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::ClientRequestBuilder;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tower::ServiceExt as _;

/// How long a frame has to arrive before the test gives up on it.
///
/// Generous on purpose: this is a "did it ever happen" bound, not a timing
/// assertion, and a loaded machine is allowed to be slow.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// A listening server, and the address it actually bound.
struct Listening {
    test: TestServer,
    address: SocketAddr,
    /// Dropping this stops the listener.
    _task: tokio::task::JoinHandle<()>,
}

async fn listening(options: TestServerOptions) -> Listening {
    let test = start_test_server(options).expect("a test server");
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .expect("a bound listener");
    let address = listener.local_addr().expect("the bound address");
    let router = test.router.clone();
    // With connect info, because the throttle and the rate limiter both key on
    // the peer address and a listener that did not supply one would exercise a
    // different branch than production.
    let task = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    Listening {
        test,
        address,
        _task: task,
    }
}

/// An open socket, and the frames that have arrived on it.
struct Connection {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    seen: Vec<Value>,
}

impl Connection {
    /// Waits for a frame of this type, remembering everything that arrives on
    /// the way.
    async fn next_of(&mut self, tag: &str) -> Value {
        if let Some(found) = self.seen.iter().find(|frame| frame["type"] == tag) {
            return found.clone();
        }
        let deadline = tokio::time::Instant::now() + FRAME_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let message = tokio::time::timeout(remaining, self.socket.next())
                .await
                .unwrap_or_else(|_| panic!("no {tag} frame arrived"))
                .unwrap_or_else(|| panic!("the socket closed before a {tag} frame"))
                .expect("a readable frame");
            if let Message::Text(text) = message {
                let frame: Value = serde_json::from_str(&text).expect("a JSON frame");
                let matched = frame["type"] == tag;
                self.seen.push(frame.clone());
                if matched {
                    return frame;
                }
            }
        }
    }

    async fn send(&mut self, frame: &Value) {
        self.socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .expect("the frame was written");
    }

    async fn close(mut self) {
        let _ = self.socket.close(None).await;
    }
}

/// Opens a socket, with a credential unless `token` is `None`.
async fn connect(
    server: &Listening,
    query: &str,
    token: Option<&str>,
) -> Result<Connection, WsError> {
    connect_from(server, query, token, None).await
}

/// Opens a socket the way a browser does, naming the page it came from.
async fn connect_from(
    server: &Listening,
    query: &str,
    token: Option<&str>,
    origin: Option<&str>,
) -> Result<Connection, WsError> {
    let uri = format!("ws://{}/ws{query}", server.address)
        .parse()
        .expect("a well-formed URL");
    let mut builder = ClientRequestBuilder::new(uri);
    if let Some(token) = token {
        builder = builder.with_header("Authorization", format!("Bearer {token}"));
    }
    if let Some(origin) = origin {
        builder = builder.with_header("Origin", origin);
    }
    let (socket, _) = tokio_tungstenite::connect_async(builder).await?;
    Ok(Connection {
        socket,
        seen: Vec::new(),
    })
}

/// The status a failed handshake reported, or `None` if it failed some other
/// way.
fn handshake_status(error: &WsError) -> Option<StatusCode> {
    match error {
        WsError::Http(response) => Some(response.status()),
        _ => None,
    }
}

// Before the upgrade

#[tokio::test]
async fn a_plain_get_is_told_what_the_endpoint_actually_speaks() {
    let test = start_test_server(TestServerOptions::default()).expect("a test server");
    let request = Request::builder()
        .uri("/ws")
        .header(header::AUTHORIZATION, format!("Bearer {}", test.token))
        .body(Body::empty())
        .expect("a well-formed request");

    let response = test
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answered");

    // 426 rather than 404: a client that forgot the upgrade headers and got a
    // 404 reads it as "wrong URL" and goes looking for a path that does not
    // exist.
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("a body");
    let body: Value = serde_json::from_slice(&bytes).expect("a JSON envelope");
    assert_eq!(body["error"]["code"], "bad_request");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("WebSocket"),
        "{body}"
    );
}

#[tokio::test]
async fn a_plain_get_without_a_credential_is_refused_before_anything_else() {
    let test = start_test_server(TestServerOptions::default()).expect("a test server");
    let request = Request::builder()
        .uri("/ws")
        .body(Body::empty())
        .expect("a well-formed request");

    // The manifest says the socket is `Required`, and the layer runs ahead of
    // the handler: an unauthenticated upgrade is an anonymous, shell-capable
    // agent.
    assert_eq!(
        test.router
            .clone()
            .oneshot(request)
            .await
            .expect("the router answered")
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn an_empty_session_parameter_is_refused_before_the_upgrade() {
    let test = start_test_server(TestServerOptions::default()).expect("a test server");
    let request = Request::builder()
        .uri("/ws?session=")
        .header(header::AUTHORIZATION, format!("Bearer {}", test.token))
        .body(Body::empty())
        .expect("a well-formed request");

    // Otherwise the client gets a socket that opens, mints a conversation it
    // did not ask for, and looks to the user like it lost the one they were in.
    assert_eq!(
        test.router
            .clone()
            .oneshot(request)
            .await
            .expect("the router answered")
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn an_empty_agent_parameter_is_refused_too() {
    let test = start_test_server(TestServerOptions::default()).expect("a test server");
    let request = Request::builder()
        .uri("/ws?agent=")
        .header(header::AUTHORIZATION, format!("Bearer {}", test.token))
        .body(Body::empty())
        .expect("a well-formed request");

    assert_eq!(
        test.router
            .clone()
            .oneshot(request)
            .await
            .expect("the router answered")
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

// Over a real socket

#[tokio::test]
async fn an_upgrade_without_a_credential_is_refused() {
    let server = listening(TestServerOptions::default()).await;

    let error = connect(&server, "", None)
        .await
        .err()
        .expect("the handshake was refused");
    assert_eq!(handshake_status(&error), Some(StatusCode::UNAUTHORIZED));
}

#[tokio::test]
async fn an_authenticated_socket_is_greeted_with_the_protocol_version() {
    let server = listening(TestServerOptions::default()).await;
    let token = server.test.token.clone();

    let mut connection = connect(&server, "", Some(&token))
        .await
        .expect("the handshake completed");
    let greeting = connection.next_of("connected").await;

    // The greeting carries `last_seq` so a fresh client knows where the session
    // is before it has seen anything.
    assert_eq!(greeting["protocolVersion"], 2);
    assert_eq!(greeting["lastSeq"], 0);
    assert!(
        greeting["sessionKey"]
            .as_str()
            .is_some_and(|key| !key.is_empty())
    );
    connection.close().await;
}

#[tokio::test]
async fn a_socket_opens_on_the_session_the_query_names_and_runs_a_turn_on_it() {
    let server = listening(TestServerOptions {
        answers: vec!["the answer".to_owned()],
        ..TestServerOptions::default()
    })
    .await;
    let token = server.test.token.clone();

    let mut connection = connect(&server, "?session=web:42", Some(&token))
        .await
        .expect("the handshake completed");
    assert_eq!(
        connection.next_of("connected").await["sessionKey"],
        "web:42"
    );

    connection
        .send(&json!({
            "type": "user.message",
            "sessionKey": "web:42",
            "content": "a question",
        }))
        .await;

    let delta = connection.next_of("assistant.delta").await;
    // The durable end of the turn, not a moment in the middle of it.
    connection.next_of("turn.end").await;

    assert_eq!(delta["text"], "the answer");
    assert_eq!(server.test.runner.inputs(), ["web:42"]);
    connection.close().await;
}

#[tokio::test]
async fn a_malformed_frame_is_answered_rather_than_dropping_the_connection() {
    let server = listening(TestServerOptions::default()).await;
    let token = server.test.token.clone();

    let mut connection = connect(&server, "", Some(&token))
        .await
        .expect("the handshake completed");
    connection.next_of("connected").await;
    connection
        .socket
        .send(Message::Text("{not json".into()))
        .await
        .expect("the frame was written");

    let error = connection.next_of("error").await;
    assert_eq!(error["code"], "bad_request");

    // Still open: nothing a client controls can take the socket down, so a
    // round trip after the bad frame still works.
    connection.send(&json!({ "type": "ping" })).await;
    connection.next_of("pong").await;
    connection.close().await;
}

#[tokio::test]
async fn a_binary_frame_is_decoded_like_a_text_one() {
    let server = listening(TestServerOptions::default()).await;
    let token = server.test.token.clone();

    let mut connection = connect(&server, "?session=web:bin", Some(&token))
        .await
        .expect("the handshake completed");
    connection.next_of("connected").await;

    connection
        .socket
        .send(Message::Binary(
            json!({ "type": "ping" }).to_string().into_bytes().into(),
        ))
        .await
        .expect("the frame was written");

    connection.next_of("pong").await;
    connection.close().await;
}

#[tokio::test]
async fn a_closed_socket_detaches_and_leaves_the_session_for_the_next_tab() {
    let server = listening(TestServerOptions::default()).await;
    let token = server.test.token.clone();

    let mut connection = connect(&server, "?session=web:1", Some(&token))
        .await
        .expect("the handshake completed");
    connection.next_of("connected").await;
    connection.close().await;

    // The state survives the connection: that is what a replay buffer is for.
    // Polled rather than slept on, so the assertion is about the state settling
    // rather than about how long the machine took to get there.
    let deadline = tokio::time::Instant::now() + FRAME_TIMEOUT;
    while server.test.hub.watchers("web:1") > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(server.test.hub.watchers("web:1"), 0);
    assert_eq!(server.test.hub.session_count(), 1);
    assert!(!server.test.hub.busy("web:1"));
}

#[tokio::test]
async fn two_tabs_on_one_session_both_see_it() {
    let server = listening(TestServerOptions::default()).await;
    let token = server.test.token.clone();

    let mut first = connect(&server, "?session=web:shared", Some(&token))
        .await
        .expect("the handshake completed");
    first.next_of("connected").await;
    let mut second = connect(&server, "?session=web:shared", Some(&token))
        .await
        .expect("the handshake completed");
    second.next_of("connected").await;

    // One session, two connections — which is the arrangement the replay ring
    // and the fanout exist for.
    assert_eq!(server.test.hub.session_count(), 1);
    assert_eq!(server.test.hub.watchers("web:shared"), 2);

    first.close().await;
    second.close().await;
}

// Where the page came from

#[tokio::test]
async fn a_page_this_server_did_not_serve_cannot_open_the_socket() {
    // Authentication off, on loopback: the one configuration where nothing but
    // this check stands between any web page and the agent.
    let mut config = darkwire_protocol::config::Config::default();
    config.server.auth.enabled = false;
    let server = listening(TestServerOptions {
        config: Some(config),
        ..TestServerOptions::default()
    })
    .await;

    for origin in ["http://evil.example", "null", "http://127.0.0.1:1"] {
        let refused = connect_from(&server, "", None, Some(origin))
            .await
            .err()
            .unwrap_or_else(|| panic!("{origin} was let in"));
        assert_eq!(
            handshake_status(&refused),
            Some(StatusCode::FORBIDDEN),
            "{origin}"
        );
    }
}

#[tokio::test]
async fn the_page_this_server_served_opens_the_socket() {
    let server = listening(TestServerOptions::default()).await;
    let token = server.test.token.clone();
    let origin = format!("http://{}", server.address);

    let mut socket = connect_from(&server, "", Some(&token), Some(&origin))
        .await
        .expect("the upgrade was accepted");
    socket.next_of("connected").await;
    socket.close().await;
}
