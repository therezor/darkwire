//! The reconcile engine: what it holds, what it hands out, and what it does
//! when an extension turns out to be less than it claimed.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use crate::common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use darkwire_agent::{RuntimePromptContext, StaticPromptContext};
use darkwire_channels::{ChannelContext, ChannelControl, ChannelInbound};
use darkwire_core::message_bus::{OutboundKind, OutboundMessage, PublishResult};
use darkwire_core::{Database, SystemClock};
use darkwire_extension_host::{ExtensionHost, ExtensionHostOptions, Timings};
use darkwire_protocol::{ExtensionState, ExtensionsConfig};
use darkwire_security::ExtensionStore;
use parking_lot::Mutex;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use common::{Harness, eventually};

#[tokio::test(flavor = "multi_thread")]
async fn a_host_with_nothing_installed_holds_nothing() {
    let temp = TempDir::new().unwrap();
    let extensions = temp.path().join("extensions");
    std::fs::create_dir_all(&extensions).unwrap();
    let store = ExtensionStore::new(
        Database::in_memory().unwrap(),
        &extensions,
        Arc::new(SystemClock),
    )
    .unwrap();
    let host = ExtensionHost::new(ExtensionHostOptions::new(store, temp.path()));

    host.reconcile(&ExtensionsConfig::default());
    assert!(host.quiesce(Duration::from_secs(5)).await);
    assert!(host.status().is_empty());
    assert_eq!(host.loaded_count(), 0);
    assert!(host.tools().is_empty());
    assert!(host.channels().is_empty());
    assert!(host.providers().is_empty());
    assert!(host.contributors().is_empty());
    assert!(host.commands().is_empty());
    assert!(host.pid("nobody").is_none());
    assert!(format!("{host:?}").contains("ExtensionHost"));

    // Stopping a host that never started anything is a no-op, not a panic.
    host.stop().await;
    // And a reconcile after a stop does nothing at all: the host is closed.
    host.reconcile(&ExtensionsConfig::default());
    assert!(host.status().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_listener_fires_once_per_change_and_not_on_a_row_that_stood_still() {
    let temp = TempDir::new().unwrap();
    let extensions = temp.path().join("extensions");
    std::fs::create_dir_all(extensions.join("chatty")).unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/chatty/darkwire.extension.yaml"
        ),
        extensions.join("chatty/darkwire.extension.yaml"),
    )
    .unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/chatty/index.mjs"
        ),
        extensions.join("chatty/index.mjs"),
    )
    .unwrap();

    let store = ExtensionStore::new(
        Database::in_memory().unwrap(),
        &extensions,
        Arc::new(SystemClock),
    )
    .unwrap();
    let fired = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&fired);
    let host = ExtensionHost::new(
        ExtensionHostOptions::new(store, temp.path())
            .with_timings(common::quick())
            .with_listener(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }))
            .with_secrets(Arc::new(|_id| None)),
    );

    let config = ExtensionsConfig::default();
    host.reconcile(&config);
    assert!(host.quiesce(Duration::from_secs(10)).await);
    assert_eq!(host.status()[0].state, ExtensionState::Unapproved);
    let after_first = fired.load(Ordering::SeqCst);
    assert!(after_first >= 1);
    let revision = host.revision();

    // The live-lock this guard exists to stop: a reconcile that announced
    // unconditionally for an unapproved extension woke the composition root,
    // which rebuilt, which reconciled again, forever.
    host.reconcile(&config);
    assert!(host.quiesce(Duration::from_secs(10)).await);
    assert_eq!(fired.load(Ordering::SeqCst), after_first);
    assert_eq!(host.revision(), revision);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_nobody_serves_is_a_refusal_rather_than_an_empty_answer() {
    let harness = Harness::with_example();
    harness.approve("hello");
    harness.settle_default().await;

    let error = harness
        .host
        .run_command("nobody-has-this", "", None, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        error.message.contains("nobody-has-this"),
        "{}",
        error.message
    );
}

/// What a recording channel context saw, in each direction.
type Published = Arc<Mutex<Vec<ChannelInbound>>>;
type Controlled = Arc<Mutex<Vec<ChannelControl>>>;

