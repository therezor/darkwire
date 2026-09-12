//! The gate: who is asked, what an answer means, and what happens when nobody
//! answers.
//!
//! The split under test is that the loop decides *whether* to ask and the gate
//! decides the answer — so no transport can forget to check, and none can
//! decide differently.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot be built is a failing test either way"
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::harness::{Answer, FakeTool, Harness, ScriptedGate, Setup, events_of};
use ghostai_agent::TurnInput;
use ghostai_agent::approval::{ApprovalDecision, DenialReason, denied_notice, denied_tool_result};
use ghostai_agent::testkit::{ScriptedTurn, tool_call};
use ghostai_protocol::{ApprovalScope, StopReason, ToolPermission, ToolPermissions, ToolsConfig};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn asking(tool: &str) -> ToolPermissions {
    let mut permissions = ToolPermissions::new();
    permissions.insert(tool.to_owned(), ToolPermission::Ask);
    permissions
}

fn denying(tool: &str) -> ToolPermissions {
    let mut permissions = ToolPermissions::new();
    permissions.insert(tool.to_owned(), ToolPermission::Deny);
    permissions
}

fn one_exec_call() -> Vec<ScriptedTurn> {
    vec![
        ScriptedTurn::calls(vec![tool_call("c1", "exec", &json!({"argv": ["ls"]}))]),
        ScriptedTurn::text("done"),
    ]
}

// The wording

#[test]
fn a_denial_says_which_of_the_three_it_was() {
    // The difference between "this deployment does not do that" and "you were
    // asked and said no" is what stops a model retrying the first one.
    for (reason, model, human) in [
        (
            DenialReason::Policy,
            "approval policy",
            "policy for this tool",
        ),
        (DenialReason::Declined, "user refused", "call was refused"),
        (DenialReason::Timeout, "nobody answered", "expired"),
    ] {
        let result = denied_tool_result("exec", reason);
        assert!(result.contains(model), "{reason:?}: {result}");
        assert!(result.contains("Do not call it again"), "{reason:?}");
        assert!(result.contains("The tool did not run"), "{reason:?}");
        assert!(denied_notice("exec", reason).contains(human), "{reason:?}");
    }
}

#[test]
fn a_decision_is_a_value_with_or_without_a_scope() {
    let allow = ApprovalDecision::allow();
    assert!(allow.approved);
    assert_eq!(allow.scope, None);

    let refuse = ApprovalDecision::refuse();
    assert!(!refuse.approved);

    let scoped = ApprovalDecision {
        scope: Some(ApprovalScope::Session),
        reason: Some("looks fine".to_owned()),
        ..ApprovalDecision::allow()
    };
    assert_eq!(scoped.scope, Some(ApprovalScope::Session));
    assert_ne!(scoped, allow);
    assert!(format!("{scoped:?}").contains("Session"));
}

// Asking

#[tokio::test]
async fn it_asks_before_an_ask_tool_and_runs_it_once_approved() {
    let exec = FakeTool::new(
        "exec",
        ghostai_protocol::ToolRisk::Exec,
        common::harness::Behaviour::Answer("ok".to_owned()),
    );
    let gate = ScriptedGate::new(vec![Answer::Allow]);
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![exec.clone()],
        permissions: Some(asking("exec")),
        approvals: Some(gate.clone()),
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "run it").await;

    assert_eq!(result.expect("a turn").stop_reason, StopReason::Complete);
    assert_eq!(exec.calls().len(), 1);

    let asked = events_of(&events, "tool.approvalRequest");
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["name"], json!("exec"));
    assert_eq!(asked[0]["risk"], json!("exec"));
    assert_eq!(asked[0]["args"], json!({"argv": ["ls"]}));
    // Wall clock, not elapsed: a reconnecting client has to render the same
    // deadline.
    assert!(asked[0]["expiresAtMs"].as_u64().unwrap() > 1_700_000_000_000);

    let seen = gate.seen();
    assert_eq!(seen[0].session_key, "web:1");
    assert_eq!(seen[0].root_session_key, "web:1");
    assert_eq!(seen[0].agent_id, "default");
    assert_eq!(seen[0].call_id, "c1");
    // The turn's own token, so a gate keeping pending prompts drops this one
    // when the turn ends: a prompt left on screen for a turn that is over is a
    // decision that can no longer mean anything. By the time the test reads it
    // the turn has been let go, so it has fired.
    assert!(seen[0].token.is_cancelled());
}

