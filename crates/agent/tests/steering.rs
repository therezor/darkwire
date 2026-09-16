//! The queue that holds a correction until the loop can absorb it.

use darkwire_agent::steering::{
    MAX_PENDING_STEER, STEERING_PREFIX, SteeringMessage, SteeringQueue, steering_text,
};

#[test]
fn a_queue_is_per_session_because_one_loop_serves_them_all() {
    let queue = SteeringQueue::new();
    assert!(queue.is_empty());

    queue.push("web:1", "one", 10);
    queue.push("web:2", "two", 11);

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
fn draining_empties_the_queue_and_draining_nothing_is_not_an_error() {
    let queue = SteeringQueue::new();
    assert!(queue.drain("web:1").is_empty());

    queue.push("web:1", "one", 1);
    queue.push("web:1", "two", 2);
    let drained = queue.drain("web:1");
    assert_eq!(drained.len(), 2);
    // Oldest first, which is the order they were said in.
    assert_eq!(drained[0].content, "one");
    assert_eq!(drained[0].received_at_ms, 1);
    assert!(queue.drain("web:1").is_empty());
}

#[test]
fn overflow_drops_the_oldest_because_the_newest_is_what_the_user_awaits() {
    let queue = SteeringQueue::new();
    for n in 0..MAX_PENDING_STEER + 3 {
        queue.push("web:1", n.to_string(), 0);
    }

    let drained = queue.drain("web:1");
    // A bound rather than a courtesy: the queue fills from a socket and drains
    // from a loop that may be blocked in a slow tool.
    assert_eq!(drained.len(), MAX_PENDING_STEER);
    assert_eq!(drained[0].content, "3");
    assert_eq!(
        drained.last().unwrap().content,
        (MAX_PENDING_STEER + 2).to_string()
    );
}

#[test]
fn a_smaller_bound_is_honoured() {
    let queue = SteeringQueue::with_capacity(2);
    queue.push("web:1", "a", 0);
    queue.push("web:1", "b", 0);
    queue.push("web:1", "c", 0);

    let drained = queue.drain("web:1");
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].content, "b");
}

#[test]
fn clearing_forgets_a_session_whatever_it_held() {
    let queue = SteeringQueue::default();
    queue.push("web:1", "one", 0);
    queue.clear("web:1");

    assert!(!queue.has_pending("web:1"));
    assert!(queue.is_empty());
    // Clearing a session that never had a queue is not an error either.
    queue.clear("web:never");
}

#[test]
fn the_prefix_marks_an_interruption_rather_than_the_next_thing_said() {
    // Without it the model reads a mid-task user turn as a new request and
    // frequently abandons what it was doing.
    let text = steering_text(&SteeringMessage {
        content: "no, the other directory".to_owned(),
        received_at_ms: 0,
    });

    assert!(text.starts_with(STEERING_PREFIX));
    assert_eq!(
        text,
        format!("{STEERING_PREFIX}\n\nno, the other directory")
    );
}

#[test]
fn a_message_is_a_value_a_test_can_compare() {
    let message = SteeringMessage {
        content: "x".to_owned(),
        received_at_ms: 7,
    };
    assert_eq!(message.clone(), message);
    assert!(format!("{message:?}").contains("received_at_ms"));
}