/// A channel context that records everything the extension pushed through it.
fn recording(id: &str) -> (ChannelContext, Published, Controlled) {
    let published: Published = Arc::new(Mutex::new(Vec::new()));
    let controlled: Controlled = Arc::new(Mutex::new(Vec::new()));
    let into_publish = Arc::clone(&published);
    let into_control = Arc::clone(&controlled);
    let context = ChannelContext {
        id: id.to_owned(),
        settings: serde_json::Map::new(),
        clock: Arc::new(SystemClock),
        token: CancellationToken::new(),
        publish: Arc::new(move |message| {
            into_publish.lock().push(message);
            PublishResult::Accepted {
                id: "m1".to_owned(),
            }
        }),
        control: Arc::new(move |command| {
            into_control.lock().push(command);
        }),
    };
    (context, published, controlled)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channel_runs_over_the_extensions_own_connection_in_both_directions() {
    let harness = Harness::with(&["talkative"]);
    harness.approve("talkative");
    harness.settle_default().await;

    let row = harness.row("talkative");
    assert_eq!(row.state, ExtensionState::Ready);
    // `squatter` is not namespaced to this extension, so it is dropped with a
    // sentence and the two that are namespaced are kept.
    assert_eq!(row.channels, vec!["talkative", "talkative-dm"]);
    assert!(
        row.warnings.iter().any(|w| w.contains("\"squatter\"")),
        "{:?}",
        row.warnings
    );

    let factories = harness.host.channels();
    assert_eq!(factories.len(), 2);
    let factory = factories
        .iter()
        .find(|f| f.id() == "talkative")
        .expect("the channel is registered");

    let (context, published, controlled) = recording("talkative");
    let channel = factory.create(context).expect("the channel builds");
    assert_eq!(channel.id(), "talkative");

    // Starting it reaches the extension, which publishes an inbound message and
    // a control frame straight back — the case the bind-at-build-time order
    // exists for.
    channel.start().await.expect("the channel starts");
    assert!(
        eventually(Duration::from_secs(5), || !published.lock().is_empty()
            && !controlled.lock().is_empty())
        .await,
        "the extension's inbound traffic never arrived"
    );
    assert_eq!(published.lock()[0].session_key, "inbound");
    assert_eq!(published.lock()[0].sender_id, "someone");
    assert_eq!(controlled.lock()[0].frame.tag(), "turn.stop");

    // And an outbound message is rendered by the extension.
    channel
        .send(OutboundMessage {
            id: "o1".to_owned(),
            channel_id: "talkative".to_owned(),
            session_key: "s1".to_owned(),
            target: "t1".to_owned(),
            content: Vec::new(),
            kind: OutboundKind::Notice,
            created_at_ms: 0,
            metadata: serde_json::Map::new(),
        })
        .await
        .expect("the message is rendered");

    channel.stop().await.expect("the channel stops");
}

#[tokio::test(flavor = "multi_thread")]
async fn providers_are_manifest_data_and_an_unknown_wire_is_only_a_warning() {
    let harness = Harness::with(&["talkative"]);
    harness.approve("talkative");
    harness.settle_default().await;

    let providers = harness.host.providers();
    // The one on a wire this build has is registered; the one on a wire it does
    // not have, and the one that is not namespaced, are sentences.
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, "talkative-gpt");
    assert_eq!(
        providers[0].default_api_base.as_deref(),
        Some("https://example.invalid/v1")
    );

    let warnings = harness.row("talkative").warnings.join("\n");
    assert!(warnings.contains("quantum-entangle"), "{warnings}");
    assert!(warnings.contains("not-namespaced"), "{warnings}");
}