#[tokio::test]
async fn a_tool_the_agent_allows_is_never_asked_about() {
    let gate = ScriptedGate::new(vec![Answer::Refuse]);
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            ghostai_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        approvals: Some(gate.clone()),
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "run it").await;

    assert!(events_of(&events, "tool.approvalRequest").is_empty());
    assert!(gate.seen().is_empty());
}

#[tokio::test]
async fn a_refused_call_is_answered_and_the_turn_continues() {
    let exec = FakeTool::new(
        "exec",
        ghostai_protocol::ToolRisk::Exec,
        common::harness::Behaviour::Answer("ok".to_owned()),
    );
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![exec.clone()],
        permissions: Some(asking("exec")),
        approvals: Some(ScriptedGate::new(vec![Answer::Refuse])),
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "run it").await;

    // A denial lets the turn continue so the model can respond to it.
    assert_eq!(result.expect("a turn").stop_reason, StopReason::Complete);
    assert!(exec.calls().is_empty(), "nothing ran");

    let notices = events_of(&events, "notice");
    assert_eq!(notices[0]["kind"], json!("approval_denied"));
    let results = events_of(&events, "tool.result");
    assert_eq!(results[0]["ok"], json!(false));
    // A denial the model cannot see is an unanswered tool call, which is a
    // provider 400 on the next turn rather than a refusal it can work around.
    assert!(
        results[0]["content"]
            .as_str()
            .unwrap()
            .contains("the user refused")
    );
}

#[tokio::test]
async fn a_gate_that_fails_denies() {
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            ghostai_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(ScriptedGate::new(vec![Answer::Fail])),
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "run it").await;

    // There is no failure mode of an approval mechanism where the safe reading
    // is "go ahead".
    assert_eq!(result.expect("a turn").stop_reason, StopReason::Complete);
    assert_eq!(
        events_of(&events, "notice")[0]["kind"],
        json!("approval_denied")
    );
}

#[tokio::test(start_paused = true)]
async fn nobody_answering_denies_at_the_deadline() {
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            ghostai_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(ScriptedGate::new(vec![Answer::Silent])),
        tools_config: ToolsConfig {
            approval_timeout_ms: 60_000,
            ..ToolsConfig::default()
        },
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "run it").await;

    // The deadline is the loop's rather than the gate's, because the case it
    // exists for is a gate that never answers.
    assert_eq!(result.expect("a turn").stop_reason, StopReason::Complete);
    let results = events_of(&events, "tool.result");
    assert!(
        results[0]["content"]
            .as_str()
            .unwrap()
            .contains("nobody answered")
    );
}

#[tokio::test(start_paused = true)]
async fn a_turn_stopped_under_an_open_prompt_is_a_stop_not_a_denial() {
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            ghostai_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(ScriptedGate::new(vec![Answer::Silent])),
        tools_config: ToolsConfig {
            approval_timeout_ms: 600_000,
            ..ToolsConfig::default()
        },
        ..Setup::default()
    });

    let turn = harness
        .agent_loop
        .run(TurnInput::new("web:1", "run it"), &CancellationToken::new());
    let token = turn.token().clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        token.cancel();
    });
    let (events, result) = turn.collect().await;

    // A denial lets the turn continue; a cancellation stops it and the
    // remaining calls. The difference matters to the caller.
    assert_eq!(result.expect("a turn").stop_reason, StopReason::Aborted);
    let notices = events_of(&events, "notice");
    assert!(
        !notices
            .iter()
            .any(|notice| notice["kind"] == json!("approval_denied"))
    );
}

