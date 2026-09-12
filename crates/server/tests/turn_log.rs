//! The running turn, kept whole: what belongs in it, how a run of deltas
//! collapses to one entry, and what happens when it outgrows its budget.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_protocol::messages::StopReason;
use ghostai_protocol::ws::{
    AssistantDelta, AssistantDeltaTag, ContextUsage, ContextUsageTag, NestedAgentEvent,
    NotificationBody, NotificationLevel, NotificationTag, ReasoningDelta, ReasoningDeltaTag,
    Sequenced, ServerMessage, SessionStatus, SessionStatusTag, SubagentEventBody, SubagentEventTag,
    ToolResult, ToolResultTag, TurnEnd, TurnEndTag, TurnStart, TurnStartTag,
};
use ghostai_server::turn_log::TurnLog;
use indexmap::IndexMap;

const TURN: &str = "turn-1";
const SESSION: &str = "web:1";
const BIG: usize = 1024 * 1024;

fn start(seq: u64, turn_id: &str) -> ServerMessage {
    ServerMessage::TurnStart(Sequenced {
        seq,
        event: TurnStart {
            tag: TurnStartTag,
            session_key: SESSION.to_owned(),
            turn_id: turn_id.to_owned(),
            first_seq: None,
            agent_id: "default".to_owned(),
            model: "test-model".to_owned(),
            provider: "test".to_owned(),
        },
    })
}

fn end(seq: u64, turn_id: &str) -> ServerMessage {
    ServerMessage::TurnEnd(Sequenced {
        seq,
        event: TurnEnd {
            tag: TurnEndTag,
            turn_id: turn_id.to_owned(),
            stop_reason: StopReason::Complete,
            usage: None,
            iterations: 1,
            elapsed_ms: None,
            generation_ms: None,
            generation_tokens: None,
            first_token_ms: None,
            first_seq: None,
            last_seq: None,
        },
    })
}

fn delta(seq: u64, text: &str, turn_id: &str) -> ServerMessage {
    ServerMessage::AssistantDelta(Sequenced {
        seq,
        event: AssistantDelta {
            tag: AssistantDeltaTag,
            turn_id: turn_id.to_owned(),
            text: text.to_owned(),
        },
    })
}

fn reasoning(seq: u64, text: &str) -> ServerMessage {
    ServerMessage::ReasoningDelta(Sequenced {
        seq,
        event: ReasoningDelta {
            tag: ReasoningDeltaTag,
            turn_id: TURN.to_owned(),
            text: text.to_owned(),
        },
    })
}

fn result(seq: u64, content: &str) -> ServerMessage {
    ServerMessage::ToolResult(Sequenced {
        seq,
        event: ToolResult {
            tag: ToolResultTag,
            turn_id: TURN.to_owned(),
            call_id: "call-1".to_owned(),
            ok: true,
            content: content.to_owned(),
            truncated: false,
            duration_ms: 4,
        },
    })
}

fn nested(seq: u64, text: &str, call_id: &str, parent_session_key: &str) -> ServerMessage {
    ServerMessage::Subagent(Sequenced {
        seq,
        event: SubagentEventBody {
            tag: SubagentEventTag,
            turn_id: TURN.to_owned(),
            parent_session_key: parent_session_key.to_owned(),
            parent_call_id: call_id.to_owned(),
            agent_id: "researcher".to_owned(),
            label: "Researcher".to_owned(),
            session_key: "subagent:1".to_owned(),
            depth: 1,
            event: NestedAgentEvent::AssistantDelta(AssistantDelta {
                tag: AssistantDeltaTag,
                turn_id: "child-turn".to_owned(),
                text: text.to_owned(),
            }),
        },
    })
}

/// The text of each retained frame, or its kind when it has none.
fn texts(log: &TurnLog) -> Vec<String> {
    log.frames()
        .iter()
        .map(|frame| match frame {
            ServerMessage::AssistantDelta(e) => e.event.text.clone(),
            ServerMessage::ReasoningDelta(e) => e.event.text.clone(),
            ServerMessage::Subagent(e) => match &e.event.event {
                NestedAgentEvent::AssistantDelta(body) => body.text.clone(),
                NestedAgentEvent::ReasoningDelta(body) => body.text.clone(),
                other => other.tag().to_owned(),
            },
            ServerMessage::TurnStart(_) => "turn.start".to_owned(),
            ServerMessage::ToolResult(_) => "tool.result".to_owned(),
            other => format!("{other:?}"),
        })
        .collect()
}

#[test]
fn holds_nothing_until_a_turn_starts() {
    let mut log = TurnLog::new(BIG);
    log.push(&delta(1, "orphan", TURN));

    assert_eq!(log.open_turn_id(), None);
    assert!(!log.complete());
    assert_eq!(log.size(), 0);
}

#[test]
fn holds_the_open_turn_from_its_start_and_forgets_it_at_its_end() {
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&delta(2, "hello", TURN));

    assert_eq!(log.open_turn_id(), Some(TURN));
    assert!(log.complete());
    assert_eq!(texts(&log), ["turn.start", "hello"]);

    log.push(&end(3, TURN));
    assert_eq!(log.open_turn_id(), None);
    assert!(!log.complete());
    assert_eq!(log.size(), 0);
}

#[test]
fn starts_over_on_the_next_turn_rather_than_accumulating() {
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&delta(2, "first", TURN));
    log.push(&end(3, TURN));
    log.push(&start(4, "turn-2"));
    log.push(&delta(5, "second", "turn-2"));

    assert_eq!(log.open_turn_id(), Some("turn-2"));
    assert_eq!(texts(&log), ["turn.start", "second"]);
}