#[tokio::test(flavor = "multi_thread")]
async fn both_context_halves_are_fetched_once_a_turn_and_read_per_iteration() {
    let harness = Harness::with(&["talkative"]);
    harness.approve("talkative");
    harness.settle_default().await;

    let contributors = harness.host.contributors();
    assert_eq!(contributors.len(), 1);
    let contributor = &contributors[0];
    assert_eq!(contributor.name(), "talkative");

    let static_context = StaticPromptContext {
        session_key: "s1".to_owned(),
        ..StaticPromptContext::default()
    };
    let runtime_context = RuntimePromptContext {
        static_context: static_context.clone(),
        ..RuntimePromptContext::default()
    };

    // Before the turn's static half runs there is nothing cached, which is what
    // stops a section leaking from one session into another.
    assert_eq!(contributor.runtime_section(&runtime_context), None);

    let section = contributor.static_section(&static_context).await.unwrap();
    assert!(section.contains("## Talkative"), "{section}");
    assert!(section.contains("Present."), "{section}");

    // And the per-iteration half reads what that fetch left behind, without an
    // RPC of its own — it is synchronous and runs five or ten times a turn.
    let live = contributor.runtime_section(&runtime_context).unwrap();
    assert_eq!(live, "Live state.");

    // Keyed by session: another conversation reads its own, not this one's.
    let other = RuntimePromptContext {
        static_context: StaticPromptContext {
            session_key: "s2".to_owned(),
            ..StaticPromptContext::default()
        },
        ..RuntimePromptContext::default()
    };
    assert_eq!(contributor.runtime_section(&other), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_extension_that_goes_away_takes_its_row_and_its_bag_with_it() {
    let harness = Harness::with(&["talkative"]);
    harness.approve("talkative");
    harness.settle_default().await;
    assert_eq!(harness.host.channels().len(), 2);

    // Uninstalled out from under the host, which is what `rm -rf` looks like.
    std::fs::remove_dir_all(harness.install("talkative")).unwrap();
    harness.settle_default().await;

    assert!(harness.host.status().is_empty());
    assert!(harness.host.channels().is_empty());
    assert!(harness.host.providers().is_empty());
    assert!(harness.host.contributors().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_that_move_restart_the_extension_and_settings_that_do_not_leave_it_alone() {
    let harness = Harness::with_example();
    harness.approve("hello");
    harness.settle_default().await;
    let first = harness.host.pid("hello").expect("it is running");

    // The same digest and the same settings: left running, connections intact.
    harness.settle_default().await;
    assert_eq!(harness.host.pid("hello"), Some(first));

    // An extension reads its settings once, in the handshake, so a changed
    // block has to reach it as a restart or it would sit in `config.yaml`
    // doing nothing until the next boot.
    let mut config = ExtensionsConfig::default();
    config.settings.insert(
        "hello".to_owned(),
        serde_json::from_value(serde_json::json!({"greeting": "Ahoy"})).unwrap(),
    );
    harness.settle(&config).await;
    let second = harness.host.pid("hello").expect("it is running again");
    assert_ne!(first, second);
    assert_eq!(harness.state("hello"), ExtensionState::Ready);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_handshake_that_never_completes_is_a_failed_row_within_the_cap() {
    let harness = Harness::with(&["stubborn"]);
    harness.approve("stubborn");

    // The stubborn fixture answers every request, so the cap is proved by
    // shortening it past what any process can meet.
    let host = ExtensionHost::new(
        ExtensionHostOptions::new(harness.store.clone(), &harness.root).with_timings(Timings {
            init_timeout: Duration::from_millis(1),
            ..common::quick()
        }),
    );
    host.reconcile(&ExtensionsConfig::default());
    assert!(host.quiesce(Duration::from_secs(30)).await);

    let row = host
        .status()
        .into_iter()
        .find(|row| row.id == "stubborn")
        .expect("a row");
    assert_eq!(row.state, ExtensionState::Failed);
    assert!(
        row.last_error.unwrap_or_default().contains("handshake"),
        "the row does not say what went wrong"
    );
    host.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_whose_extension_is_gone_is_a_refusal_not_a_hang() {
    let harness = Harness::with(&["slow"]);
    harness.approve("slow");
    harness.settle_default().await;
    assert_eq!(harness.host.commands().len(), 1);

    harness.host.stop().await;
    let error = harness
        .host
        .run_command("slow-forever", "", None, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.message.contains("slow-forever"));
}

/// Whether `pid` names a live process.
fn alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

/// Every pid the tally fixture was started as, oldest first.
fn tallied(harness: &Harness) -> Vec<i32> {
    std::fs::read_to_string(harness.root.join("extension-data/tally/pids"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

/// Stops the host and kills any child it left, so a failure leaks nothing.
async fn survivors(harness: &Harness) -> Vec<i32> {
    let alive_now: Vec<i32> = tallied(harness)
        .into_iter()
        .filter(|pid| alive(*pid))
        .collect();
    harness.host.stop().await;
    for pid in tallied(harness) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    alive_now
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_reconcile_mid_handshake_leaves_the_start_alone() {
    let harness = Harness::with(&["tally"]);
    harness.approve("tally");

    // Back to back, so the second arrives before the first start has landed.
    harness.host.reconcile(&ExtensionsConfig::default());
    harness.settle_default().await;
    assert_eq!(harness.state("tally"), ExtensionState::Ready);
    let running = i32::try_from(harness.host.pid("tally").unwrap()).unwrap();

    assert!(eventually(Duration::from_secs(10), || !tallied(&harness).is_empty()).await);
    let started = tallied(&harness);
    assert_eq!(survivors(&harness).await, [running]);
    assert_eq!(started, [running]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_superseded_mid_handshake_leaves_no_child_behind() {
    let harness = Harness::with(&["tally"]);
    harness.approve("tally");

    // New settings before the first start has landed: that start is retired,
    // and its child with it.
    harness.host.reconcile(&ExtensionsConfig::default());
    let mut config = ExtensionsConfig::default();
    config.settings.insert(
        "tally".to_owned(),
        serde_json::from_value(serde_json::json!({"round": 2})).unwrap(),
    );
    harness.settle(&config).await;
    assert_eq!(harness.state("tally"), ExtensionState::Ready);
    let running = i32::try_from(harness.host.pid("tally").unwrap()).unwrap();

    assert!(eventually(Duration::from_secs(10), || tallied(&harness).len() == 2).await);
    let settled = eventually(Duration::from_secs(10), || {
        tallied(&harness)
            .into_iter()
            .filter(|pid| alive(*pid))
            .count()
            == 1
    })
    .await;
    let left = survivors(&harness).await;
    assert!(settled, "a superseded start left its child running");
    assert_eq!(left, [running]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_stops_reading_is_restarted_like_a_crashed_one() {
    let harness = Harness::with(&["deaf"]);
    harness.approve("deaf");
    harness.settle_default().await;
    assert_eq!(harness.state("deaf"), ExtensionState::Ready);
    let first = harness.host.pid("deaf").unwrap();

    let never = CancellationToken::new();
    let deafened = harness
        .host
        .run_command("deaf-now", "", None, &never)
        .await
        .unwrap();
    assert!(deafened.ok);

    // Enough to overflow the pipe and the queue together whatever order they
    // reach the writer in, so the last of them finds no room.
    let host = Arc::new(harness);
    let call = |args: String| {
        let host = Arc::clone(&host);
        tokio::spawn(async move {
            host.host
                .run_command("deaf-now", &args, None, &CancellationToken::new())
                .await
        })
    };
    let mut calls = vec![call("x".repeat(4 * 1024 * 1024))];
    for _ in 0..darkwire_extension_host::OUTBOUND_QUEUE + 128 {
        calls.push(call("x".repeat(1024)));
    }
    let outcomes = tokio::time::timeout(Duration::from_secs(20), futures::future::join_all(calls))
        .await
        .expect("every call ends once the child is called hung");
    for outcome in outcomes {
        let outcome = outcome.unwrap();
        assert!(outcome.is_err() || !outcome.unwrap().ok);
    }

    assert!(
        eventually(Duration::from_secs(20), || {
            host.host.pid("deaf").is_some_and(|pid| pid != first)
                && host.state("deaf") == ExtensionState::Ready
        })
        .await,
        "the hung child was not replaced"
    );
    assert!(!alive(i32::try_from(first).unwrap()));
    host.host.stop().await;
}