#[tokio::test]
async fn deny_is_enforced_with_no_gate_installed() {
    let exec = FakeTool::new(
        "exec",
        ghostai_protocol::ToolRisk::Exec,
        common::harness::Behaviour::Answer("ok".to_owned()),
    );
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![exec.clone()],
        permissions: Some(denying("exec")),
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "run it").await;

    // Refusing needs no one to answer.
    assert!(exec.calls().is_empty());
    let results = events_of(&events, "tool.result");
    assert!(
        results[0]["content"]
            .as_str()
            .unwrap()
            .contains("approval policy")
            || results[0]["content"]
                .as_str()
                .unwrap()
                .contains("No tool named"),
        "{}",
        results[0]["content"]
    );
}

#[tokio::test]
async fn an_ask_policy_with_no_gate_runs_the_tool() {
    // Denying here would make the default config refuse every command in a
    // terminal session, where the operator asking for it *is* the approval.
    let exec = FakeTool::new(
        "exec",
        ghostai_protocol::ToolRisk::Exec,
        common::harness::Behaviour::Answer("ok".to_owned()),
    );
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![exec.clone()],
        permissions: Some(asking("exec")),
        approvals: None,
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "run it").await;

    assert_eq!(result.expect("a turn").stop_reason, StopReason::Complete);
    assert_eq!(exec.calls().len(), 1);
    assert!(events_of(&events, "tool.approvalRequest").is_empty());
}

#[tokio::test]
async fn calls_are_gated_one_at_a_time_in_the_order_the_model_asked() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![
                tool_call("c1", "exec", &json!({"n": 1})),
                tool_call("c2", "exec", &json!({"n": 2})),
            ]),
            ScriptedTurn::text("done"),
        ],
        tools: vec![FakeTool::new(
            "exec",
            ghostai_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(ScriptedGate::new(vec![Answer::Allow, Answer::Refuse])),
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "run both").await;

    let asked = events_of(&events, "tool.approvalRequest");
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0]["callId"], json!("c1"));
    assert_eq!(asked[1]["callId"], json!("c2"));

    let results = events_of(&events, "tool.result");
    assert_eq!(results[0]["ok"], json!(true));
    assert_eq!(results[1]["ok"], json!(false));
}

#[tokio::test]
async fn a_subagent_delegation_is_gated_on_the_conversation_not_the_delegation() {
    // An operator picks "this session" while looking at their conversation, and
    // the conversation is what they meant — a subagent's own session exists for
    // the length of one delegation.
    let gate = ScriptedGate::new(vec![Answer::Refuse]);
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call(
                "c1",
                "ask_researcher",
                &json!({"task": "find out"}),
            )]),
            ScriptedTurn::text("done"),
        ],
        subagents: vec![ghostai_agent::SubagentBinding {
            tool_name: "ask_researcher".to_owned(),
            agent_id: "researcher".to_owned(),
            label: "Researcher".to_owned(),
            prompt: String::new(),
            permission: ToolPermission::Ask,
        }],
        approvals: Some(gate.clone()),
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "delegate").await;

    let seen = gate.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].name, "ask_researcher");
    assert_eq!(seen[0].root_session_key, "web:1");
    let results = events_of(&events, "tool.result");
    assert_eq!(results[0]["ok"], json!(false));
}

#[tokio::test]
async fn an_approval_request_names_the_agent_that_asked() {
    let gate = ScriptedGate::new(vec![Answer::Allow]);
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            ghostai_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(gate.clone()),
        agent: Some(ghostai_agent::LoopAgent {
            id: "locked-down".to_owned(),
            ..ghostai_agent::LoopAgent::default()
        }),
        ..Setup::default()
    });

    let _ = harness.say("web:1", "run it").await;

    // A standing "always allow" given while using a permissive agent must not
    // silently pre-approve it for a locked-down one: the permission an operator
    // granted was to that agent, not to a name.
    assert_eq!(gate.seen()[0].agent_id, "locked-down");
}

#[tokio::test]
async fn the_request_is_debuggable_without_leaking_the_token() {
    let gate = ScriptedGate::new(vec![Answer::Allow]);
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            ghostai_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(Arc::clone(&gate) as Arc<dyn ghostai_agent::ApprovalGate>),
        ..Setup::default()
    });
    let _ = harness.say("web:1", "run it").await;

    let shown = format!("{:?}", gate.seen()[0]);
    assert!(shown.contains("exec"));
    assert!(shown.contains("web:1"));
}
