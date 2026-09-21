//! Reading a cheap model's two answers, including the ways it answers badly.
//!
//! The whole point of this module being pure is that these cases are reachable
//! from a [`ChatResult`] literal rather than a live endpoint.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_protocol::messages::{
    AssistantMessage, AssistantRole, ChatMessage, ContentPart, TextPart, TextTag, ToolCall, Usage,
};
use darkwire_providers::{ChatResult, FinishReason};
use darkwire_server::heartbeat::{
    DecideMessagesInput, EvaluateMessagesInput, HEARTBEAT_RESULT_TOOL, HEARTBEAT_TOOL,
    HeartbeatAction, MAX_TASK_FILE_BYTES, build_decide_messages, build_evaluate_messages,
    read_decision, read_evaluation,
};

fn result(text: &str, tool_calls: Vec<ToolCall>) -> ChatResult {
    ChatResult {
        message: AssistantMessage {
            role: AssistantRole,
            content: if text.is_empty() {
                Vec::new()
            } else {
                vec![ContentPart::Text(TextPart {
                    tag: TextTag,
                    text: text.to_owned(),
                })]
            },
            tool_calls,
            reasoning: None,
            reasoning_ms: None,
        },
        finish_reason: FinishReason::Stop,
        usage: Usage::default(),
        model: "cheap".to_owned(),
        generation_ms: None,
        first_token_ms: None,
    }
}

fn call(name: &str, arguments_json: &str) -> ToolCall {
    ToolCall {
        id: "call_1".to_owned(),
        name: name.to_owned(),
        arguments_json: arguments_json.to_owned(),
    }
}

fn decision_call(arguments_json: &str) -> ChatResult {
    result("", vec![call(&HEARTBEAT_TOOL.name, arguments_json)])
}

// The tool definitions

#[test]
fn the_two_tools_are_safe_builtins_the_model_is_told_to_call() {
    assert_eq!(HEARTBEAT_TOOL.name, "heartbeat");
    assert_eq!(HEARTBEAT_RESULT_TOOL.name, "heartbeat_result");
    for tool in [&*HEARTBEAT_TOOL, &*HEARTBEAT_RESULT_TOOL] {
        assert_eq!(tool.risk, darkwire_protocol::tools::ToolRisk::Safe);
        assert_eq!(tool.source, darkwire_protocol::tools::ToolSource::Builtin);
        assert_eq!(tool.parameters["type"], "object");
        assert_eq!(tool.parameters["additionalProperties"], false);
    }
    // `instruction` is optional on purpose: the decision is the model's job,
    // and a missing phrasing is not a reason to refuse a run it committed to.
    assert_eq!(
        HEARTBEAT_TOOL.parameters["required"],
        serde_json::json!(["action", "reason"])
    );
    assert_eq!(
        HEARTBEAT_RESULT_TOOL.parameters["required"],
        serde_json::json!(["notify", "title"])
    );
}

// Decide

#[test]
fn the_decision_request_carries_the_file_and_the_current_time() {
    let messages = build_decide_messages(&DecideMessagesInput {
        file: "TASK.md".to_owned(),
        contents: "Ship the thing on Tuesday.".to_owned(),
        now_iso: "2023-11-14T22:13:20.000Z".to_owned(),
    });

    assert_eq!(messages.len(), 2);
    let ChatMessage::System(system) = &messages[0] else {
        panic!("the first message should be the system one");
    };
    assert!(system.content.contains("2023-11-14T22:13:20.000Z"));
    assert!(system.content.contains("Choose skip unless"));

    let ChatMessage::User(user) = &messages[1] else {
        panic!("the second message should be the user one");
    };
    let ContentPart::Text(text) = &user.content[0] else {
        panic!("the user message should be text");
    };
    assert!(text.text.contains("`TASK.md`"));
    assert!(text.text.contains("Ship the thing on Tuesday."));
}

