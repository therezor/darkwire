//! One server: connect, republish, back off, re-bridge, close.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::time::Duration;

use darkwire_core::testkit::ManualClock;
use darkwire_core::{ErrorKind, WireError};
use darkwire_mcp::testkit::{FakeServer, echo_tool};
use darkwire_mcp::{
    BackoffOptions, McpConnection, McpConnectionOptions, McpConnectionSpec, McpToolDescriptor,
    resolve_spec,
};
use darkwire_protocol::{McpServerConfig, McpServerState};
use darkwire_security::testkit::FixedRandom;
use darkwire_tools::AnyTool;
use darkwire_tools::testkit::TestWorkspace;
use parking_lot::Mutex;
use serde_json::{Value, json};

fn spec_for(overrides: Value) -> McpConnectionSpec {
    let mut base = json!({ "command": "npx" });
    if let (Value::Object(base), Value::Object(overrides)) = (&mut base, overrides) {
        base.extend(overrides);
    }
    let config: McpServerConfig = serde_json::from_value(base).unwrap();
    resolve_spec("demo", &config).unwrap()
}

struct Harness {
    connection: McpConnection,
    server: FakeServer,
    clock: Arc<ManualClock>,
    published: Arc<Mutex<Vec<Vec<String>>>>,
    status_changes: Arc<Mutex<usize>>,
}

impl Harness {
    fn new(spec: Option<McpConnectionSpec>, server: Option<FakeServer>) -> Harness {
        let clock = Arc::new(ManualClock::at(1_700_000_000_000));
        let server = server.unwrap_or_default();
        let published: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let status_changes = Arc::new(Mutex::new(0usize));
        let recorder = Arc::clone(&published);
        let counter = Arc::clone(&status_changes);
        let connection = McpConnection::new(McpConnectionOptions {
            spec: spec.unwrap_or_else(|| spec_for(json!({}))),
            connect: server.connector(),
            publish: Arc::new(move |_: &str, tools: Vec<AnyTool>| {
                recorder
                    .lock()
                    .push(tools.iter().map(|t| t.definition().name.clone()).collect());
            }),
            on_status_changed: Some(Arc::new(move || *counter.lock() += 1)),
            authorization: None,
            clock: Arc::clone(&clock) as Arc<dyn darkwire_core::Clock>,
            random: Arc::new(FixedRandom::constant(0)),
            // No jitter, so the cadence is the thing under test rather than a
            // distribution over it.
            backoff: BackoffOptions {
                jitter: Some(Arc::new(|ceiling| ceiling)),
                ..BackoffOptions::default()
            },
        });
        Harness {
            connection,
            server,
            clock,
            published,
            status_changes,
        }
    }

    /// The names registered right now.
    fn current(&self) -> Vec<String> {
        self.published.lock().last().cloned().unwrap_or_default()
    }
}

/// Lets every ready task run: dial, list, republish, publish.
async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

async fn advance(ms: u64) {
    tokio::time::advance(Duration::from_millis(ms)).await;
    settle().await;
}

fn refused() -> WireError {
    WireError::new(ErrorKind::Network, "ECONNREFUSED")
}

