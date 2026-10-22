//! A step: one model call and the tools it asked for, written and reported as
//! one unit while the turn goes on.

use crate::common;

use common::harness::{FakeTool, Harness, Setup, events_of};
use darkwire_agent::testkit::{ScriptedTurn, raw_tool_call, tool_call};
use darkwire_protocol::{ChatMessage, StepStats, Usage};
use darkwire_providers::FinishReason;
use serde_json::{Value, json};

fn usage(prompt: u64, completion: u64) -> Usage {
    Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        cached_tokens: None,
        reasoning_tokens: None,
    }
}

fn stats_of(message: &ChatMessage) -> Option<&StepStats> {
    match message {
        ChatMessage::Assistant(assistant) => assistant.stats.as_deref(),
        _ => None,
    }
}

fn tool_then_answer() -> Harness {
    Harness::build(Setup {
        turns: vec![
            ScriptedTurn {
                usage: Some(usage(10, 4)),
                generation_ms: Some(100.0),
                first_token_ms: Some(50.0),
                ..ScriptedTurn::calls(vec![tool_call("c1", "read", &json!({"path": "a.md"}))])
            },
            ScriptedTurn {
                usage: Some(usage(20, 6)),
                ..ScriptedTurn::text("It says hello.")
            },
        ],
        tools: vec![FakeTool::reading("read", "file contents")],
        ..Setup::default()
    })
}

#[tokio::test]
async fn each_step_is_reported_with_the_rows_it_wrote() {
    let harness = tool_then_answer();
    let (events, result) = harness.say("web:1", "read it").await;
    result.expect("a completed turn");

    let steps = events_of(&events, "step.end");
    assert_eq!(steps.len(), 2);
    // The user row is 1; the first step wrote the call and its result.
    assert_eq!(steps[0]["firstSeq"], json!(2));
    assert_eq!(steps[0]["lastSeq"], json!(3));
    assert_eq!(steps[1]["firstSeq"], json!(4));
    assert_eq!(steps[1]["lastSeq"], json!(4));
    assert_eq!(steps[0]["stats"]["usage"]["promptTokens"], json!(10));
    assert_eq!(steps[1]["stats"]["usage"]["promptTokens"], json!(20));
    assert_eq!(steps[0]["stats"]["generationMs"], json!(100));
    assert_eq!(steps[0]["stats"]["generationTokens"], json!(4));
    assert_eq!(steps[0]["stats"]["firstTokenMs"], json!(50));
    assert_eq!(steps[1]["stats"].get("generationMs"), None);
}

#[tokio::test]
async fn a_step_is_reported_after_its_tools_and_before_the_next_call() {
    let harness = tool_then_answer();
    let (events, _) = harness.say("web:1", "read it").await;

    let tags: Vec<&str> = events.iter().map(darkwire_agent::AgentEvent::tag).collect();
    let result = tags.iter().position(|tag| *tag == "tool.result").unwrap();
    let step = tags.iter().position(|tag| *tag == "step.end").unwrap();
    let answer = tags
        .iter()
        .position(|tag| *tag == "assistant.delta")
        .unwrap();
    assert!(result < step && step < answer, "{tags:?}");
    assert_eq!(tags.last(), Some(&"turn.end"));
}

#[tokio::test]
async fn each_assistant_row_stores_its_own_step() {
    let harness = tool_then_answer();
    harness.say("web:1", "read it").await.1.expect("a turn");

    let stored = harness.stored("web:1");
    let first = stats_of(&stored[1]).expect("the call's row has stats");
    let second = stats_of(&stored[3]).expect("the answer's row has stats");
    assert_eq!(first.usage, usage(10, 4));
    assert_eq!(second.usage, usage(20, 6));
    assert!(first.model.is_some());
    assert!(matches!(stored[2], ChatMessage::Tool(_)));
}

#[tokio::test]
async fn the_steps_sum_to_the_turn() {
    let harness = tool_then_answer();
    let (events, result) = harness.say("web:1", "read it").await;
    let result = result.expect("a turn");

    let prompt: u64 = events_of(&events, "step.end")
        .iter()
        .filter_map(|step| step["stats"]["usage"]["promptTokens"].as_u64())
        .sum();
    assert_eq!(prompt, result.usage.prompt_tokens);
    let end = &events_of(&events, "turn.end")[0];
    assert_eq!(end["usage"]["promptTokens"], json!(30));
}

#[tokio::test]
async fn a_call_that_wrote_nothing_lands_its_figures_on_the_next_row() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn {
                finish_reason: Some(FinishReason::Length),
                usage: Some(usage(5, 3)),
                ..ScriptedTurn::calls(vec![raw_tool_call("c1", "write", r#"{"path": "#)])
            },
            ScriptedTurn {
                usage: Some(usage(7, 2)),
                ..ScriptedTurn::text("done")
            },
        ],
        tools: vec![FakeTool::reading("write", "wrote")],
        ..Setup::default()
    });

    let (events, result) = harness.say("web:1", "write it").await;
    let result = result.expect("a turn");

    let steps = events_of(&events, "step.end");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0]["stats"]["usage"]["promptTokens"], json!(12));
    assert_eq!(steps[0]["stats"]["usage"]["completionTokens"], json!(5));

    let stored = harness.stored("web:1");
    assert_eq!(stored.len(), 2);
    assert_eq!(stats_of(&stored[1]).unwrap().usage, result.usage);
}

#[tokio::test]
async fn the_figures_never_reach_the_provider() {
    let harness = tool_then_answer();
    harness.say("web:1", "read it").await.1.expect("a turn");

    // The second request replays the first step's row, stats and all.
    let request = &harness.provider.requests()[1];
    let body = serde_json::to_value(&request.messages).unwrap();
    let replayed = body
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == json!("assistant"))
        .expect("the first step's assistant row");
    assert!(replayed.get("stats").is_some_and(Value::is_object));

    let wire: Vec<_> = request
        .messages
        .iter()
        .map(darkwire_providers::wire_encode::encode_message)
        .collect();
    let encoded = serde_json::to_string(&wire).unwrap();
    assert!(!encoded.contains("elapsedMs"), "{encoded}");
}
