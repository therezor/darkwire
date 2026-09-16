//! History windowing: the boundary rules, the truncation, the pipeline, and the
//! parity fixture.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::collections::BTreeSet;

use darkwire_core::history::{
    DEFAULT_MAX_TOOL_RESULT_CHARS, HistoryOptions, MessageWindow, SessionHistorySource,
    find_legal_end, find_legal_start, has_orphaned_tool_result, has_unanswered_tool_call,
    history_for_llm, session_history, truncate_head_tail,
};
use darkwire_core::messages::{
    AssistantOptions, ToolOptions, assistant_message, system_message, tool_message, user_message,
};
use darkwire_protocol::{ChatMessage, ToolCall};
use proptest::prelude::*;
use serde_json::Value;

const WINDOWS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/history/windows.json"
));

fn call(id: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: "read_file".to_owned(),
        arguments_json: "{}".to_owned(),
    }
}

fn calling(ids: &[&str]) -> ChatMessage {
    assistant_message(
        "",
        AssistantOptions {
            tool_calls: ids.iter().map(|id| call(id)).collect(),
            reasoning: None,
        },
    )
    .into()
}

fn user(text: &str) -> ChatMessage {
    user_message(text).into()
}

fn assistant(text: &str) -> ChatMessage {
    assistant_message(text, AssistantOptions::default()).into()
}

fn system(text: &str) -> ChatMessage {
    system_message(text).into()
}

fn tool(id: &str, content: &str) -> ChatMessage {
    tool_message(id, "read_file", content, ToolOptions::default()).into()
}

fn tool_of(message: &ChatMessage) -> &darkwire_protocol::ToolMessage {
    match message {
        ChatMessage::Tool(tool) => tool,
        other => panic!("expected a tool message, got {other:?}"),
    }
}

mod find_legal_start_tests {
    use super::*;

    #[test]
    fn accepts_an_empty_history() {
        assert_eq!(find_legal_start(&[]), 0);
    }

    #[test]
    fn accepts_a_history_with_no_tool_traffic() {
        assert_eq!(find_legal_start(&[user("hi"), assistant("hello")]), 0);
    }

    #[test]
    fn accepts_a_well_paired_exchange() {
        let messages = [
            user("read it"),
            calling(&["a"]),
            tool("a", "contents"),
            assistant("done"),
        ];
        assert_eq!(find_legal_start(&messages), 0);
    }

    #[test]
    fn cuts_past_a_leading_orphan() {
        // The window opened mid-turn: the assistant that declared `a` fell off.
        assert_eq!(
            find_legal_start(&[tool("a", "contents"), assistant("done")]),
            1
        );
    }

    #[test]
    fn keeps_a_later_well_paired_exchange_after_cutting_an_orphan() {
        let messages = [tool("a", "x"), calling(&["b"]), tool("b", "y")];
        assert_eq!(find_legal_start(&messages), 1);
    }

    #[test]
    fn pairs_each_of_several_parallel_tool_calls() {
        let messages = [
            calling(&["a", "b", "c"]),
            tool("a", "x"),
            tool("b", "y"),
            tool("c", "z"),
        ];
        assert_eq!(find_legal_start(&messages), 0);
    }

    #[test]
    fn discards_everything_when_the_only_orphan_is_last() {
        let messages = [calling(&["a"]), tool("a", "x"), tool("b", "y")];
        assert_eq!(find_legal_start(&messages), messages.len());
    }

    #[test]
    fn re_orphans_a_result_whose_declaring_assistant_is_cut_away() {
        // `a` is declared before the cut at index 2, so it cannot count as
        // declared afterwards: this is what clearing the set protects against.
        let messages = [
            calling(&["a"]),
            tool("b", "orphan"),
            tool("a", "now also orphaned"),
        ];
        assert_eq!(find_legal_start(&messages), 3);
    }

    #[test]
    fn requires_the_assistant_to_come_first_not_merely_to_exist() {
        assert_eq!(find_legal_start(&[tool("a", "x"), calling(&["a"])]), 1);
    }
}

