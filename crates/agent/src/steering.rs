//! Mid-turn steering.
//!
//! A turn that calls tools can run for minutes, and for most of that time the
//! user is watching it do the wrong thing. Steering is the answer: what they
//! type lands in this queue, the loop drains it at the top of the next
//! iteration and appends it to history as a user message, and the model sees
//! the correction before its next decision.
//!
//! Two details make it work rather than merely exist:
//!
//!  - **Drained at the top of the iteration, not at the point of arrival.**
//!    The loop is inside a provider request or a tool call for nearly all of
//!    its wall-clock time, and neither can absorb a new message. The queue is
//!    what holds the gap.
//!  - **A steering message that arrives while the model is composing its final
//!    answer makes the loop carry on rather than end.** Otherwise the turn
//!    ends, the queue is discarded, and the user's correction is answered by
//!    silence — the failure is invisible, because from the outside a completed
//!    turn looks like a completed turn.
//!
//! The queue is keyed by session because one loop serves every session on the
//! instance, and a correction typed into one conversation must not surface in
//! another.

use std::collections::{HashMap, VecDeque};

use parking_lot::Mutex;

/// Marks the message as an interruption rather than the next thing the user
/// said.
///
/// Without it the model reads a mid-task user turn as a new request and
/// frequently abandons what it was doing; with it, the common case — "no, the
/// other directory" — is understood as a correction to the task in flight.
pub const STEERING_PREFIX: &str = "[Steering: sent by the user while this task was running]";

/// How many pending messages one session may hold.
///
/// A bound rather than a courtesy: the queue fills from a socket and drains
/// from a loop that may be blocked in a slow tool, so without one a client that
/// sends faster than the loop iterates grows it without limit.
pub const MAX_PENDING_STEER: usize = 16;

/// One correction, waiting for the next iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteeringMessage {
    /// What the user typed.
    pub content: String,
    /// Wall-clock epoch milliseconds, from the caller's clock.
    pub received_at_ms: i64,
}

/// Corrections waiting for the turns they belong to.
///
/// Shared behind an `Arc` by a loop and whatever transport pushes into it, so
/// the interior mutability is the type's rather than every caller's.
#[derive(Debug)]
pub struct SteeringQueue {
    queues: Mutex<HashMap<String, VecDeque<SteeringMessage>>>,
    max_pending: usize,
}

impl Default for SteeringQueue {
    fn default() -> SteeringQueue {
        SteeringQueue::new()
    }
}

impl SteeringQueue {
    /// An empty queue holding at most [`MAX_PENDING_STEER`] per session.
    pub fn new() -> SteeringQueue {
        SteeringQueue::with_capacity(MAX_PENDING_STEER)
    }

    /// An empty queue holding at most `max_pending` per session.
    pub fn with_capacity(max_pending: usize) -> SteeringQueue {
        SteeringQueue {
            queues: Mutex::new(HashMap::new()),
            max_pending,
        }
    }

    /// Queues a message for the next iteration of `session_key`'s turn.
    ///
    /// Overflow drops the *oldest* pending message. The newest correction is
    /// the one the user is waiting on; discarding it to preserve a stale one
    /// inverts the point of steering.
    pub fn push(&self, session_key: &str, content: impl Into<String>, received_at_ms: i64) {
        let mut queues = self.queues.lock();
        let queue = queues.entry(session_key.to_owned()).or_default();
        queue.push_back(SteeringMessage {
            content: content.into(),
            received_at_ms,
        });
        while queue.len() > self.max_pending {
            queue.pop_front();
            tracing::warn!(
                session_key,
                max_pending = self.max_pending,
                "steering queue full, dropped oldest message"
            );
        }
    }

    /// Whether anything is waiting. Checked before the loop ends a turn.
    pub fn has_pending(&self, session_key: &str) -> bool {
        self.queues
            .lock()
            .get(session_key)
            .is_some_and(|queue| !queue.is_empty())
    }

    /// Takes everything queued for the session and empties it.
    pub fn drain(&self, session_key: &str) -> Vec<SteeringMessage> {
        let mut queues = self.queues.lock();
        match queues.get(session_key) {
            None => Vec::new(),
            Some(queue) if queue.is_empty() => Vec::new(),
            Some(_) => queues
                .remove(session_key)
                .map(Vec::from)
                .unwrap_or_default(),
        }
    }

    /// Forgets a session's queue. The loop calls this when a turn ends.
    pub fn clear(&self, session_key: &str) {
        self.queues.lock().remove(session_key);
    }

    /// Sessions currently holding pending messages.
    pub fn len(&self) -> usize {
        self.queues.lock().len()
    }

    /// Whether no session is holding anything.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The prefixed text as it enters history.
pub fn steering_text(message: &SteeringMessage) -> String {
    format!("{STEERING_PREFIX}\n\n{}", message.content)
}