#[tokio::test(start_paused = true)]
async fn registers_the_server_tools_it_can_reach() {
    let test = Harness::new(None, None);
    test.connection.start();
    settle().await;

    assert_eq!(test.connection.state(), McpServerState::Ready);
    assert_eq!(test.current(), ["mcp_demo_echo"]);
    let status = test.connection.status();
    assert_eq!(status.server_name, "fake-demo");
    assert_eq!(status.server_version, "1.0.0");
    assert_eq!(status.tools, ["mcp_demo_echo"]);
    assert!(*test.status_changes.lock() > 0);
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn records_when_it_last_connected_for_the_status_row() {
    let test = Harness::new(None, None);
    test.connection.start();
    settle().await;
    assert_eq!(
        test.connection.status().last_connected_at_ms,
        Some(u64::try_from(darkwire_core::Clock::now_ms(&*test.clock)).unwrap())
    );
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn re_lists_when_the_server_says_its_tools_moved() {
    let test = Harness::new(None, None);
    test.connection.start();
    settle().await;

    test.server.set_tools(vec![
        echo_tool(),
        McpToolDescriptor {
            name: "reverse".to_owned(),
            ..echo_tool()
        },
    ]);
    settle().await;

    assert_eq!(test.current(), ["mcp_demo_echo", "mcp_demo_reverse"]);
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn unregisters_its_tools_the_moment_the_server_goes_away() {
    let test = Harness::new(None, None);
    test.connection.start();
    settle().await;
    assert_eq!(test.current(), ["mcp_demo_echo"]);

    test.server.drop_session(None);
    settle().await;

    // A turn starting now must not be offered a tool nothing can answer.
    assert!(test.current().is_empty());
    assert_eq!(test.connection.state(), McpServerState::Failed);
    assert!(
        test.connection
            .status()
            .last_error
            .unwrap()
            .contains("closed the connection")
    );
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn retries_on_a_widening_backoff_and_comes_back_on_its_own() {
    let server = FakeServer::default();
    server.fail_connects(refused());
    let test = Harness::new(None, Some(server.clone()));

    test.connection.start();
    settle().await;
    assert_eq!(test.connection.state(), McpServerState::Failed);
    assert_eq!(server.attempts(), 1);

    // Nothing before the first delay is due.
    advance(999).await;
    assert_eq!(server.attempts(), 1);
    advance(1).await;
    assert_eq!(server.attempts(), 2);

    // Doubling: the second wait is 2 s, the third 4 s.
    advance(1_999).await;
    assert_eq!(server.attempts(), 2);
    advance(1).await;
    assert_eq!(server.attempts(), 3);

    server.recover();
    advance(4_000).await;
    assert_eq!(test.connection.state(), McpServerState::Ready);
    assert_eq!(test.current(), ["mcp_demo_echo"]);
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn never_waits_longer_than_the_ceiling() {
    let server = FakeServer::default();
    server.fail_connects(refused());
    let test = Harness::new(None, Some(server.clone()));
    test.connection.start();
    settle().await;

    for _ in 0..12 {
        advance(60_000).await;
    }
    // A laptop that has been asleep for an hour has to come back without an
    // operator, so there is no attempt cap — only a ceiling on the wait.
    assert_eq!(server.attempts(), 13);
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn does_not_retry_a_server_that_needs_an_operator_to_authorize() {
    let server = FakeServer::default();
    server.fail_connects(
        WireError::new(ErrorKind::PermissionDenied, "authorize me")
            .with_detail("needsAuthorization", true),
    );
    let test = Harness::new(None, Some(server.clone()));

    test.connection.start();
    settle().await;
    assert_eq!(test.connection.state(), McpServerState::NeedsAuthorization);

    // Looping on a redirect nobody is going to follow spends the authorization
    // server's rate limit to reach the same answer.
    advance(600_000).await;
    assert_eq!(server.attempts(), 1);
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn leaves_no_timer_armed_after_close() {
    let server = FakeServer::default();
    server.fail_connects(refused());
    let test = Harness::new(None, Some(server.clone()));
    test.connection.start();
    settle().await;

    test.connection.close().await;
    assert!(test.current().is_empty());
    assert_eq!(test.connection.state(), McpServerState::Disabled);

    // A timer that had already been armed cannot resurrect it.
    advance(600_000).await;
    assert_eq!(server.attempts(), 1);
    // And neither can a start after close.
    test.connection.start();
    settle().await;
    assert_eq!(server.attempts(), 1);
}

#[tokio::test(start_paused = true)]
async fn does_not_read_its_own_close_as_a_drop() {
    let test = Harness::new(None, None);
    test.connection.start();
    settle().await;

    test.connection.close().await;
    settle().await;
    assert!(test.server.closed());
    assert_eq!(test.connection.state(), McpServerState::Disabled);
    advance(600_000).await;
    assert_eq!(test.server.attempts(), 1);
}

#[tokio::test(start_paused = true)]
async fn re_filters_without_reconnecting_when_only_exposure_changed() {
    let test = Harness::new(None, None);
    test.connection.start();
    settle().await;
    test.server.set_tools(vec![
        echo_tool(),
        McpToolDescriptor {
            name: "reverse".to_owned(),
            ..echo_tool()
        },
    ]);
    settle().await;
    assert_eq!(test.current().len(), 2);

    let attempts_before = test.server.attempts();
    test.connection
        .rebridge(spec_for(json!({ "enabledTools": ["echo"] })))
        .unwrap();

    assert_eq!(test.current(), ["mcp_demo_echo"]);
    assert_eq!(test.connection.status().filtered_tools, ["reverse"]);
    // The whole point of the two fingerprints: no subprocess was bounced.
    assert_eq!(test.server.attempts(), attempts_before);

    // The same exposure again is a no-op.
    let publishes = test.published.lock().len();
    test.connection
        .rebridge(spec_for(json!({ "enabledTools": ["echo"] })))
        .unwrap();
    assert_eq!(test.published.lock().len(), publishes);
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn refuses_a_rebridge_that_would_need_a_new_connection() {
    let test = Harness::new(None, None);
    let error = test
        .connection
        .rebridge(spec_for(json!({ "command": "other" })))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.contains("new connection"));
}

#[tokio::test(start_paused = true)]
async fn warns_about_an_enabled_tools_entry_that_matches_nothing() {
    let test = Harness::new(Some(spec_for(json!({ "enabledTools": ["nope"] }))), None);
    test.connection.start();
    settle().await;

    assert!(test.current().is_empty());
    assert!(
        test.connection
            .status()
            .warnings
            .join(" ")
            .contains("\"nope\"")
    );
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn warns_about_a_collision_and_a_sloppy_schema_without_dropping_the_server() {
    let server = FakeServer::new(vec![
        McpToolDescriptor {
            name: "open file".to_owned(),
            ..echo_tool()
        },
        McpToolDescriptor {
            name: "open_file".to_owned(),
            ..echo_tool()
        },
        McpToolDescriptor {
            name: "broken".to_owned(),
            input_schema: json!({ "type": "string" }),
            ..echo_tool()
        },
    ]);
    let test = Harness::new(None, Some(server));
    test.connection.start();
    settle().await;

    assert_eq!(
        test.current(),
        ["mcp_demo_open-file", "mcp_demo_open-file_2"]
    );
    let warnings = test.connection.status().warnings.join("\n");
    assert!(warnings.contains("renamed"));
    assert!(warnings.contains("broken: Tool broken must take an object"));
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn refuses_a_call_once_the_server_has_gone_rather_than_hanging() {
    let test = Harness::new(None, None);
    test.connection.start();
    settle().await;
    let tool = test.connection.tools().into_iter().next().expect("a tool");

    test.server
        .drop_session(Some(WireError::new(ErrorKind::Network, "reset")));
    settle().await;
    assert_eq!(
        test.connection.status().last_error.as_deref(),
        Some("reset")
    );

    let workspace = TestWorkspace::new();
    let execution = tool
        .execute(json!({ "text": "hi" }), workspace.context())
        .await;
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::Network));
    assert!(execution.content.contains("not connected"));
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_bridged_call_reaches_the_current_session() {
    let test = Harness::new(None, None);
    test.connection.start();
    settle().await;
    let tool = test.connection.tools().into_iter().next().expect("a tool");
    let workspace = TestWorkspace::new();
    let execution = tool
        .execute(json!({ "text": "hi" }), workspace.context())
        .await;
    assert!(!execution.is_error);
    assert_eq!(test.server.calls()[0].name, "echo");
    test.connection.close().await;
}

#[tokio::test(start_paused = true)]
async fn reports_an_authorization_url_as_a_state() {
    let test = Harness::new(None, None);
    test.connection
        .report_authorization_url("https://auth.test/authorize?x=1");
    let status = test.connection.status();
    assert_eq!(status.state, McpServerState::NeedsAuthorization);
    assert_eq!(
        status.authorization_url.as_deref(),
        Some("https://auth.test/authorize?x=1")
    );
    assert_eq!(test.connection.server_id(), "demo");
    assert_eq!(test.connection.spec().server_id, "demo");
}

#[tokio::test(start_paused = true)]
async fn a_one_shot_failure_is_followed_by_normal_service() {
    let server = FakeServer::default();
    server.fail_next_connect(refused());
    let test = Harness::new(None, Some(server.clone()));
    test.connection.start();
    settle().await;
    assert_eq!(test.connection.state(), McpServerState::Failed);
    advance(1_000).await;
    assert_eq!(test.connection.state(), McpServerState::Ready);
    assert_eq!(server.attempts(), 2);
    test.connection.close().await;
}