mod has_orphaned_tool_result_tests {
    use super::*;

    #[test]
    fn is_false_for_a_paired_exchange() {
        assert!(!has_orphaned_tool_result(&[
            calling(&["a"]),
            tool("a", "x")
        ]));
    }

    #[test]
    fn is_true_for_a_leading_tool_result() {
        assert!(has_orphaned_tool_result(&[tool("a", "x")]));
    }
}

fn message_strategy() -> impl Strategy<Value = ChatMessage> {
    let id = prop_oneof![Just("a"), Just("b"), Just("c"), Just("d")];
    prop_oneof![
        Just(user("hi")),
        Just(assistant("plain answer")),
        Just(system("you are a ghost")),
        proptest::collection::btree_set(id.clone(), 1..=3)
            .prop_map(|ids: BTreeSet<&str>| { calling(&ids.into_iter().collect::<Vec<_>>()) }),
        id.prop_map(|id| tool(id, "result")),
    ]
}

fn history_strategy() -> impl Strategy<Value = Vec<ChatMessage>> {
    proptest::collection::vec(message_strategy(), 0..=24)
}

proptest! {
    #[test]
    fn find_legal_start_never_leaves_an_orphaned_tool_result_behind(
        messages in history_strategy()
    ) {
        let aligned = &messages[find_legal_start(&messages)..];
        prop_assert!(!has_orphaned_tool_result(aligned));
    }

    #[test]
    fn find_legal_start_returns_an_index_within_the_list(messages in history_strategy()) {
        prop_assert!(find_legal_start(&messages) <= messages.len());
    }

    #[test]
    fn find_legal_start_is_a_no_op_on_histories_that_were_already_legal(
        messages in history_strategy()
    ) {
        prop_assume!(!has_orphaned_tool_result(&messages));
        prop_assert_eq!(find_legal_start(&messages), 0);
    }

    #[test]
    fn find_legal_start_is_idempotent(messages in history_strategy()) {
        let aligned = &messages[find_legal_start(&messages)..];
        prop_assert_eq!(find_legal_start(aligned), 0);
    }

    #[test]
    fn the_full_pipeline_produces_a_legal_window(
        messages in history_strategy(),
        max_messages in 0_usize..=24
    ) {
        let options = HistoryOptions { max_messages, ..HistoryOptions::default() };
        prop_assert!(!has_orphaned_tool_result(&history_for_llm(&messages, &options)));
    }

    #[test]
    fn find_legal_end_never_leaves_an_unanswered_tool_call_behind(
        messages in history_strategy()
    ) {
        let kept = &messages[..find_legal_end(&messages)];
        prop_assert!(!has_unanswered_tool_call(kept));
    }

    #[test]
    fn find_legal_end_returns_an_index_within_the_list(messages in history_strategy()) {
        prop_assert!(find_legal_end(&messages) <= messages.len());
    }

    #[test]
    fn find_legal_end_is_a_no_op_on_histories_that_were_already_complete(
        messages in history_strategy()
    ) {
        prop_assume!(!has_unanswered_tool_call(&messages));
        prop_assert_eq!(find_legal_end(&messages), messages.len());
    }

    #[test]
    fn find_legal_end_is_idempotent(messages in history_strategy()) {
        let kept = &messages[..find_legal_end(&messages)];
        prop_assert_eq!(find_legal_end(kept), kept.len());
    }

    #[test]
    fn truncate_head_tail_retains_exactly_the_budgeted_units(
        text in ".*",
        max_chars in 1_usize..=64
    ) {
        let result = truncate_head_tail(&text, max_chars);
        if !result.truncated {
            return Ok(());
        }
        let units: Vec<u16> = text.encode_utf16().collect();
        let head = String::from_utf16_lossy(&units[..max_chars.div_ceil(2)]);
        prop_assert!(result.text.starts_with(&head));
        prop_assert_eq!(result.omitted, units.len() - max_chars);
    }
}

mod find_legal_end_tests {
    use super::*;

    #[test]
    fn accepts_an_empty_history() {
        assert_eq!(find_legal_end(&[]), 0);
    }

