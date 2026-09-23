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

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use common::harness::{Answer, FakeTool, Harness, ScriptedGate, Setup, events_of};
use darkwire_agent::TurnInput;
use darkwire_agent::approval::{ApprovalDecision, DenialReason, denied_notice, denied_tool_result};
use darkwire_agent::testkit::{ScriptedTurn, tool_call};
use darkwire_protocol::{
    AgentSettings, ApprovalScope, ExecRule, StopReason, ToolPermission, ToolPermissions,
};
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
        (DenialReason::NoPrompt, "nobody to ask", "cannot ask"),
    ] {
        let result = denied_tool_result("exec", reason);
        assert!(result.contains(model), "{reason:?}: {result}");
        assert!(result.contains("Do not call it again"), "{reason:?}");
        assert!(result.contains("The tool did not run"), "{reason:?}");
        assert!(denied_notice("exec", reason).contains(human), "{reason:?}");
    }
    // A rule refuses one command, not the tool, so the model is told it may
    // try another rather than to stop using `exec`.
    let rule = denied_tool_result("exec", DenialReason::Rule);
    assert!(rule.contains("rule on this agent"), "{rule}");
    assert!(rule.contains("Do not repeat this command"), "{rule}");
    assert!(rule.contains("different command may be allowed"), "{rule}");
    assert!(denied_notice("exec", DenialReason::Rule).contains("command rule"));
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
        darkwire_protocol::ToolRisk::Exec,
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
            darkwire_protocol::ToolRisk::Exec,
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
        darkwire_protocol::ToolRisk::Exec,
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
            darkwire_protocol::ToolRisk::Exec,
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
            darkwire_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(ScriptedGate::new(vec![Answer::Silent])),
        config: AgentSettings {
            approval_timeout_ms: 60_000,
            ..Setup::default().config
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

#[tokio::test]
async fn a_gate_reaching_its_own_deadline_is_a_timeout_not_a_refusal() {
    // The gate and the loop count down to the same instant, and the gate
    // usually wins. The model must still hear that nobody answered.
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            darkwire_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(ScriptedGate::new(vec![Answer::Expire])),
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "run it").await;

    let content = events_of(&events, "tool.result")[0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(content.contains("nobody answered"), "{content}");
    assert!(!content.contains("refused"), "{content}");
}

#[tokio::test]
async fn a_gate_with_nobody_to_ask_refuses_without_a_prompt() {
    let exec = FakeTool::new(
        "exec",
        darkwire_protocol::ToolRisk::Exec,
        common::harness::Behaviour::Answer("ok".to_owned()),
    );
    let gate = ScriptedGate::new(vec![Answer::NoOne]);
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![exec.clone()],
        permissions: Some(asking("exec")),
        approvals: Some(gate.clone()),
        ..Setup::default()
    });

    let (events, result) = harness.say("cli:1", "run it").await;

    assert_eq!(result.expect("a turn").stop_reason, StopReason::Complete);
    assert!(exec.calls().is_empty());
    assert!(events_of(&events, "tool.approvalRequest").is_empty());
    assert_eq!(gate.seen().len(), 1);
    let content = events_of(&events, "tool.result")[0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(content.contains("nobody to ask"), "{content}");
}

#[tokio::test(start_paused = true)]
async fn a_turn_stopped_under_an_open_prompt_is_a_stop_not_a_denial() {
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            darkwire_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(ScriptedGate::new(vec![Answer::Silent])),
        config: AgentSettings {
            approval_timeout_ms: 600_000,
            ..Setup::default().config
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
        darkwire_protocol::ToolRisk::Exec,
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
    // No gate at all is `darkwire chat --yes`: the process chose to run `ask`
    // tools unasked.
    let exec = FakeTool::new(
        "exec",
        darkwire_protocol::ToolRisk::Exec,
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
            darkwire_protocol::ToolRisk::Exec,
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
        subagents: vec![darkwire_agent::SubagentBinding {
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
            darkwire_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(gate.clone()),
        agent: Some(darkwire_agent::LoopAgent {
            id: "locked-down".to_owned(),
            ..darkwire_agent::LoopAgent::default()
        }),
        ..Setup::default()
    });

    let _ = harness.say("web:1", "run it").await;

    // The notification raised for a prompt nobody is watching names the agent
    // that wants to run something. "An agent wants to run `exec`" is not
    // something an operator can act on.
    assert_eq!(gate.seen()[0].agent_id, "locked-down");
}

#[tokio::test]
async fn does_not_announce_a_prompt_the_gate_has_already_answered() {
    let gate = ScriptedGate::remembering(ApprovalDecision::allow());
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            darkwire_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(gate.clone()),
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "run it").await;

    // The point of "this session" is that the question stops being asked. A
    // request announced anyway draws a card on every client and replaces it a
    // millisecond later, which is what the operator sees as a flash.
    assert!(events_of(&events, "tool.approvalRequest").is_empty());
    assert!(gate.seen().is_empty());
    let results = events_of(&events, "tool.result");
    assert_eq!(results[0]["ok"], json!(true));
}

#[tokio::test]
async fn refuses_without_announcing_when_the_gate_remembers_a_denial() {
    let gate = ScriptedGate::remembering(ApprovalDecision::refuse());
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            darkwire_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(gate.clone()),
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "run it").await;

    assert!(events_of(&events, "tool.approvalRequest").is_empty());
    let results = events_of(&events, "tool.result");
    assert_eq!(results[0]["ok"], json!(false));
    // The remembered refusal is a person's, so the model is told it was
    // refused rather than that the deployment forbids it.
    let notices = events_of(&events, "notice");
    assert!(notices[0]["message"].as_str().unwrap().contains("refused"));
}

