//! The approval gate: what it parks, what it remembers, and every way a
//! parked request can end without a person answering it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire_agent::{ApprovalDecision, ApprovalGate, ApprovalRequest};
use darkwire_core::ErrorKind;
use darkwire_core::testkit::ManualClock;
use darkwire_protocol::tools::{ApprovalScope, ToolRisk};
use darkwire_server::approvals::{HubApprovalGate, HubApprovalGateOptions, UnattendedApproval};
use parking_lot::Mutex;
use serde_json::json;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const START_MS: i64 = 1_700_000_000_000;
const TIMEOUT_MS: u64 = 60_000;
/// A deadline near enough to wait out, for the two tests that are about the
/// deadline. Everything else uses `TIMEOUT_MS`, which never fires during a test.
const SOON_MS: u64 = 30;

#[derive(Default)]
struct RequestOptions {
    session_key: Option<&'static str>,
    root_session_key: Option<&'static str>,
    agent_id: Option<&'static str>,
    call_id: Option<&'static str>,
    name: Option<&'static str>,
    memory_key: Option<&'static str>,
    token: Option<CancellationToken>,
    expires_at_ms: Option<u64>,
}

fn approval_request(options: RequestOptions) -> ApprovalRequest {
    let session_key = options.session_key.unwrap_or("web:1").to_owned();
    ApprovalRequest {
        root_session_key: options
            .root_session_key
            .map_or_else(|| session_key.clone(), str::to_owned),
        session_key,
        agent_id: options.agent_id.unwrap_or("default").to_owned(),
        turn_id: "turn-1".to_owned(),
        call_id: options.call_id.unwrap_or("call-1").to_owned(),
        name: options.name.unwrap_or("exec").to_owned(),
        args: json!({ "argv": ["ls"] }),
        risk: ToolRisk::Exec,
        memory_key: options
            .memory_key
            .or(options.name)
            .unwrap_or("exec")
            .to_owned(),
        command: None,
        expires_at_ms: options
            .expires_at_ms
            .unwrap_or(u64::try_from(START_MS).unwrap() + TIMEOUT_MS),
        token: options.token.unwrap_or_default(),
    }
}

fn gate_at(start_ms: i64) -> Arc<HubApprovalGate> {
    Arc::new(HubApprovalGate::new(HubApprovalGateOptions {
        clock: Some(Arc::new(ManualClock::at(start_ms))),
        ..HubApprovalGateOptions::default()
    }))
}

/// Asks in a task of its own, because the answer arrives from this one.
fn ask(gate: &Arc<HubApprovalGate>, request: ApprovalRequest) -> JoinHandle<ApprovalDecision> {
    let gate = Arc::clone(gate);
    tokio::spawn(async move { gate.ask(&request).await.unwrap() })
}

/// Asks and keeps the error, for the tests about a deadline.
fn ask_until_expiry(
    gate: &Arc<HubApprovalGate>,
    request: ApprovalRequest,
) -> JoinHandle<darkwire_core::Result<ApprovalDecision>> {
    let gate = Arc::clone(gate);
    tokio::spawn(async move { gate.ask(&request).await })
}