    #[test]
    fn accepts_a_history_with_no_tool_traffic() {
        assert_eq!(find_legal_end(&[user("hi"), assistant("hello")]), 2);
    }

    #[test]
    fn accepts_a_well_paired_exchange() {
        let messages = [
            user("read it"),
            calling(&["a"]),
            tool("a", "contents"),
            assistant("done"),
        ];
        assert_eq!(find_legal_end(&messages), 4);
    }

    #[test]
    fn cuts_before_an_assistant_whose_calls_were_never_answered() {
        assert_eq!(find_legal_end(&[user("read it"), calling(&["a"])]), 1);
    }

    #[test]
    fn cuts_before_the_assistant_when_only_some_of_its_calls_were_answered() {
        assert_eq!(
            find_legal_end(&[calling(&["a", "b"]), tool("a", "contents")]),
            0
        );
    }

    #[test]
    fn keeps_an_earlier_well_paired_exchange_when_cutting_a_later_one() {
        let messages = [
            user("first"),
            calling(&["a"]),
            tool("a", "x"),
            user("second"),
            calling(&["b"]),
        ];
        assert_eq!(find_legal_end(&messages), 4);
    }

    #[test]
    fn discards_answers_that_sit_past_the_cut() {
        // `b` is answered, but only after the unanswered `a`, so the answer is
        // dropped along with the cut and cannot rescue the assistant declaring it.
        let messages = [calling(&["a"]), calling(&["b"]), tool("b", "y")];
        assert_eq!(find_legal_end(&messages), 0);
    }

    #[test]
    fn pairs_each_of_several_parallel_tool_calls() {
        let messages = [
            calling(&["a", "b", "c"]),
            tool("a", "x"),
            tool("b", "y"),
            tool("c", "z"),
        ];
        assert_eq!(find_legal_end(&messages), 4);
    }
}

mod has_unanswered_tool_call_tests {
    use super::*;

    #[test]
    fn is_false_for_an_empty_history() {
        assert!(!has_unanswered_tool_call(&[]));
    }

    #[test]
    fn is_false_for_a_well_paired_exchange() {
        assert!(!has_unanswered_tool_call(&[
            calling(&["a"]),
            tool("a", "x")
        ]));
    }

    #[test]
    fn is_true_when_an_answer_never_arrives() {
        assert!(has_unanswered_tool_call(&[calling(&["a"])]));
    }

    #[test]
    fn is_true_when_the_answer_precedes_the_call() {
        assert!(has_unanswered_tool_call(&[tool("a", "x"), calling(&["a"])]));
    }
}

mod truncate_head_tail_tests {
    use super::*;

    #[test]
    fn leaves_short_text_alone() {
        let result = truncate_head_tail("short", 100);
        assert_eq!(result.text, "short");
        assert!(!result.truncated);
        assert_eq!(result.omitted, 0);
    }

    #[test]
    fn leaves_text_of_exactly_the_budget_alone() {
        assert!(!truncate_head_tail("abcde", 5).truncated);
    }

    #[test]
    fn treats_a_zero_budget_as_no_limit() {
        assert!(!truncate_head_tail("anything at all", 0).truncated);
    }

    #[test]
    fn keeps_both_ends_and_reports_the_gap() {
        let result = truncate_head_tail("abcdefghij", 4);
        assert!(result.truncated);
        assert_eq!(result.omitted, 6);
        assert!(result.text.starts_with("ab"));
        assert!(result.text.ends_with("ij"));
        assert!(result.text.contains("6 characters truncated"));
    }

    #[test]
    fn biases_the_odd_character_to_the_head() {
        let result = truncate_head_tail("abcdefghij", 5);
        assert!(result.text.starts_with("abc"));
        assert!(result.text.ends_with("ij"));
    }

    #[test]
    fn handles_a_budget_of_one_where_there_is_no_tail_to_keep() {
        let result = truncate_head_tail("abcdef", 1);
        assert!(result.text.starts_with('a'));
        assert_eq!(result.omitted, 5);
    }