#[test]
fn ignores_frames_belonging_to_another_turn() {
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&delta(2, "mine", TURN));
    log.push(&delta(3, "someone else's", "turn-2"));

    assert_eq!(texts(&log), ["turn.start", "mine"]);
}

#[test]
fn keeps_a_turn_open_when_another_turns_end_arrives() {
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&delta(2, "mine", TURN));
    log.push(&end(3, "turn-2"));

    assert_eq!(log.open_turn_id(), Some(TURN));
    assert_eq!(texts(&log), ["turn.start", "mine"]);
}

#[test]
fn keeps_the_session_scoped_frames_out_so_a_replay_raises_no_second_toast() {
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&ServerMessage::SessionStatus(Sequenced {
        seq: 2,
        event: SessionStatus {
            tag: SessionStatusTag,
            session_key: SESSION.to_owned(),
            busy: true,
            queue_depth: 0,
            workspace_id: "default".to_owned(),
            turn_id: Some(TURN.to_owned()),
        },
    }));
    log.push(&ServerMessage::ContextUsage(Sequenced {
        seq: 3,
        event: ContextUsage {
            tag: ContextUsageTag,
            session_key: SESSION.to_owned(),
            estimated_tokens: 10,
            context_window_tokens: 100,
            breakdown: IndexMap::new(),
        },
    }));
    log.push(&ServerMessage::Notification(Sequenced {
        seq: 4,
        event: NotificationBody {
            tag: NotificationTag,
            id: "n1".to_owned(),
            title: "done".to_owned(),
            body: String::new(),
            level: NotificationLevel::Info,
            created_at_ms: 0,
            session_key: None,
            job_id: None,
        },
    }));

    assert_eq!(texts(&log), ["turn.start"]);
}

#[test]
fn merges_a_run_of_deltas_into_one_entry_carrying_the_later_seq() {
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&delta(2, "one ", TURN));
    log.push(&delta(3, "two ", TURN));
    log.push(&delta(4, "three", TURN));

    assert_eq!(texts(&log), ["turn.start", "one two three"]);
    // The later seq, so a client's cursor is not left behind what it rendered.
    assert_eq!(
        log.frames().last().and_then(ServerMessage::seq),
        Some(4),
        "the merged entry reports the last seq it absorbed"
    );
}

#[test]
fn does_not_merge_across_a_change_of_kind() {
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&delta(2, "answer", TURN));
    log.push(&reasoning(3, "thinking"));
    log.push(&delta(4, "more", TURN));

    assert_eq!(texts(&log), ["turn.start", "answer", "thinking", "more"]);
}

#[test]
fn does_not_merge_a_turns_own_text_into_its_subagents() {
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&delta(2, "parent ", TURN));
    log.push(&nested(3, "child ", "call-1", SESSION));
    log.push(&nested(4, "more", "call-1", SESSION));
    log.push(&delta(5, "again", TURN));

    assert_eq!(
        texts(&log),
        ["turn.start", "parent ", "child more", "again"]
    );
}

#[test]
fn keeps_two_delegations_apart_even_when_they_share_a_call_id() {
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&nested(2, "first ", "call-1", SESSION));
    log.push(&nested(3, "second", "call-1", "subagent:1"));

    assert_eq!(texts(&log), ["turn.start", "first ", "second"]);
}

#[test]
fn drops_everything_and_says_so_once_past_its_budget() {
    let mut log = TurnLog::new(256);
    log.push(&start(1, TURN));
    log.push(&delta(2, "a", TURN));
    assert!(log.complete());

    log.push(&result(3, &"x".repeat(512)));

    assert!(!log.complete());
    assert_eq!(log.size(), 0);
    assert_eq!(log.retained_bytes(), 0);
    // Still knows which turn is running — it just cannot describe it.
    assert_eq!(log.open_turn_id(), Some(TURN));

    // And it stays dropped for the rest of the turn.
    log.push(&delta(4, "b", TURN));
    assert_eq!(log.size(), 0);

    log.push(&end(5, TURN));
    log.push(&start(6, "turn-2"));
    assert!(log.complete());
}

#[test]
fn retains_nothing_at_all_when_the_budget_is_zero() {
    let mut log = TurnLog::new(0);
    log.push(&start(1, TURN));
    log.push(&delta(2, "hello", TURN));

    assert_eq!(log.open_turn_id(), Some(TURN));
    assert!(!log.complete());
    assert_eq!(log.size(), 0);
}

#[test]
fn charges_a_merged_delta_for_its_text_not_for_a_new_entry() {
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&delta(2, "abcd", TURN));
    let after_first = log.retained_bytes();
    log.push(&delta(3, "efgh", TURN));

    assert_eq!(log.retained_bytes() - after_first, 4);
}

#[test]
fn charges_text_in_the_units_the_client_counts() {
    // A budget measured in UTF-8 bytes would be a different budget for the same
    // answer depending on the language it is written in.
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&delta(2, "ab", TURN));
    let after_first = log.retained_bytes();
    // One astral-plane character is two UTF-16 code units and four UTF-8 bytes.
    log.push(&delta(3, "\u{1F600}", TURN));

    assert_eq!(log.retained_bytes() - after_first, 2);
}

#[test]
fn forgets_the_open_turn_when_the_conversation_moves_under_it() {
    let mut log = TurnLog::new(BIG);
    log.push(&start(1, TURN));
    log.push(&delta(2, "hello", TURN));
    log.clear();

    assert_eq!(log.open_turn_id(), None);
    assert!(!log.complete());
    assert!(log.frames().is_empty());
}