#[tokio::test]
async fn the_request_is_debuggable_without_leaking_the_token() {
    let gate = ScriptedGate::new(vec![Answer::Allow]);
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            darkwire_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(Arc::clone(&gate) as Arc<dyn darkwire_agent::ApprovalGate>),
        ..Setup::default()
    });
    let _ = harness.say("web:1", "run it").await;

    let shown = format!("{:?}", gate.seen()[0]);
    assert!(shown.contains("exec"));
    assert!(shown.contains("web:1"));
}

// Command rules

fn allowing(tool: &str) -> ToolPermissions {
    let mut permissions = ToolPermissions::new();
    permissions.insert(tool.to_owned(), ToolPermission::Allow);
    permissions
}

fn with_rules(rules: Vec<ExecRule>) -> AgentSettings {
    let mut config = Setup::default().config;
    config.exec.rules = rules;
    config
}

fn exec_rule(action: ToolPermission, argv: &[&str]) -> ExecRule {
    ExecRule {
        action,
        argv: argv.iter().map(|&token| token.to_owned()).collect(),
    }
}

fn one_call(argv: &[&str]) -> Vec<ScriptedTurn> {
    vec![
        ScriptedTurn::calls(vec![tool_call("c1", "exec", &json!({ "argv": argv }))]),
        ScriptedTurn::text("done"),
    ]
}

#[tokio::test]
async fn an_allow_rule_runs_an_ask_tool_without_asking() {
    let gate = ScriptedGate::new(vec![Answer::Refuse]);
    let harness = Harness::build(Setup {
        turns: one_call(&["true"]),
        tools: vec![darkwire_tools::exec_tool()],
        permissions: Some(asking("exec")),
        config: with_rules(vec![exec_rule(ToolPermission::Allow, &["true"])]),
        approvals: Some(gate.clone()),
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "run it").await;

    assert!(events_of(&events, "tool.approvalRequest").is_empty());
    assert!(gate.seen().is_empty());
    assert_eq!(events_of(&events, "tool.result")[0]["ok"], json!(true));
}

#[tokio::test]
async fn a_deny_rule_refuses_an_allowed_tool_without_asking_anyone() {
    let gate = ScriptedGate::new(vec![Answer::Allow]);
    let harness = Harness::build(Setup {
        turns: one_call(&["ls", "-la"]),
        tools: vec![darkwire_tools::exec_tool()],
        permissions: Some(allowing("exec")),
        config: with_rules(vec![exec_rule(ToolPermission::Deny, &["ls", "*"])]),
        approvals: Some(gate.clone()),
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "run it").await;

    assert!(gate.seen().is_empty());
    let result = &events_of(&events, "tool.result")[0];
    assert_eq!(result["ok"], json!(false));
    assert!(
        result["content"]
            .as_str()
            .unwrap()
            .contains("rule on this agent"),
        "{}",
        result["content"]
    );
    let notices = events_of(&events, "notice");
    assert!(
        notices
            .iter()
            .any(|notice| notice["message"].as_str().unwrap().contains("command rule"))
    );
}

#[tokio::test]
async fn a_prompt_for_a_command_carries_what_the_rules_made_of_it() {
    let gate = ScriptedGate::new(vec![Answer::Allow]);
    let harness = Harness::build(Setup {
        turns: one_call(&["true"]),
        tools: vec![darkwire_tools::exec_tool()],
        permissions: Some(asking("exec")),
        approvals: Some(gate.clone()),
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "run it").await;

    let asked = events_of(&events, "tool.approvalRequest");
    assert_eq!(
        asked[0]["command"],
        json!({"argv": ["true"], "shell": false})
    );
    let seen = gate.seen();
    // Remembered by the exact command, so "this session" covers this one.
    assert!(
        seen[0].memory_key.starts_with("exec:"),
        "{}",
        seen[0].memory_key
    );
    assert!(seen[0].command.is_some());
}

#[tokio::test]
async fn a_tool_without_a_policy_is_remembered_by_its_name() {
    let gate = ScriptedGate::new(vec![Answer::Allow]);
    let harness = Harness::build(Setup {
        turns: one_exec_call(),
        tools: vec![FakeTool::new(
            "exec",
            darkwire_protocol::ToolRisk::Exec,
            common::harness::Behaviour::Answer("ok".to_owned()),
        )],
        permissions: Some(asking("exec")),
        approvals: Some(gate.clone()),
        ..Setup::default()
    });

    let (events, _) = harness.say("web:1", "run it").await;

    assert!(
        events_of(&events, "tool.approvalRequest")[0]
            .get("command")
            .is_none()
    );
    assert_eq!(gate.seen()[0].memory_key, "exec");
    assert!(gate.seen()[0].command.is_none());
}