/// Waits until the gate is holding `count` prompts, so an answer is not sent
/// before there is anything to answer.
async fn parked(gate: &HubApprovalGate, count: usize) {
    for _ in 0..1_000 {
        if gate.pending_count() == count {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the gate never parked {count} request(s)");
}

// Parking and answering

#[tokio::test]
async fn parks_a_request_until_a_client_answers_it() {
    let gate = gate_at(START_MS);
    let pending = ask(&gate, approval_request(RequestOptions::default()));
    parked(&gate, 1).await;

    assert!(gate.resolve("call-1", true, ApprovalScope::Once));

    let decision = pending.await.unwrap();
    assert!(decision.approved);
    assert_eq!(decision.scope, Some(ApprovalScope::Once));
    assert_eq!(gate.pending_count(), 0);
}

#[tokio::test]
async fn answers_to_nobody_for_an_unknown_call_id() {
    let gate = gate_at(START_MS);
    assert!(!gate.resolve("call-nobody-asked-about", true, ApprovalScope::Once));
}

#[tokio::test]
async fn does_not_remember_an_answer_scoped_to_once() {
    let gate = gate_at(START_MS);

    let first = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("a"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.resolve("a", true, ApprovalScope::Once);
    first.await.unwrap();

    let second = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("b"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.resolve("b", true, ApprovalScope::Once);
    assert!(second.await.unwrap().approved);
}

#[tokio::test]
async fn remembers_a_session_scoped_answer_for_that_session_alone() {
    let gate = gate_at(START_MS);

    let first = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("a"),
            session_key: Some("web:1"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.resolve("a", true, ApprovalScope::Session);
    first.await.unwrap();

    // Same session, same tool: answered without asking.
    let second = gate
        .ask(&approval_request(RequestOptions {
            call_id: Some("b"),
            session_key: Some("web:1"),
            ..RequestOptions::default()
        }))
        .await
        .unwrap();
    assert!(second.approved);
    assert_eq!(second.scope, Some(ApprovalScope::Session));
    assert_eq!(gate.pending_count(), 0);

    // Another session has not answered anything.
    let _third = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("c"),
            session_key: Some("web:2"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
}

// What the loop is told before it announces a prompt

#[tokio::test]
async fn reports_a_remembered_answer_without_parking_anything() {
    let gate = gate_at(START_MS);

    let first = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("a"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.resolve("a", true, ApprovalScope::Session);
    first.await.unwrap();

    // The loop asks this before it emits `tool.approvalRequest`. An answer here
    // is what keeps a prompt the operator has already answered off the screen
    // instead of drawing it and replacing it in the same breath.
    let second = approval_request(RequestOptions {
        call_id: Some("b"),
        ..RequestOptions::default()
    });
    let remembered = gate.remembered(&second).expect("the session answered");
    assert!(remembered.approved);
    assert_eq!(remembered.scope, Some(ApprovalScope::Session));
    assert_eq!(gate.pending_count(), 0);
}

#[tokio::test]
async fn reports_nothing_for_a_tool_nobody_has_answered_for() {
    let gate = gate_at(START_MS);

    assert!(
        gate.remembered(&approval_request(RequestOptions::default()))
            .is_none()
    );
}

#[tokio::test]
async fn remembers_a_refusal_exactly_like_an_approval() {
    // "No, and stop asking" is a thing users mean, and a scope that only ever
    // widened permission would be a scope that works in one direction.
    let gate = gate_at(START_MS);

    let first = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("a"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.resolve("a", false, ApprovalScope::Session);
    let decision = first.await.unwrap();
    assert!(!decision.approved);
    assert_eq!(decision.scope, Some(ApprovalScope::Session));

    let second = gate
        .ask(&approval_request(RequestOptions {
            call_id: Some("b"),
            ..RequestOptions::default()
        }))
        .await
        .unwrap();
    assert!(!second.approved);
    assert_eq!(second.scope, Some(ApprovalScope::Session));
}

#[tokio::test]
async fn scopes_memory_by_tool_name_so_another_tool_still_asks() {
    // Remembered by name, never by arguments: a memory keyed by arguments would
    // be a cache nobody can predict.
    let gate = gate_at(START_MS);

    let first = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("a"),
            name: Some("exec"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.resolve("a", true, ApprovalScope::Session);
    first.await.unwrap();

    let _second = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("b"),
            name: Some("write"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
}

#[tokio::test]
async fn remembers_a_tool_without_a_policy_by_its_name() {
    let gate = gate_at(START_MS);
    let first = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("a"),
            name: Some("deploy"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.resolve("a", true, ApprovalScope::Session);
    first.await.unwrap();

    // Same tool, different arguments. A tool that keys on its name approves
    // the next call too, whatever it turns out to be.
    let mut different = approval_request(RequestOptions {
        call_id: Some("b"),
        name: Some("deploy"),
        ..RequestOptions::default()
    });
    different.args = json!({ "target": "production" });
    assert!(gate.ask(&different).await.unwrap().approved);
}

#[tokio::test]
async fn remembers_a_command_by_its_key_and_asks_about_another() {
    let gate = gate_at(START_MS);
    let first = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("a"),
            memory_key: Some("exec:cargo-test"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.resolve("a", true, ApprovalScope::Session);
    first.await.unwrap();

    let same = approval_request(RequestOptions {
        call_id: Some("b"),
        memory_key: Some("exec:cargo-test"),
        ..RequestOptions::default()
    });
    assert!(gate.remembered(&same).unwrap().approved);

    // Approving `cargo test` for the session does not approve `rm -rf`.
    let other = approval_request(RequestOptions {
        call_id: Some("c"),
        memory_key: Some("exec:rm"),
        ..RequestOptions::default()
    });
    assert!(gate.remembered(&other).is_none());
    assert!(gate.pending("c").is_none());
}

#[tokio::test]
async fn hands_back_the_request_still_parked_under_a_call() {
    let gate = gate_at(START_MS);
    let pending = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("a"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    assert_eq!(gate.pending("a").unwrap().call_id, "a");
    assert!(gate.pending("b").is_none());
    gate.resolve("a", false, ApprovalScope::Once);
    pending.await.unwrap();
    assert!(gate.pending("a").is_none());
}

// Deadlines and cancellation

#[tokio::test]
async fn times_out_when_the_deadline_passes_unanswered() {
    // A real deadline, a short one: the assertion is that it *expires*, which
    // only becomes more true the longer the machine takes to get here. An
    // error of kind `timeout`, so the loop tells the model nobody answered
    // rather than that somebody said no.
    let gate = gate_at(START_MS);
    let pending = ask_until_expiry(
        &gate,
        approval_request(RequestOptions {
            expires_at_ms: Some(u64::try_from(START_MS).unwrap() + SOON_MS),
            ..RequestOptions::default()
        }),
    );

    let error = pending.await.unwrap().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert_eq!(gate.pending_count(), 0);
}

#[tokio::test]
async fn times_out_immediately_when_the_deadline_has_already_passed() {
    // The loop hands the gate a wall-clock instant, and a clock that has moved
    // is not a reason to wait forever.
    let gate = gate_at(START_MS);
    let error = gate
        .ask(&approval_request(RequestOptions {
            expires_at_ms: Some(u64::try_from(START_MS).unwrap() - 1),
            ..RequestOptions::default()
        }))
        .await
        .unwrap_err();

    assert_eq!(error.kind, ErrorKind::Timeout);
    assert_eq!(gate.pending_count(), 0);
}

#[tokio::test]
async fn drops_a_prompt_when_its_turn_is_cancelled() {
    // Cancellation resolves as a denial rather than failing: the loop already
    // stopped racing this, and a tool failure the model then sees would be
    // worse than a denial nobody reads.
    let gate = gate_at(START_MS);
    let token = CancellationToken::new();
    let pending = ask(
        &gate,
        approval_request(RequestOptions {
            token: Some(token.clone()),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;

    token.cancel();

    assert!(!pending.await.unwrap().approved);
    assert_eq!(gate.pending_count(), 0);
}

#[tokio::test]
async fn refuses_without_parking_when_the_turn_is_already_cancelled() {
    let gate = gate_at(START_MS);
    let token = CancellationToken::new();
    token.cancel();

    let decision = gate
        .ask(&approval_request(RequestOptions {
            token: Some(token),
            ..RequestOptions::default()
        }))
        .await
        .unwrap();

    assert!(!decision.approved);
    assert_eq!(gate.pending_count(), 0);
}

#[tokio::test]
async fn supersedes_an_earlier_prompt_that_reused_a_call_id() {
    // A `call_id` is the model's, so a collision is not impossible. The older
    // prompt is the one nothing will answer — its turn has moved on.
    let gate = gate_at(START_MS);
    let first = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("call-1"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    let second = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("call-1"),
            ..RequestOptions::default()
        }),
    );

    assert!(!first.await.unwrap().approved);
    parked(&gate, 1).await;

    gate.resolve("call-1", true, ApprovalScope::Once);
    assert!(second.await.unwrap().approved);
}

#[tokio::test]
async fn settles_pending_prompts_and_forgets_memory_when_a_session_closes() {
    let gate = gate_at(START_MS);

    let remembered = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("a"),
            session_key: Some("web:1"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.resolve("a", true, ApprovalScope::Session);
    remembered.await.unwrap();

    let pending = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("b"),
            session_key: Some("web:2"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.clear_session("web:2");
    assert!(!pending.await.unwrap().approved);

    gate.clear_session("web:1");
    let _third = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("c"),
            session_key: Some("web:1"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
}

#[tokio::test]
async fn settles_a_prompt_a_subagent_parked_under_the_conversation_that_closed() {
    // The operator watching that prompt was watching this conversation, and
    // closing it is what took the answer away.
    let gate = gate_at(START_MS);
    let mut request = approval_request(RequestOptions {
        call_id: Some("a"),
        session_key: Some("subagent:1"),
        ..RequestOptions::default()
    });
    request.root_session_key = "web:1".to_owned();

    let pending = {
        let gate = Arc::clone(&gate);
        tokio::spawn(async move { gate.ask(&request).await.unwrap() })
    };
    parked(&gate, 1).await;

    gate.clear_session("web:1");
    assert!(!pending.await.unwrap().approved);
}

// Nobody watching

/// A gate that records what it raised, over a watcher count the test sets.
fn watched(
    counts: &'static [(&'static str, usize)],
) -> (Arc<HubApprovalGate>, Arc<Mutex<Vec<UnattendedApproval>>>) {
    let raised = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&raised);
    let gate = Arc::new(HubApprovalGate::new(HubApprovalGateOptions {
        clock: Some(Arc::new(ManualClock::at(START_MS))),
        watchers: Some(Arc::new(move |session_key: &str| {
            counts
                .iter()
                .find(|(key, _)| *key == session_key)
                .map_or(0, |(_, count)| *count)
        })),
        on_unattended: Some(Arc::new(move |approval| sink.lock().push(approval))),
    }));
    (gate, raised)
}

#[tokio::test]
async fn raises_the_request_that_reached_an_empty_room() {
    // A scheduled run's session is its own and nothing subscribes to it, so the
    // prompt goes nowhere and the turn waits out the whole timeout for a denial
    // that was certain when it was raised. This is what sends someone to look.
    let (gate, raised) = watched(&[]);
    let _pending = ask(
        &gate,
        approval_request(RequestOptions {
            session_key: Some("automation:job-1:run-1"),
            name: Some("exec"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;

    assert_eq!(
        *raised.lock(),
        vec![UnattendedApproval {
            session_key: "automation:job-1:run-1".to_owned(),
            agent_id: "default".to_owned(),
            tool_name: "exec".to_owned(),
            expires_at_ms: u64::try_from(START_MS).unwrap() + TIMEOUT_MS,
        }]
    );
}

#[tokio::test]
async fn stays_quiet_when_a_tab_is_open_on_the_session() {
    let (gate, raised) = watched(&[("web:1", 1)]);
    let _pending = ask(
        &gate,
        approval_request(RequestOptions {
            session_key: Some("web:1"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    assert!(raised.lock().is_empty());
}

#[tokio::test]
async fn counts_a_subagent_prompt_against_the_conversation_watching_it() {
    // A subagent's session exists for one delegation and nothing subscribes to
    // it. The person who would answer is looking at the parent.
    let (gate, raised) = watched(&[("web:1", 1)]);
    let _pending = ask(
        &gate,
        approval_request(RequestOptions {
            session_key: Some("web:1:sub:1"),
            root_session_key: Some("web:1"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    assert!(raised.lock().is_empty());
}

#[tokio::test]
async fn raises_an_unwatched_subagent_prompt_against_its_conversation() {
    let (gate, raised) = watched(&[]);
    let _pending = ask(
        &gate,
        approval_request(RequestOptions {
            session_key: Some("automation:job-1:run-1:sub:1"),
            root_session_key: Some("automation:job-1:run-1"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    assert_eq!(raised.lock()[0].session_key, "automation:job-1:run-1");
}

#[tokio::test]
async fn does_not_raise_for_a_call_answered_from_memory_which_asks_nobody() {
    let (gate, raised) = watched(&[]);
    let first = ask(
        &gate,
        approval_request(RequestOptions {
            call_id: Some("a"),
            session_key: Some("s"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.resolve("a", true, ApprovalScope::Session);
    first.await.unwrap();
    raised.lock().clear();

    // Second call on the same session: settled by the remembered answer without
    // ever being parked, so there is nothing for anyone to come and do.
    gate.ask(&approval_request(RequestOptions {
        call_id: Some("b"),
        session_key: Some("s"),
        ..RequestOptions::default()
    }))
    .await
    .unwrap();
    assert!(raised.lock().is_empty());
}

#[tokio::test]
async fn does_not_raise_for_a_turn_that_was_already_cancelled() {
    let (gate, raised) = watched(&[]);
    let token = CancellationToken::new();
    token.cancel();

    gate.ask(&approval_request(RequestOptions {
        token: Some(token),
        ..RequestOptions::default()
    }))
    .await
    .unwrap();
    assert!(raised.lock().is_empty());
}

#[tokio::test]
async fn raises_once_when_it_parks_not_again_when_it_expires() {
    // A notification saying "this needed you five minutes ago" is worse than
    // none: by the timeout the answer is already decided.
    let (gate, raised) = watched(&[]);
    let pending = ask_until_expiry(
        &gate,
        approval_request(RequestOptions {
            expires_at_ms: Some(u64::try_from(START_MS).unwrap() + SOON_MS),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    assert_eq!(raised.lock().len(), 1);

    assert!(pending.await.unwrap().is_err());
    assert_eq!(raised.lock().len(), 1);
}

#[tokio::test]
async fn treats_an_untracked_deployment_as_attended_so_a_fixture_raises_nothing() {
    // Neither hook wired is every path except the live server. Defaulting the
    // other way would raise a notification for every prompt the CLI shows.
    let raised: Arc<Mutex<Vec<UnattendedApproval>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&raised);
    let gate = Arc::new(HubApprovalGate::new(HubApprovalGateOptions {
        clock: Some(Arc::new(ManualClock::at(START_MS))),
        watchers: None,
        on_unattended: Some(Arc::new(move |approval| sink.lock().push(approval))),
    }));

    let _pending = ask(&gate, approval_request(RequestOptions::default()));
    parked(&gate, 1).await;
    assert!(raised.lock().is_empty());
}

// Across agents

#[tokio::test]
async fn keeps_a_session_scoped_answer_to_that_session_whoever_runs_it() {
    let gate = gate_at(START_MS);

    let first = ask(
        &gate,
        approval_request(RequestOptions {
            agent_id: Some("writer"),
            call_id: Some("c1"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
    gate.resolve("c1", true, ApprovalScope::Session);
    first.await.unwrap();

    // A session is bound to one agent, so the session scope needs no agent
    // dimension — but it must not reach a different session.
    let _second = ask(
        &gate,
        approval_request(RequestOptions {
            session_key: Some("web:2"),
            agent_id: Some("writer"),
            call_id: Some("c2"),
            ..RequestOptions::default()
        }),
    );
    parked(&gate, 1).await;
}
