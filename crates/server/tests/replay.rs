//! The per-session replay ring: what a reconnecting tab still needs, and when
//! the buffer has to admit it cannot say.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use darkwire_protocol::ws::{AssistantDelta, AssistantDeltaTag, PongTag, Sequenced, ServerMessage};
use darkwire_server::replay::ReplayBuffer;

fn delta(seq: u64, text: &str) -> ServerMessage {
    ServerMessage::AssistantDelta(Sequenced {
        seq,
        event: AssistantDelta {
            tag: AssistantDeltaTag,
            turn_id: "turn-1".to_owned(),
            text: text.to_owned(),
        },
    })
}

fn seqs(messages: &[ServerMessage]) -> Vec<u64> {
    messages.iter().filter_map(ServerMessage::seq).collect()
}

#[test]
fn retains_up_to_its_capacity_dropping_the_oldest() {
    let mut buffer = ReplayBuffer::new(3);
    for seq in 1..=5 {
        buffer.push(delta(seq, "x"));
    }

    assert_eq!(buffer.size(), 3);
    assert_eq!(buffer.capacity(), 3);
    assert_eq!(seqs(&buffer.after(0).messages), [3, 4, 5]);
}

#[test]
fn replays_exactly_the_tail_after_a_mid_stream_seq() {
    let mut buffer = ReplayBuffer::new(16);
    for seq in 1..=6 {
        buffer.push(delta(seq, &format!("chunk-{seq}")));
    }

    let slice = buffer.after(3);
    assert!(slice.complete);
    assert_eq!(
        slice.messages,
        [
            delta(4, "chunk-4"),
            delta(5, "chunk-5"),
            delta(6, "chunk-6")
        ]
    );
}

#[test]
fn reports_a_gap_rather_than_a_tail_that_starts_late() {
    let mut buffer = ReplayBuffer::new(2);
    for seq in 1..=5 {
        buffer.push(delta(seq, "x"));
    }

    // The client wants everything after 1; the buffer starts at 4.
    let slice = buffer.after(1);
    assert!(!slice.complete);
    assert_eq!(seqs(&slice.messages), [4, 5]);
}

#[test]
fn treats_a_client_that_has_seen_everything_as_complete_and_empty() {
    let mut buffer = ReplayBuffer::new(8);
    buffer.push(delta(1, "x"));
    buffer.push(delta(2, "x"));

    let slice = buffer.after(2);
    assert!(slice.messages.is_empty());
    assert!(slice.complete);
}

#[test]
fn treats_a_client_ahead_of_the_buffer_as_a_gap() {
    // A restart: the counter began again, and this client's history predates it.
    let mut buffer = ReplayBuffer::new(8);
    buffer.push(delta(1, "x"));

    let slice = buffer.after(57);
    assert!(slice.messages.is_empty());
    assert!(!slice.complete);
}

#[test]
fn keeps_counting_with_a_capacity_of_zero_and_never_claims_coverage() {
    let mut buffer = ReplayBuffer::new(0);
    buffer.push(delta(1, "x"));
    buffer.push(delta(2, "x"));

    assert_eq!(buffer.size(), 0);
    assert_eq!(buffer.last_seq(), 2);
    assert!(!buffer.after(1).complete);
    assert!(buffer.after(2).complete);
}

#[test]
fn forgets_its_entries_on_clear_but_not_where_the_sequence_is() {
    let mut buffer = ReplayBuffer::new(4);
    buffer.push(delta(1, "x"));
    buffer.push(delta(2, "x"));
    buffer.clear();

    assert_eq!(buffer.size(), 0);
    assert_eq!(buffer.last_seq(), 2);
    assert!(!buffer.after(1).complete);
}

#[test]
fn ignores_a_connection_level_event_that_carries_no_seq() {
    // `connected`, `pong` and `error` are not part of any session's replayable
    // history, so retaining one would invent a position it never held.
    let mut buffer = ReplayBuffer::new(4);
    buffer.push(delta(1, "x"));
    buffer.push(ServerMessage::Pong(darkwire_protocol::ws::PongEvent {
        tag: PongTag,
        server_time_ms: 1_700_000_000_000,
    }));

    assert_eq!(buffer.size(), 1);
    assert_eq!(buffer.last_seq(), 1);
}
