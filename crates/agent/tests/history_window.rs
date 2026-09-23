//! Fitting the history to the context window by estimated tokens.

use darkwire_agent::history_window::{FixedCost, history_budget, token_window_start};
use darkwire_core::history::has_orphaned_tool_result;
use darkwire_core::messages::{
    AssistantOptions, ToolOptions, assistant_message, tool_message, user_message,
};
use darkwire_core::session_store::StoredMessageRecord;
use darkwire_protocol::{ChatMessage, ToolCall};
use darkwire_providers::estimate_message_tokens;

fn record(seq: i64, turn: Option<&str>, message: ChatMessage) -> StoredMessageRecord {
    StoredMessageRecord {
        id: format!("m{seq}"),
        session_key: "web:1".to_owned(),
        seq,
        created_at_ms: 0,
        turn_id: turn.map(str::to_owned),
        message,
    }
}

fn user(text: &str) -> ChatMessage {
    ChatMessage::User(user_message(text))
}

fn assistant(text: &str) -> ChatMessage {
    ChatMessage::Assistant(assistant_message(text, AssistantOptions::default()))
}

fn calling(id: &str) -> ChatMessage {
    ChatMessage::Assistant(assistant_message(
        "",
        AssistantOptions {
            tool_calls: vec![ToolCall {
                id: id.to_owned(),
                name: "read".to_owned(),
                arguments_json: "{}".to_owned(),
            }],
            ..AssistantOptions::default()
        },
    ))
}

fn result(id: &str, text: &str) -> ChatMessage {
    ChatMessage::Tool(tool_message(id, "read", text, ToolOptions::default()))
}

fn cost(records: &[StoredMessageRecord]) -> usize {
    records
        .iter()
        .map(|record| estimate_message_tokens(&record.message))
        .sum()
}

fn long(marker: &str) -> String {
    format!("{marker} ").repeat(200)
}

#[test]
fn no_known_window_means_no_budget() {
    assert_eq!(
        history_budget(&FixedCost {
            context_window_tokens: 0,
            prompt_tokens: 100,
            max_output_tokens: 100,
        }),
        None
    );
}

#[test]
fn the_budget_is_nine_tenths_of_what_the_fixed_costs_leave() {
    assert_eq!(
        history_budget(&FixedCost {
            context_window_tokens: 1_000,
            prompt_tokens: 100,
            max_output_tokens: 100,
        }),
        Some(720)
    );
    // A prompt larger than the window leaves nothing rather than wrapping.
    assert_eq!(
        history_budget(&FixedCost {
            context_window_tokens: 1_000,
            prompt_tokens: 5_000,
            max_output_tokens: 100,
        }),
        Some(0)
    );
}

#[test]
fn a_history_that_fits_is_kept_whole() {
    let records = vec![
        record(1, Some("t1"), user("hi")),
        record(2, Some("t1"), assistant("hello")),
        record(3, Some("t2"), user("now")),
    ];
    assert_eq!(token_window_start(&records, Some(3), usize::MAX), 0);
}

#[test]
fn the_turn_being_answered_is_kept_whatever_it_costs() {
    let records = vec![
        record(1, Some("t1"), user("hi")),
        record(2, Some("t1"), assistant("hello")),
        record(3, Some("t2"), user(&long("question"))),
        record(4, Some("t2"), calling("c1")),
        record(5, Some("t2"), result("c1", &long("output"))),
    ];
    assert_eq!(token_window_start(&records, Some(3), 0), 2);
}

#[test]
fn the_cut_lands_on_a_turn_boundary_and_never_on_a_steer() {
    let records = vec![
        record(1, Some("t1"), user(&long("old"))),
        record(2, Some("t1"), assistant("answered")),
        record(3, Some("t2"), user(&long("asked"))),
        record(4, Some("t2"), user("steered")),
        record(5, Some("t2"), assistant("answered")),
        record(6, Some("t3"), user("now")),
    ];
    // Room for the steer and its answer, not for the question before them.
    let budget = cost(&records[3..]);
    assert_eq!(token_window_start(&records, Some(6), budget), 5);
    // Room for the whole of `t2`.
    let budget = cost(&records[2..]);
    assert_eq!(token_window_start(&records, Some(6), budget), 2);
}

#[test]
fn an_opening_row_outside_the_window_cuts_nothing() {
    let records = vec![
        record(10, Some("t1"), user(&long("a"))),
        record(11, Some("t1"), assistant(&long("b"))),
    ];
    assert_eq!(token_window_start(&records, Some(3), 0), 0);
}

#[test]
fn a_tool_result_never_outlives_its_call() {
    // Rows with no turn id fall back to any user message as a boundary, and
    // one sits between a call and its result.
    let records = vec![
        record(1, None, user(&long("old"))),
        record(2, None, calling("c1")),
        record(3, None, user("aside")),
        record(4, None, result("c1", "output")),
        record(5, None, assistant("answered")),
        record(6, None, user("now")),
    ];
    let budget = cost(&records[2..]);
    let start = token_window_start(&records, Some(6), budget);
    assert!(start > 2, "{start}");
    let kept: Vec<ChatMessage> = records[start..]
        .iter()
        .map(|record| record.message.clone())
        .collect();
    assert!(!has_orphaned_tool_result(&kept));
    assert_eq!(kept.last(), Some(&user("now")));
}

#[test]
fn between_turns_every_row_is_an_older_turn() {
    let records = vec![
        record(1, Some("t1"), user(&long("old"))),
        record(2, Some("t1"), assistant("answered")),
        record(3, Some("t2"), user("newer")),
        record(4, Some("t2"), assistant("answered")),
    ];
    assert_eq!(token_window_start(&records, None, usize::MAX), 0);
    assert_eq!(token_window_start(&records, None, cost(&records[2..])), 2);
    assert_eq!(token_window_start(&records, None, 0), 4);
}
