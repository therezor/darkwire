//! Fitting the history to the context window by estimated tokens.

use darkwire_agent::history_window::{
    FixedCost, RowPrices, calibration_of, history_budget, token_window_start,
};
use darkwire_core::history::has_orphaned_tool_result;
use darkwire_core::messages::{
    AssistantOptions, FileDetails, ToolOptions, assistant_message, file_part, tool_message,
    user_message,
};
use darkwire_core::session_store::StoredMessageRecord;
use darkwire_protocol::{Calibration, ChatMessage, StepStats, ToolCall, UserMessage};
use darkwire_providers::{IMAGE_TOKENS, estimate_message_tokens};

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
            calibration: None,
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
            calibration: None,
        }),
        Some(720)
    );
    // A prompt larger than the window leaves nothing rather than wrapping.
    assert_eq!(
        history_budget(&FixedCost {
            context_window_tokens: 1_000,
            prompt_tokens: 5_000,
            max_output_tokens: 100,
            calibration: None,
        }),
        Some(0)
    );
}

fn budget_at(calibration: Option<f64>) -> Option<usize> {
    history_budget(&FixedCost {
        context_window_tokens: 1_000,
        prompt_tokens: 100,
        max_output_tokens: 100,
        calibration,
    })
}

#[test]
fn a_calibrated_budget_is_the_window_in_the_providers_tokens() {
    // Undercounting by 30%: the prompt really takes 130, and what is left is
    // worth fewer estimated tokens. (1000 - 130 - 100) * 0.9 / 1.3
    assert_eq!(budget_at(Some(1.3)), Some(533));
    // Overcounting by 20%: more history fits than the estimate alone allowed.
    // (1000 - 80 - 100) * 0.9 / 0.8
    assert_eq!(budget_at(Some(0.8)), Some(922));
    assert_eq!(budget_at(Some(1.0)), budget_at(None));
}

#[test]
fn a_count_far_below_the_estimate_cannot_grow_the_budget_past_the_floor() {
    // A server truncating to a smaller context reports less and less as the
    // history grows. Believing it would let the history grow with it.
    assert_eq!(budget_at(Some(0.3)), budget_at(Some(0.75)));
    assert_eq!(budget_at(Some(0.3)), Some(990));
}

fn answered_on(seq: i64, model: &str, reported: u64) -> StoredMessageRecord {
    let mut message = assistant_message("answer", AssistantOptions::default());
    message.stats = Some(Box::new(StepStats {
        calibration: Some(Calibration {
            reported_tokens: reported,
            estimated_tokens: 100,
            model: model.to_owned(),
        }),
        ..StepStats::default()
    }));
    record(seq, Some("t"), ChatMessage::Assistant(message))
}

#[test]
fn the_newest_count_decides_and_only_for_its_own_model() {
    let records = vec![
        record(1, Some("t"), user("a")),
        answered_on(2, "small", 150),
        record(3, Some("u"), user("b")),
        answered_on(4, "small", 120),
        record(5, Some("v"), user("c")),
    ];
    assert_eq!(calibration_of(&records, "small"), Some(1.2));
    // Not the older count on the same model: a session moved to another one
    // reads as estimated until that model has answered.
    let moved = [records.clone(), vec![answered_on(6, "large", 90)]].concat();
    assert_eq!(calibration_of(&moved, "small"), None);
    assert_eq!(calibration_of(&records[..1], "small"), None);
}

fn attached(seq: i64, mime_type: &str, path: &str, size_bytes: u64) -> StoredMessageRecord {
    record(
        seq,
        Some("t"),
        ChatMessage::User(UserMessage {
            content: vec![file_part(
                path,
                mime_type,
                FileDetails {
                    name: None,
                    size_bytes: Some(size_bytes),
                },
            )],
            ..user_message("")
        }),
    )
}

#[test]
fn an_attachment_is_priced_as_the_request_carries_it() {
    let extra = |row: &StoredMessageRecord, prices: &mut RowPrices| {
        prices.of(row) - estimate_message_tokens(&row.message)
    };
    let mut vision = RowPrices::new(true);
    let mut blind = RowPrices::new(false);

    // The file's text, about four bytes a token.
    let notes = attached(1, "text/markdown", "notes.md", 4_000);
    assert_eq!(extra(&notes, &mut vision), 1_000);
    // Too long to paste in: only the reference line goes.
    let log = attached(2, "text/plain", "big.log", 64 * 1024);
    assert_eq!(extra(&log, &mut vision), 0);

    let photo = attached(3, "image/png", "photo.png", 300_000);
    assert_eq!(extra(&photo, &mut vision), IMAGE_TOKENS);
    // A model that cannot see gets the path instead.
    assert_eq!(extra(&photo, &mut blind), 0);
    let huge = attached(4, "image/png", "huge.png", 40 * 1024 * 1024);
    assert_eq!(extra(&huge, &mut vision), 0);
}

#[test]
fn a_history_that_fits_is_kept_whole() {
    let records = vec![
        record(1, Some("t1"), user("hi")),
        record(2, Some("t1"), assistant("hello")),
        record(3, Some("t2"), user("now")),
    ];
    assert_eq!(
        token_window_start(&records, Some(3), usize::MAX, &mut RowPrices::default()),
        0
    );
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
    assert_eq!(
        token_window_start(&records, Some(3), 0, &mut RowPrices::default()),
        2
    );
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
    assert_eq!(
        token_window_start(&records, Some(6), budget, &mut RowPrices::default()),
        5
    );
    // Room for the whole of `t2`.
    let budget = cost(&records[2..]);
    assert_eq!(
        token_window_start(&records, Some(6), budget, &mut RowPrices::default()),
        2
    );
}

#[test]
fn an_opening_row_outside_the_window_cuts_nothing() {
    let records = vec![
        record(10, Some("t1"), user(&long("a"))),
        record(11, Some("t1"), assistant(&long("b"))),
    ];
    assert_eq!(
        token_window_start(&records, Some(3), 0, &mut RowPrices::default()),
        0
    );
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
    let start = token_window_start(&records, Some(6), budget, &mut RowPrices::default());
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
    assert_eq!(
        token_window_start(&records, None, usize::MAX, &mut RowPrices::default()),
        0
    );
    assert_eq!(
        token_window_start(
            &records,
            None,
            cost(&records[2..]),
            &mut RowPrices::default()
        ),
        2
    );
    assert_eq!(
        token_window_start(&records, None, 0, &mut RowPrices::default()),
        4
    );
}