#[test]
fn a_skip_keeps_its_reason() {
    let decision = read_decision(
        &decision_call(r#"{"action":"skip","reason":"Nothing is due until Friday."}"#),
        "TASK.md",
    );
    assert_eq!(decision.action, HeartbeatAction::Skip);
    assert_eq!(decision.reason, "Nothing is due until Friday.");
    assert_eq!(decision.instruction, "");
    assert!(decision.warnings.is_empty());
}

#[test]
fn a_skip_with_no_reason_still_says_something() {
    let decision = read_decision(
        &decision_call(r#"{"action":"skip","reason":""}"#),
        "TASK.md",
    );
    assert_eq!(decision.reason, "Nothing due.");
}

#[test]
fn a_run_carries_the_models_own_phrasing() {
    let decision = read_decision(
        &decision_call(
            r#"{"action":"run","reason":"The deploy is due.","instruction":"Deploy the service."}"#,
        ),
        "TASK.md",
    );
    assert_eq!(decision.action, HeartbeatAction::Run);
    assert_eq!(decision.reason, "The deploy is due.");
    assert_eq!(decision.instruction, "Deploy the service.");
    assert!(decision.warnings.is_empty());
}

#[test]
fn a_run_with_no_phrasing_still_runs_and_says_so() {
    // The model committed to the decision; only the wording was missing, and
    // refusing on that would turn a working heartbeat into one that silently
    // never acts.
    for arguments in [
        r#"{"action":"run","reason":"Due."}"#,
        r#"{"action":"run","reason":"Due.","instruction":"   "}"#,
    ] {
        let decision = read_decision(&decision_call(arguments), "NOTES.md");
        assert_eq!(decision.action, HeartbeatAction::Run);
        assert_eq!(decision.instruction, "Read `NOTES.md` and do what it asks.");
        assert_eq!(decision.warnings.len(), 1);
        assert!(decision.warnings[0].contains("without saying what to do"));
    }
}

#[test]
fn a_run_with_no_reason_still_says_something() {
    let decision = read_decision(
        &decision_call(r#"{"action":"run","reason":"","instruction":"Go."}"#),
        "TASK.md",
    );
    assert_eq!(decision.reason, "The task file asks for work.");
}

#[test]
fn an_answer_with_no_tool_call_is_a_skip_with_a_warning() {
    // Not defensive programming: the resilience decorator strips a required
    // tool choice and retries whenever a provider objects to it, so a model
    // that answers in prose is a normal outcome of a normal degradation.
    let decision = read_decision(&result("I think you should deploy.", Vec::new()), "TASK.md");
    assert_eq!(decision.action, HeartbeatAction::Skip);
    assert_eq!(decision.reason, "The model did not answer with a decision.");
    assert_eq!(decision.warnings.len(), 1);
    assert!(decision.warnings[0].contains("without calling the decision tool"));
}

#[test]
fn a_call_to_some_other_tool_is_also_no_decision() {
    let decision = read_decision(&result("", vec![call("something_else", "{}")]), "TASK.md");
    assert_eq!(decision.action, HeartbeatAction::Skip);
}

#[test]
fn arguments_that_do_not_parse_are_a_skip_that_says_why() {
    for arguments in [
        "not json at all",
        r#"{"action":"maybe","reason":"hm"}"#,
        r#"{"reason":"no action key"}"#,
    ] {
        let decision = read_decision(&decision_call(arguments), "TASK.md");
        assert_eq!(decision.action, HeartbeatAction::Skip, "{arguments}");
        assert!(
            decision
                .reason
                .starts_with("The model's decision could not be read")
        );
        assert_eq!(decision.warnings.len(), 1);
        assert!(decision.warnings[0].contains("did not parse"));
    }
}

#[test]
fn a_very_long_reason_is_cut_to_something_a_card_can_hold() {
    let long = "x".repeat(400);
    let decision = read_decision(
        &decision_call(&format!(r#"{{"action":"skip","reason":"{long}"}}"#)),
        "TASK.md",
    );
    assert_eq!(decision.reason.chars().count(), 256);
    assert!(decision.reason.ends_with('…'));
}

#[test]
fn the_task_file_cap_is_what_a_repeated_classification_is_worth_paying_for() {
    assert_eq!(MAX_TASK_FILE_BYTES, 64 * 1024);
}

// Evaluate

#[test]
fn the_evaluation_request_carries_both_halves_of_the_run() {
    let messages = build_evaluate_messages(&EvaluateMessagesInput {
        instruction: "Deploy the service.".to_owned(),
        output: "Deployed at 12:04.".to_owned(),
    });
    assert_eq!(messages.len(), 2);
    let ChatMessage::User(user) = &messages[1] else {
        panic!("the second message should be the user one");
    };
    let ContentPart::Text(text) = &user.content[0] else {
        panic!("the user message should be text");
    };
    assert!(text.text.contains("Deploy the service."));
    assert!(text.text.contains("Deployed at 12:04."));
}

#[test]
fn an_evaluation_is_taken_at_its_word() {
    let answered = result(
        "",
        vec![call(
            &HEARTBEAT_RESULT_TOOL.name,
            r#"{"notify":false,"title":"Nothing to report","summary":"Routine."}"#,
        )],
    );
    let verdict = read_evaluation(&answered, "fallback");
    assert!(!verdict.notify);
    assert_eq!(verdict.title, "Nothing to report");
    assert_eq!(verdict.summary, "Routine.");
    assert!(verdict.warnings.is_empty());
}

#[test]
fn a_missing_summary_is_empty_rather_than_absent() {
    let answered = result(
        "",
        vec![call(
            &HEARTBEAT_RESULT_TOOL.name,
            r#"{"notify":true,"title":"Done"}"#,
        )],
    );
    assert_eq!(read_evaluation(&answered, "fallback").summary, "");
}

#[test]
fn an_unreadable_evaluation_notifies_anyway() {
    // The opposite default to the decision, and for a symmetric reason: there,
    // failing open costs an unwanted agent turn; here it costs a toast.
    for answered in [
        result("The run went fine.", Vec::new()),
        result(
            "The run went fine.",
            vec![call(&HEARTBEAT_RESULT_TOOL.name, "{{{")],
        ),
        result(
            "The run went fine.",
            vec![call(
                &HEARTBEAT_RESULT_TOOL.name,
                r#"{"notify":true,"title":""}"#,
            )],
        ),
    ] {
        let verdict = read_evaluation(&answered, "Nightly finished");
        assert!(verdict.notify);
        assert_eq!(verdict.title, "Nightly finished");
        assert_eq!(verdict.summary, "The run went fine.");
        assert_eq!(verdict.warnings.len(), 1);
    }
}

#[test]
fn a_very_long_title_is_cut_too() {
    let long = "t".repeat(400);
    let answered = result(
        "",
        vec![call(
            &HEARTBEAT_RESULT_TOOL.name,
            &format!(r#"{{"notify":true,"title":"{long}"}}"#),
        )],
    );
    assert_eq!(read_evaluation(&answered, "x").title.chars().count(), 256);
}
