//! The queue that holds a correction until the loop can absorb it.

use darkwire_agent::steering::{STEERING_PREFIX, SteeringMessage, SteeringQueue, steering_text};

fn message(id: &str, content: &str) -> SteeringMessage {
    SteeringMessage {
        id: id.to_owned(),
        content: content.to_owned(),
        sender: "conn-1".to_owned(),
    }
}

#[test]
fn a_queue_is_per_session_because_one_loop_serves_them_all() {
    let queue = SteeringQueue::new();
    queue.open("web:1");
    queue.open("web:2");

    assert!(queue.push("web:1", message("a", "one")));
    assert!(queue.push("web:2", message("b", "two")));

    // A correction typed into one conversation must not surface in another.
    assert_eq!(queue.pending("web:1"), 1);
    assert_eq!(
        queue.peek("web:1").map(|m| m.content),
        Some("one".to_owned())
    );
    queue.pop("web:1");
    assert!(!queue.has_pending("web:1"));
    assert!(queue.has_pending("web:2"));
}

#[test]
fn nothing_is_accepted_for_a_session_no_turn_has_open() {
    // A steer sent as a turn ends, or before one starts: refused, so the
    // transport delivers it as a turn of its own.
    let queue = SteeringQueue::new();
    assert!(!queue.push("web:1", message("a", "late")));
    queue.open("web:1");
    assert!(queue.push("web:1", message("b", "in time")));
    let unread = queue.close("web:1");
    assert_eq!(unread, [message("b", "in time")]);
    assert!(!queue.push("web:1", message("c", "too late")));
}

#[test]
fn a_peeked_message_stays_until_it_is_popped() {
    // The loop writes before it pops, so a failed write leaves the steer for
    // the close to hand back.
    let queue = SteeringQueue::new();
    queue.open("web:1");
    queue.push("web:1", message("a", "one"));
    queue.push("web:1", message("b", "two"));
    assert_eq!(queue.peek("web:1").map(|m| m.id), Some("a".to_owned()));
    assert_eq!(queue.pending("web:1"), 2);
    queue.pop("web:1");
    assert_eq!(queue.peek("web:1").map(|m| m.id), Some("b".to_owned()));
    assert_eq!(queue.close("web:1"), [message("b", "two")]);
}

#[test]
fn the_text_in_history_is_prefixed_so_the_model_can_tell_it_apart() {
    let text = steering_text(&message("a", "use the other file"));
    assert!(text.starts_with(STEERING_PREFIX));
    assert!(text.ends_with("use the other file"));
}
