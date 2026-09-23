//! What a turn on a session would send, measured.
//!
//! The figures are of the request *body*, not of what is stored: a field that
//! never reaches a provider is never billed.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot be built is a failing test either way"
)]

use crate::common;

use common::harness::{FakeTool, Harness, Setup};
use darkwire_agent::context::{
    ContextBreakdown, MeasureContext, MeasureWindow, describe_context, measure_context,
    measure_context_with,
};
use darkwire_agent::testkit::{ScriptedTurn, tool_call};
use darkwire_agent::{PromptPreview, PromptPreviewInput};
use darkwire_core::history::DEFAULT_MAX_HISTORY_MESSAGES;
use darkwire_core::messages::{AssistantOptions, assistant_message, tool_message, user_message};
use darkwire_core::session_store::AppendOptions;
use darkwire_protocol::{ChatMessage, ToolCall};
use serde_json::json;

fn preview() -> PromptPreview {
    PromptPreview {
        static_prompt: "You are a helpful agent working in a workspace.".to_owned(),
        runtime_block: "## Live state\n\nCurrent time: now".to_owned(),
    }
}

#[tokio::test]
async fn a_session_that_has_not_started_has_no_context_to_describe() {
    // Inventing an empty one would report a system prompt for a workspace
    // nobody chose.
    let harness = Harness::simple();
    let report = describe_context(
        &harness.store,
        &harness.agent_loop,
        &[],
        &PromptPreviewInput {
            session_key: "web:never".to_owned(),
            ..Default::default()
        },
        65_536,
    )
    .await
    .expect("a measurement");

    assert!(report.is_none());
}

#[tokio::test]
async fn it_prices_both_halves_of_the_prompt_apart() {
    let harness = Harness::simple();
    let _ = harness.say("web:1", "hello").await;

    let report = measure_context(&MeasureContext {
        store: &harness.store,
        tools: &[],
        session_key: "web:1",
        prompt: &preview(),
        context_window_tokens: 65_536,
    })
    .expect("a report");

    assert_eq!(report.session_key, "web:1");
    assert_eq!(report.context_window_tokens, 65_536);
    assert_eq!(report.system_prompt, preview().static_prompt);
    assert_eq!(report.runtime_block, preview().runtime_block);

    // The trailing turn is reported apart from the system prompt because it is
    // the one section billed at full price on every iteration.
    assert!(report.breakdown.system_prompt > 0);
    assert!(report.breakdown.runtime_block > 0);
    assert!(report.breakdown.messages > 0);
    assert_eq!(report.breakdown.tools, 0, "no tools, no charge");
    assert_eq!(
        report.estimated_tokens,
        report.breakdown.system_prompt
            + report.breakdown.tools
            + report.breakdown.messages
            + report.breakdown.runtime_block
    );
}

#[tokio::test]
async fn raw_mode_has_no_second_half_to_price() {
    let harness = Harness::simple();
    let _ = harness.say("web:1", "hello").await;

    let report = measure_context(&MeasureContext {
        store: &harness.store,
        tools: &[],
        session_key: "web:1",
        prompt: &PromptPreview {
            runtime_block: String::new(),
            ..preview()
        },
        context_window_tokens: 65_536,
    })
    .expect("a report");

    assert_eq!(report.breakdown.runtime_block, 0);
}

#[tokio::test]
async fn the_tools_are_returned_as_well_as_measured() {
    // The breakdown says a number and the only follow-up question anyone has is
    // *which* tools.
    let harness = Harness::build(Setup {
        tools: vec![FakeTool::reading("read", "x")],
        ..Setup::default()
    });
    let _ = harness.say("web:1", "hello").await;
    let definitions = harness.agent_loop.permitted_definitions();

    let report = measure_context(&MeasureContext {
        store: &harness.store,
        tools: &definitions,
        session_key: "web:1",
        prompt: &preview(),
        context_window_tokens: 65_536,
    })
    .expect("a report");

    assert!(report.breakdown.tools > 0);
    assert_eq!(report.tools.len(), 1);
    // The list is fuller than the figure: risk is on the entry because the
    // panel badges it, and it is not billed because it is not sent.
    assert_eq!(report.tools[0].risk, darkwire_protocol::ToolRisk::Safe);
}

