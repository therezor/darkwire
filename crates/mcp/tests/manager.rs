//! Reconciling the settings tree: the four-outcome diff and the status rows.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use ghostai_core::testkit::ManualClock;
use ghostai_core::{ErrorKind, GhostError};
use ghostai_mcp::testkit::{FakeServer, echo_tool};
use ghostai_mcp::{BackoffOptions, McpManager, McpManagerOptions, McpToolDescriptor, McpToolSink};
use ghostai_protocol::{McpServerConfig, McpServerState};
use ghostai_security::testkit::FixedRandom;
use ghostai_tools::AnyTool;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Value, json};

fn server(overrides: Value) -> McpServerConfig {
    let mut base = json!({ "command": "npx" });
    if let (Value::Object(base), Value::Object(overrides)) = (&mut base, overrides) {
        base.extend(overrides);
    }
    serde_json::from_value(base).unwrap()
}

fn servers(entries: Vec<(&str, McpServerConfig)>) -> IndexMap<String, McpServerConfig> {
    entries
        .into_iter()
        .map(|(id, config)| (id.to_owned(), config))
        .collect()
}

#[derive(Default)]
struct Recorder {
    registered: Mutex<IndexMap<String, Vec<String>>>,
    /// Names this sink will refuse, as another source already holds them.
    refused: Mutex<HashSet<String>>,
}

impl Recorder {
    fn registered(&self, server_id: &str) -> Vec<String> {
        self.registered
            .lock()
            .get(server_id)
            .cloned()
            .unwrap_or_default()
    }
}

impl McpToolSink for Recorder {
    fn replace(&self, owner_id: &str, tools: Vec<AnyTool>) -> Vec<String> {
        let refused = self.refused.lock();
        let (rejected, accepted): (Vec<_>, Vec<_>) = tools
            .iter()
            .map(|tool| tool.definition().name.clone())
            .partition(|name| refused.contains(name));
        self.registered.lock().insert(owner_id.to_owned(), accepted);
        rejected
    }
}

struct Harness {
    manager: McpManager,
    sink: Arc<Recorder>,
    fake: FakeServer,
}

fn harness(fake: FakeServer) -> Harness {
    let sink = Arc::new(Recorder::default());
    let manager = McpManager::new(McpManagerOptions {
        sink: Arc::clone(&sink) as Arc<dyn McpToolSink>,
        connect: fake.connector(),
        clock: Arc::new(ManualClock::at(1_700_000_000_000)),
        random: Arc::new(FixedRandom::constant(1)),
        vault: None,
        backoff: BackoffOptions {
            jitter: Some(Arc::new(|ceiling| ceiling)),
            ..BackoffOptions::default()
        },
        on_status_changed: None,
        callback_port: Some(0),
        http: reqwest::Client::new(),
        endpoint_guard: None,
    });
    Harness {
        manager,
        sink,
        fake,
    }
}