    #[test]
    fn counts_utf16_code_units_and_marks_a_split_pair() {
        let result = truncate_head_tail("🐕🐕🐕🐕🐕🐕", 6);
        assert_eq!(result.omitted, 6);
        assert_eq!(
            result.text,
            "🐕\u{FFFD}\n\n… [6 characters truncated] …\n\n\u{FFFD}🐕"
        );
    }
}

mod history_for_llm_tests {
    use super::*;

    #[test]
    fn keeps_the_most_recent_max_messages() {
        let messages = [user("a"), user("b"), user("c")];
        let options = HistoryOptions {
            max_messages: 2,
            ..HistoryOptions::default()
        };
        assert_eq!(
            history_for_llm(&messages, &options),
            vec![user("b"), user("c")]
        );
    }

    #[test]
    fn treats_max_messages_of_zero_as_unlimited() {
        let messages = [user("a"), user("b"), user("c")];
        let options = HistoryOptions {
            max_messages: 0,
            ..HistoryOptions::default()
        };
        assert_eq!(history_for_llm(&messages, &options).len(), 3);
    }

    #[test]
    fn starts_at_the_first_user_message() {
        let messages = [system("stale prompt"), assistant("mid-turn"), user("go")];
        assert_eq!(
            history_for_llm(&messages, &HistoryOptions::default()),
            vec![user("go")]
        );
    }

    #[test]
    fn keeps_the_window_when_it_contains_no_user_message_at_all() {
        let messages = [calling(&["a"]), tool("a", "x")];
        assert_eq!(
            history_for_llm(&messages, &HistoryOptions::default()).len(),
            2
        );
    }

    #[test]
    fn aligns_after_trimming_to_the_first_user_message() {
        // Trimming to the user message strands the tool result that follows it,
        // so alignment has to run after the trim rather than before.
        let messages = [
            calling(&["a"]),
            user("interrupting"),
            tool("a", "stranded"),
            user("next"),
        ];
        let result = history_for_llm(&messages, &HistoryOptions::default());
        assert!(!has_orphaned_tool_result(&result));
        assert_eq!(result, vec![user("next")]);
    }

    #[test]
    fn truncates_long_tool_results_and_flags_them() {
        let long = "x".repeat(DEFAULT_MAX_TOOL_RESULT_CHARS + 100);
        let messages = [user("go"), calling(&["a"]), tool("a", &long)];
        let result = history_for_llm(&messages, &HistoryOptions::default());
        let truncated = tool_of(&result[2]);
        assert!(truncated.truncated);
        assert!(truncated.content.len() < long.len());
    }

    #[test]
    fn leaves_tool_results_within_the_cap_untouched() {
        let messages = [user("go"), calling(&["a"]), tool("a", "small")];
        let result = history_for_llm(&messages, &HistoryOptions::default());
        assert!(!tool_of(&result[2]).truncated);
    }

    #[test]
    fn disables_truncation_when_the_cap_is_zero() {
        let long = "x".repeat(20_000);
        let messages = [user("go"), calling(&["a"]), tool("a", &long)];
        let options = HistoryOptions {
            max_tool_result_chars: 0,
            ..HistoryOptions::default()
        };
        let result = history_for_llm(&messages, &options);
        assert_eq!(tool_of(&result[2]).content.len(), 20_000);
    }

    #[test]
    fn never_mutates_the_callers_messages() {
        let long = "x".repeat(200);
        let original = tool("a", &long);
        let messages = [user("go"), calling(&["a"]), original.clone()];
        let options = HistoryOptions {
            max_tool_result_chars: 10,
            ..HistoryOptions::default()
        };
        history_for_llm(&messages, &options);
        assert_eq!(messages[2], original);
        assert_eq!(tool_of(&messages[2]).content, long);
    }
}

mod session_history_tests {
    use super::*;

    /// A store that records what it was asked and answers from a list.
    struct Source {
        rows: Vec<ChatMessage>,
        asked: std::cell::RefCell<Vec<(String, MessageWindow)>>,
    }