#[tokio::test]
async fn every_window_entry_is_matched_back_to_the_row_it_came_from() {
    let harness = Harness::build(Setup {
        turns: vec![
            ScriptedTurn::calls(vec![tool_call("c1", "read", &json!({}))]),
            ScriptedTurn::text("done"),
        ],
        tools: vec![FakeTool::reading("read", "contents")],
        ..Setup::default()
    });
    let _ = harness.say("web:1", "go").await;

    let report = measure_context(&MeasureContext {
        store: &harness.store,
        tools: &[],
        session_key: "web:1",
        prompt: &preview(),
        context_window_tokens: 65_536,
    })
    .expect("a report");

    // Storage records rather than wire types, each carrying the id and seq a
    // panel needs.
    assert_eq!(report.messages.len(), 4);
    let seqs: Vec<i64> = report.messages.iter().map(|record| record.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4]);
    assert!(report.messages.iter().all(|record| !record.id.is_empty()));
}

#[tokio::test]
async fn a_session_past_the_message_cap_reports_the_newest_rows() {
    let harness = Harness::simple();
    let rows: Vec<ChatMessage> = (0..DEFAULT_MAX_HISTORY_MESSAGES + 7)
        .map(|index| {
            if index % 2 == 0 {
                ChatMessage::User(user_message(format!("question {index}")))
            } else {
                ChatMessage::Assistant(assistant_message(
                    format!("answer {index}"),
                    AssistantOptions::default(),
                ))
            }
        })
        .collect();
    let total = i64::try_from(rows.len()).unwrap();
    harness
        .store
        .append_many("web:1", rows, &AppendOptions::default())
        .expect("stored");

    let report = measure_context(&MeasureContext {
        store: &harness.store,
        tools: &[],
        session_key: "web:1",
        prompt: &preview(),
        context_window_tokens: 65_536,
    })
    .expect("a report");

    // The cap lands on an answer, so the window opens at the next question.
    assert_eq!(report.messages.len(), DEFAULT_MAX_HISTORY_MESSAGES - 1);
    assert_eq!(report.messages.last().unwrap().seq, total);
    assert!(matches!(report.messages[0].message, ChatMessage::User(_)));
    let seqs: Vec<i64> = report.messages.iter().map(|record| record.seq).collect();
    assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]));
}

#[tokio::test]
async fn a_window_the_walker_trims_keeps_the_rows_it_kept() {
    // The walker only ever trims from the front, which is what lets each entry
    // be matched back to its row without object identity.
    let harness = Harness::simple();
    // An orphaned tool result at the head: legal history starts after it.
    harness
        .store
        .append_many(
            "web:1",
            vec![
                ChatMessage::Tool(tool_message(
                    "orphan",
                    "read",
                    "no call made this",
                    darkwire_core::messages::ToolOptions::default(),
                )),
                ChatMessage::User(user_message("the real start")),
                ChatMessage::Assistant(assistant_message("hello", AssistantOptions::default())),
            ],
            &AppendOptions::default(),
        )
        .expect("stored");

    let report = measure_context(&MeasureContext {
        store: &harness.store,
        tools: &[],
        session_key: "web:1",
        prompt: &preview(),
        context_window_tokens: 65_536,
    })
    .expect("a report");

    assert_eq!(report.messages.len(), 2);
    assert_eq!(report.messages[0].seq, 2);
    assert!(matches!(report.messages[0].message, ChatMessage::User(_)));
}