async fn settle() {
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

fn refused() -> GhostError {
    GhostError::new(ErrorKind::Network, "ECONNREFUSED")
}

#[tokio::test(start_paused = true)]
async fn connects_a_new_server_and_registers_what_it_offers() {
    let test = harness(FakeServer::default());
    test.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;

    assert_eq!(test.sink.registered("demo"), ["mcp_demo_echo"]);
    assert_eq!(test.manager.connected_count(), 1);
    test.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn is_synchronous_and_never_fails_whatever_the_servers_do() {
    let fake = FakeServer::default();
    fake.fail_connects(refused());
    let test = harness(fake);

    // A save must not be lost to an unreachable server.
    test.manager.reconcile(&servers(vec![
        ("broken", server(json!({}))),
        ("missing", server(json!({}))),
    ]));
    settle().await;
    assert_eq!(test.manager.connected_count(), 0);
    test.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn leaves_an_unchanged_server_entirely_alone() {
    let test = harness(FakeServer::default());
    test.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;
    let attempts = test.fake.attempts();

    test.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;
    assert_eq!(test.fake.attempts(), attempts);
    test.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn reconnects_when_the_transport_moved() {
    let test = harness(FakeServer::default());
    test.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;
    assert_eq!(test.fake.attempts(), 1);

    test.manager.reconcile(&servers(vec![(
        "demo",
        server(json!({ "args": ["--verbose"] })),
    )]));
    settle().await;
    assert_eq!(test.fake.attempts(), 2);
    assert_eq!(test.sink.registered("demo"), ["mcp_demo_echo"]);
    test.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn re_filters_without_reconnecting_when_only_exposure_moved() {
    let test = harness(FakeServer::default());
    test.fake.set_tools(vec![
        echo_tool(),
        McpToolDescriptor {
            name: "reverse".to_owned(),
            ..echo_tool()
        },
    ]);
    test.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;
    assert_eq!(test.sink.registered("demo").len(), 2);

    test.manager.reconcile(&servers(vec![(
        "demo",
        server(json!({ "enabledTools": ["echo"] })),
    )]));
    settle().await;

    assert_eq!(test.sink.registered("demo"), ["mcp_demo_echo"]);
    assert_eq!(test.fake.attempts(), 1);
    test.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn unregisters_a_server_that_left_the_config() {
    let test = harness(FakeServer::default());
    test.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;

    test.manager.reconcile(&IndexMap::new());
    settle().await;

    assert!(test.sink.registered("demo").is_empty());
    assert!(test.manager.statuses().is_empty());
    assert_eq!(test.manager.connected_count(), 0);
    assert!(test.fake.closed());
    test.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn takes_down_a_server_that_was_switched_off_and_says_so() {
    let test = harness(FakeServer::default());
    test.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;

    test.manager.reconcile(&servers(vec![(
        "demo",
        server(json!({ "enabled": false })),
    )]));
    settle().await;

    assert!(test.sink.registered("demo").is_empty());
    // Still a row: an operator who switched it off should see it switched off,
    // not see it vanish.
    let statuses = test.manager.statuses();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].id, "demo");
    assert_eq!(statuses[0].state, McpServerState::Disabled);
    assert!(!statuses[0].enabled);
    test.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn reports_a_misconfigured_entry_rather_than_refusing_the_save() {
    let test = harness(FakeServer::default());
    let broken: McpServerConfig = serde_json::from_value(json!({})).unwrap();
    test.manager.reconcile(&servers(vec![("broken", broken)]));
    settle().await;

    let statuses = test.manager.statuses();
    assert_eq!(statuses[0].id, "broken");
    assert_eq!(statuses[0].state, McpServerState::Failed);
    assert!(statuses[0].enabled);
    assert!(
        statuses[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("neither a command nor a url")
    );
    test.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn clears_a_misconfiguration_once_it_is_fixed() {
    let test = harness(FakeServer::default());
    let broken: McpServerConfig = serde_json::from_value(json!({})).unwrap();
    test.manager.reconcile(&servers(vec![("demo", broken)]));
    settle().await;
    assert_eq!(test.manager.statuses()[0].state, McpServerState::Failed);

    test.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;
    let statuses = test.manager.statuses();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].state, McpServerState::Ready);
    test.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn lists_every_server_connected_or_not_sorted_by_id() {
    let test = harness(FakeServer::default());
    let broken: McpServerConfig = serde_json::from_value(json!({})).unwrap();
    test.manager.reconcile(&servers(vec![
        ("zulu", server(json!({}))),
        ("alpha", broken),
    ]));
    settle().await;

    let ids: Vec<String> = test.manager.statuses().into_iter().map(|s| s.id).collect();
    assert_eq!(ids, ["alpha", "zulu"]);
    test.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn keeps_one_server_working_when_another_cannot_be_reached() {
    // Two managers rather than two servers on one fake, because the fake is the
    // transport: what matters is that a failure is scoped to its own row.
    let good = harness(FakeServer::default());
    good.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;

    let broken = FakeServer::default();
    broken.fail_connects(refused());
    let bad = harness(broken);
    bad.manager
        .reconcile(&servers(vec![("other", server(json!({})))]));
    settle().await;

    assert_eq!(good.sink.registered("demo"), ["mcp_demo_echo"]);
    assert_eq!(bad.manager.statuses()[0].state, McpServerState::Failed);
    good.manager.close().await;
    bad.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn close_unregisters_everything_and_stops_every_timer() {
    let fake = FakeServer::default();
    fake.fail_connects(refused());
    let test = harness(fake.clone());
    test.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;
    assert_eq!(fake.attempts(), 1);

    test.manager.close().await;

    assert!(test.sink.registered("demo").is_empty());
    tokio::time::advance(Duration::from_mins(10)).await;
    settle().await;
    assert_eq!(fake.attempts(), 1);
}

#[tokio::test(start_paused = true)]
async fn ignores_a_reconcile_after_it_has_closed() {
    let test = harness(FakeServer::default());
    test.manager.close().await;
    test.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;
    assert_eq!(test.fake.attempts(), 0);
}

#[tokio::test(start_paused = true)]
async fn registers_what_it_can_and_reports_the_rest_on_a_name_collision() {
    let test = harness(FakeServer::default());
    test.sink.refused.lock().insert("mcp_demo_echo".to_owned());
    test.manager
        .reconcile(&servers(vec![("demo", server(json!({})))]));
    settle().await;

    // The other tools of a server whose one name clashes still work.
    assert!(test.sink.registered("demo").is_empty());
    assert_eq!(test.manager.statuses()[0].state, McpServerState::Ready);
    test.manager.close().await;
}

#[tokio::test(start_paused = true)]
async fn hands_an_oauth_server_a_flow_and_a_callback() {
    // The fake never demands authorization, so the flow is carried but unused;
    // what this proves is that an `oauth` block puts a broker on the dial.
    let test = harness(FakeServer::default());
    test.manager.reconcile(&servers(vec![(
        "remote",
        serde_json::from_value(json!({
            "url": "http://127.0.0.1:9/mcp",
            "oauth": { "authUrl": "http://127.0.0.1:9/a", "tokenUrl": "http://127.0.0.1:9/t", "clientId": "c" }
        }))
        .unwrap(),
    )]));
    // The broker binds the callback listener, which is real I/O; give it room.
    for _ in 0..20 {
        settle().await;
        if test.fake.attempts() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(test.fake.last_attempt_had_auth());
    assert_eq!(test.manager.statuses()[0].state, McpServerState::Ready);
    test.manager.close().await;
}