    impl SessionHistorySource for Source {
        fn messages(
            &self,
            session_key: &str,
            window: &MessageWindow,
        ) -> darkwire_core::Result<Vec<ChatMessage>> {
            self.asked
                .borrow_mut()
                .push((session_key.to_owned(), *window));
            let rows = match window.limit {
                Some(limit) if window.from_end && self.rows.len() > limit => {
                    self.rows[self.rows.len() - limit..].to_vec()
                }
                Some(limit) => self.rows.iter().take(limit).cloned().collect(),
                None => self.rows.clone(),
            };
            Ok(rows)
        }
    }

    fn source(rows: Vec<ChatMessage>) -> Source {
        Source {
            rows,
            asked: std::cell::RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn asks_for_the_last_max_messages_and_windows_them() {
        let store = source(vec![user("a"), calling(&["x"]), tool("x", "r"), user("b")]);
        let options = HistoryOptions {
            max_messages: 2,
            ..HistoryOptions::default()
        };
        let history = session_history(&store, "s", &options).unwrap();
        assert_eq!(history, vec![user("b")]);
        assert_eq!(
            store.asked.borrow().as_slice(),
            &[(
                "s".to_owned(),
                MessageWindow {
                    after_seq: 0,
                    limit: Some(2),
                    from_end: true
                }
            )]
        );
    }

    #[test]
    fn reads_everything_when_there_is_no_limit() {
        let store = source(vec![user("a"), user("b")]);
        let options = HistoryOptions {
            max_messages: 0,
            ..HistoryOptions::default()
        };
        assert_eq!(
            session_history(&store, "s", &options).unwrap(),
            vec![user("a"), user("b")]
        );
        assert_eq!(store.asked.borrow()[0].1, MessageWindow::default());
    }

    #[test]
    fn an_unknown_session_is_an_empty_history() {
        let store = source(Vec::new());
        assert!(
            session_history(&store, "nope", &HistoryOptions::default())
                .unwrap()
                .is_empty()
        );
    }
}

/// The fixture's expected output holds two lone surrogates where the UTF-16
/// cut split a pair; a Rust string cannot, so they read as the replacement
/// character the port produces. Nothing else in the file is touched.
fn replace_lone_surrogates(text: &str) -> String {
    let escapes: Vec<(usize, u32)> = text
        .match_indices("\\u")
        .filter_map(|(index, _)| {
            let hex = text.get(index + 2..index + 6)?;
            u32::from_str_radix(hex, 16).ok().map(|code| (index, code))
        })
        .collect();
    let mut lone = Vec::new();
    let mut previous_high: Option<usize> = None;
    for (index, code) in escapes {
        if let Some(high) = previous_high.take() {
            if (0xDC00..=0xDFFF).contains(&code) && high + 6 == index {
                continue;
            }
            lone.push(high);
        }
        match code {
            0xD800..=0xDBFF => previous_high = Some(index),
            0xDC00..=0xDFFF => lone.push(index),
            _ => {}
        }
    }
    lone.extend(previous_high);
    let mut out = text.to_owned();
    for index in lone.into_iter().rev() {
        out.replace_range(index..index + 6, "\\ufffd");
    }
    out
}

#[test]
fn matches_the_windows_fixture() {
    let fixture: Value = serde_json::from_str(&replace_lone_surrogates(WINDOWS)).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 25);

    for case in cases {
        let name = case["name"].as_str().unwrap();
        let messages: Vec<ChatMessage> =
            serde_json::from_value(case["input"]["messages"].clone()).unwrap();
        let patch = &case["input"]["options"];
        let budget = |key: &str, default: usize| -> usize {
            patch
                .get(key)
                .and_then(Value::as_i64)
                .map_or(default, |value| usize::try_from(value).unwrap_or(0))
        };
        let options = HistoryOptions {
            max_messages: budget("maxMessages", HistoryOptions::default().max_messages),
            max_tool_result_chars: budget(
                "maxToolResultChars",
                HistoryOptions::default().max_tool_result_chars,
            ),
        };
        let produced = serde_json::to_value(history_for_llm(&messages, &options)).unwrap();
        assert_eq!(produced, case["output"], "case: {name}");
    }
}