#[tokio::test]
async fn reasoning_is_stored_beside_the_answer_and_billed_nothing() {
    let harness = Harness::simple();
    harness
        .store
        .append(
            "web:1",
            ChatMessage::User(user_message("hi")),
            &AppendOptions::default(),
        )
        .expect("stored");
    let plain = measure_context(&MeasureContext {
        store: &harness.store,
        tools: &[],
        session_key: "web:1",
        prompt: &preview(),
        context_window_tokens: 65_536,
    })
    .expect("a report")
    .breakdown
    .messages;

    harness
        .store
        .append(
            "web:1",
            ChatMessage::Assistant(assistant_message(
                "the answer",
                AssistantOptions {
                    reasoning: Some("a very long private deliberation".repeat(20)),
                    reasoning_ms: None,
                    tool_calls: Vec::<ToolCall>::new(),
                },
            )),
            &AppendOptions::default(),
        )
        .expect("stored");
    let with_reasoning = measure_context(&MeasureContext {
        store: &harness.store,
        tools: &[],
        session_key: "web:1",
        prompt: &preview(),
        context_window_tokens: 65_536,
    })
    .expect("a report")
    .breakdown
    .messages;

    // It costs nothing on the wire, so it is charged nothing here — which is
    // why adding a reasoning model to a conversation does not move the number.
    let answer_alone = "the answer".len() / 2;
    assert!(
        with_reasoning - plain < answer_alone + 20,
        "{with_reasoning} vs {plain}"
    );
}

#[tokio::test]
async fn the_breakdown_is_a_map_in_request_order() {
    let breakdown = ContextBreakdown {
        system_prompt: 400,
        tools: 560,
        messages: 201,
        runtime_block: 30,
    };
    let map = breakdown.to_map();

    // Cached-then-not: the three sections a provider can serve from its prefix
    // cache, then the tail re-read at full price.
    let keys: Vec<&str> = map.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        vec!["systemPrompt", "tools", "messages", "runtimeBlock"]
    );
    // Floats because the wire carries numbers, not integers; the values are
    // counts and land exactly.
    assert!((map["systemPrompt"] - 400.0).abs() < f64::EPSILON);
    assert!((map["runtimeBlock"] - 30.0).abs() < f64::EPSILON);
    // A map rather than a fixed object, so a new section is not a wire change.
    assert_eq!(breakdown, ContextBreakdown { ..breakdown });
}

#[tokio::test]
async fn describing_a_session_uses_the_loops_own_prompt() {
    // Composing the two halves outside the loop would work today and quietly
    // lie later: memory and skills arrive as contributors attached to it.
    let harness = Harness::simple();
    let _ = harness.say("web:1", "hello").await;

    let report = describe_context(
        &harness.store,
        &harness.agent_loop,
        &[],
        &PromptPreviewInput {
            session_key: "web:1".to_owned(),
            channel: Some("cli".to_owned()),
            ..Default::default()
        },
        4_096,
    )
    .await
    .expect("a measurement")
    .expect("a session that exists");

    assert!(report.system_prompt.contains("`default` workspace"));
    assert!(report.runtime_block.contains("Current time"));
    assert_eq!(report.context_window_tokens, 4_096);
    assert!(report.estimated_tokens > 0);
}

#[tokio::test]
async fn between_turns_the_budget_leaves_out_the_oldest_turns() {
    let harness = Harness::simple();
    harness
        .store
        .ensure_session(
            "web:1",
            darkwire_core::session_store::CreateSession::default(),
        )
        .expect("a session");
    for index in 0..40 {
        let options = AppendOptions {
            turn_id: Some(format!("old-{index}")),
        };
        harness
            .store
            .append_many(
                "web:1",
                vec![
                    ChatMessage::User(user_message(format!("q{index} ").repeat(1_500))),
                    ChatMessage::Assistant(assistant_message(
                        format!("answer {index}"),
                        AssistantOptions::default(),
                    )),
                ],
                &options,
            )
            .expect("an append");
    }
    let input = MeasureContext {
        store: &harness.store,
        tools: &[],
        session_key: "web:1",
        prompt: &preview(),
        context_window_tokens: 65_536,
    };

    let report = measure_context_with(
        &input,
        &MeasureWindow {
            max_output_tokens: 8_192,
            opening_seq: None,
        },
    )
    .expect("a report");
    assert!(report.messages.len() < 80);
    assert_eq!(report.messages.len() % 2, 0, "whole turns only");
    assert_eq!(report.messages.last().expect("a row").seq, 80);

    // No window, no budget: the message cap alone.
    let unbounded = measure_context(&MeasureContext {
        context_window_tokens: 0,
        ..input
    })
    .expect("a report");
    assert_eq!(unbounded.messages.len(), 80);
}
