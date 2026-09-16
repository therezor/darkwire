//! The agent-resolution policy, on its own.
//!
//! The route tests cover this through HTTP; these cover the two cases the route
//! cannot easily reach and the one a second caller depends on — a chat channel
//! asking about a conversation that has not been spoken in.
//!
//! The policy in one sentence: a session is measured against **its own agent**,
//! unless that agent has since been deleted, in which case the honest answer is
//! the one a turn *would* get — the default — reported through
//! `requestedAgentId` so a reader can be told what they are looking at rather
//! than quietly shown something else.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_core::messages::user_message;
use darkwire_core::session_store::{AppendOptions, CreateSession};
use darkwire_protocol::config::Config;
use darkwire_protocol::messages::ChatMessage;
use darkwire_server::context::build_context_response;
use darkwire_server::runtime::ServerRuntime as _;
use darkwire_server::testkit::{TestServer, TestServerOptions, start_test_server};
use serde_json::json;

fn server(config: Option<Config>) -> TestServer {
    start_test_server(TestServerOptions {
        config,
        ..TestServerOptions::default()
    })
    .expect("a test server")
}

/// A conversation with one thing said in it.
fn spoken_in(test: &TestServer, key: &str, agent_id: Option<&str>) {
    let store = test.runtime.store();
    store
        .ensure_session(
            key,
            CreateSession {
                agent_id: agent_id.map(str::to_owned),
                ..CreateSession::default()
            },
        )
        .expect("a session row");
    store
        .append(
            key,
            ChatMessage::User(user_message("hello")),
            &AppendOptions::default(),
        )
        .expect("a stored message");
}

#[tokio::test]
async fn a_session_that_does_not_exist_measures_to_nothing() {
    // `None` rather than a failure: the route turns it into a 404 and a chat
    // channel says "nothing to measure yet", and a conversation that has been
    // created but not spoken in is normal rather than an error.
    let test = server(None);
    let report = build_context_response(test.runtime.as_ref(), "nope")
        .await
        .expect("the measurement ran");
    assert!(report.is_none());
}

#[tokio::test]
async fn a_session_that_has_been_spoken_in_is_measured() {
    let test = server(None);
    spoken_in(&test, "web:1", None);

    let report = build_context_response(test.runtime.as_ref(), "web:1")
        .await
        .expect("the measurement ran")
        .expect("a conversation with something in it");

    assert_eq!(report.session_key, "web:1");
    assert!(report.estimated_tokens > 0);
    assert!(report.context_window_tokens > 0);
}

#[tokio::test]
async fn it_measures_against_the_sessions_own_agent() {
    // Its tools, its prompt and its window are what a turn here would carry; a
    // meter read against another agent's is simply wrong.
    let test = server(None);
    spoken_in(&test, "web:1", Some("default"));

    let report = build_context_response(test.runtime.as_ref(), "web:1")
        .await
        .expect("the measurement ran")
        .expect("a measured conversation");

    assert_eq!(report.agent_id.as_deref(), Some("default"));
    // Absent is the healthy state, so a client can treat its presence as the
    // whole signal.
    assert_eq!(report.requested_agent_id, None);
}

#[tokio::test]
async fn a_bound_agent_that_is_gone_falls_back_and_says_so() {
    // A 404 would be wrong: the conversation lists and opens perfectly well,
    // and a turn in it *would* run — on the default.
    let test = server(None);
    spoken_in(&test, "web:1", Some("departed"));

    let report = build_context_response(test.runtime.as_ref(), "web:1")
        .await
        .expect("the measurement ran")
        .expect("a measured conversation");

    assert_eq!(report.requested_agent_id.as_deref(), Some("departed"));
    assert_ne!(report.agent_id.as_deref(), Some("departed"));
    assert_eq!(report.agent_id.as_deref(), Some("default"));
}

#[tokio::test]
async fn an_agent_that_still_exists_is_not_reported_as_a_fallback() {
    let config = darkwire_protocol::config::parse_config(json!({
        "agents": {"list": {"reviewer": {"label": "Reviewer"}}},
    }))
    .expect("a parseable config");
    let test = server(Some(config));
    spoken_in(&test, "web:1", Some("reviewer"));

    let report = build_context_response(test.runtime.as_ref(), "web:1")
        .await
        .expect("the measurement ran")
        .expect("a measured conversation");

    assert_eq!(report.requested_agent_id, None);
}

#[tokio::test]
async fn it_reports_the_breakdown_a_meter_is_drawn_from() {
    let test = server(None);
    spoken_in(&test, "web:1", None);

    let report = build_context_response(test.runtime.as_ref(), "web:1")
        .await
        .expect("the measurement ran")
        .expect("a measured conversation");

    // A number with nothing behind it is the part of an inspector that gets
    // asked about.
    assert!(!report.breakdown.is_empty());
    for section in ["systemPrompt", "tools", "messages", "runtimeBlock"] {
        assert!(
            report.breakdown.contains_key(section),
            "{section} is missing from the breakdown"
        );
    }
}

#[tokio::test]
async fn the_prompt_is_the_one_the_agent_would_actually_send() {
    // It comes from the loop rather than being reassembled here, so the preview
    // and the turn cannot drift.
    let test = start_test_server(TestServerOptions {
        runtime: darkwire_server::testkit::FakeRuntimeOptions {
            system_prompt: Some("# DarkWire\n\nSession: {session}".to_owned()),
            runtime_block: Some("## Live state".to_owned()),
            ..darkwire_server::testkit::FakeRuntimeOptions::default()
        },
        ..TestServerOptions::default()
    })
    .expect("a test server");
    spoken_in(&test, "web:1", None);

    let report = build_context_response(test.runtime.as_ref(), "web:1")
        .await
        .expect("the measurement ran")
        .expect("a measured conversation");

    assert!(report.system_prompt.contains("Session: web:1"));
    assert_eq!(report.runtime_block, "## Live state");
}

#[tokio::test]
async fn the_window_never_carries_the_models_own_reasoning() {
    // The wire has never carried reasoning, and a payload that shipped it
    // invited the panel to show it.
    let test = server(None);
    spoken_in(&test, "web:1", None);

    let report = build_context_response(test.runtime.as_ref(), "web:1")
        .await
        .expect("the measurement ran")
        .expect("a measured conversation");

    for stored in &report.messages {
        if let ChatMessage::Assistant(assistant) = &stored.message {
            assert_eq!(assistant.reasoning, None);
        }
    }
}

#[tokio::test]
async fn a_session_row_with_no_messages_still_measures() {
    // Created but not spoken in is normal: the meter reads zero-ish rather than
    // refusing.
    let test = server(None);
    test.runtime
        .store()
        .ensure_session("web:quiet", CreateSession::default())
        .expect("a session row");

    let report = build_context_response(test.runtime.as_ref(), "web:quiet")
        .await
        .expect("the measurement ran")
        .expect("a row exists, so there is something to measure");
    assert_eq!(report.session_key, "web:quiet");
    assert!(report.messages.is_empty());
}
