//! The queue that holds a correction until the loop can absorb it.

use darkwire_agent::steering::{
    MAX_PENDING_STEER, STEERING_PREFIX, SteeringMessage, SteeringQueue, steering_text,
};

fn message(id: &str, content: &str, received_at_ms: i64) -> SteeringMessage {
    SteeringMessage {
        id: id.to_owned(),
        content: content.to_owned(),
        received_at_ms,
    }
}

#[test]
fn a_queue_is_per_session_because_one_loop_serves_them_all() {
    let queue = SteeringQueue::new();
    assert!(queue.is_empty());
    queue.open("web:1");
    queue.open("web:2");

    assert!(queue.push("web:1", message("a", "one", 10)));
    assert!(queue.push("web:2", message("b", "two", 11)));

    assert_eq!(queue.len(), 2);
    assert!(queue.has_pending("web:1"));
    // A correction typed into one conversation must not surface in another.
    let drained = queue.drain("web:1");
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].content, "one");
    assert!(!queue.has_pending("web:1"));
    assert!(queue.has_pending("web:2"));
}

#[test]
fn nothing_is_accepted_for_a_session_no_turn_has_open() {
    // The case the hub exists to catch: a steer sent as a turn ends, or before
    // one starts. Refused, so the sender delivers it as a turn of its own.
    let queue = SteeringQueue::new();
    assert!(!queue.push("web:1", message("a", "late", 0)));
    assert!(!queue.has_pending("web:1"));
    assert!(queue.is_empty());
}

#[test]
fn draining_empties_the_queue_and_keeps_it_open() {
    let queue = SteeringQueue::new();
    queue.open("web:1");
    assert!(queue.drain("web:1").is_empty());

    queue.push("web:1", message("a", "one", 1));
    queue.push("web:1", message("b", "two", 2));
    let drained = queue.drain("web:1");
    assert_eq!(drained.len(), 2);
    // Oldest first, which is the order they were said in.
    assert_eq!(drained[0].content, "one");
    assert_eq!(drained[0].received_at_ms, 1);
    assert!(queue.drain("web:1").is_empty());
    // Still open: the next iteration of the same turn may be steered again.
    assert!(queue.push("web:1", message("c", "three", 3)));
}

#[test]
fn a_full_queue_refuses_rather_than_dropping_what_it_holds() {
    let queue = SteeringQueue::new();
    queue.open("web:1");
    for n in 0..MAX_PENDING_STEER {
        assert!(queue.push("web:1", message(&n.to_string(), &n.to_string(), 0)));
    }
    assert!(!queue.push("web:1", message("over", "over", 0)));

    let drained = queue.drain("web:1");
    assert_eq!(drained.len(), MAX_PENDING_STEER);
    assert_eq!(drained[0].content, "0");
}

#[test]
fn a_smaller_bound_is_honoured() {
    let queue = SteeringQueue::with_capacity(2);
    queue.open("web:1");
    assert!(queue.push("web:1", message("a", "a", 0)));
    assert!(queue.push("web:1", message("b", "b", 0)));
    assert!(!queue.push("web:1", message("c", "c", 0)));

    assert_eq!(queue.drain("web:1").len(), 2);
}

#[test]
fn closing_hands_back_what_no_iteration_read() {
    let queue = SteeringQueue::default();
    queue.open("web:1");
    queue.push("web:1", message("a", "one", 0));

    let unread = queue.close("web:1");
    assert_eq!(unread, vec![message("a", "one", 0)]);
    assert!(!queue.has_pending("web:1"));
    assert!(queue.is_empty());
    // Closed, so a push now is refused rather than kept for a later turn.
    assert!(!queue.push("web:1", message("b", "two", 0)));
    // Closing a session that was never open is not an error either.
    assert!(queue.close("web:never").is_empty());
}

#[test]
fn the_prefix_marks_an_interruption_rather_than_the_next_thing_said() {
    // Without it the model reads a mid-task user turn as a new request and
    // frequently abandons what it was doing.
    let text = steering_text(&message("a", "no, the other directory", 0));

    assert!(text.starts_with(STEERING_PREFIX));
    assert_eq!(
        text,
        format!("{STEERING_PREFIX}\n\nno, the other directory")
    );
}

#[test]
fn a_message_is_a_value_a_test_can_compare() {
    let message = message("a", "x", 7);
    assert_eq!(message.clone(), message);
    assert!(format!("{message:?}").contains("received_at_ms"));
}
