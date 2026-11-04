//! Mid-turn steering.
//!
//! A turn that calls tools can run for minutes, and for most of that time the
//! user is watching it do the wrong thing. Steering is the answer: what they
//! type lands in this queue, the loop writes it at the top of the next
//! iteration as a user message, and the model sees the correction before its
//! next decision.
//!
//! Two details make it work rather than merely exist:
//!
//!  - **Read at the top of the iteration, not at the point of arrival.** The
//!    loop is inside a provider request or a tool call for nearly all of its
//!    wall-clock time, and neither can absorb a new message. The queue holds
//!    the gap.
//!  - **A steer that arrives while the model is composing its final answer
//!    makes the loop carry on rather than end**, so the correction is answered
//!    rather than met with silence.
//!
//! The queue is keyed by session because one loop serves every session on the
//! instance. **The transport owns it**: it opens a session's queue before the
//! turn starts and closes it after the turn ends, and whatever `close` hands
//! back was never read, so the transport runs it as the next turn. The loop
//! only reads.

use std::collections::{HashMap, VecDeque};

use parking_lot::Mutex;

pub use darkwire_core::messages::STEERING_PREFIX;

/// One correction, waiting for the next iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteeringMessage {
    /// The sender's id for it, echoed when the loop writes it.
    pub id: String,
    /// What the user typed.
    pub content: String,
    /// Who sent it, in the transport's terms, so an unread one runs as that
    /// sender's next turn.
    pub sender: String,
}

/// Corrections waiting for the turns they belong to. A key is present exactly
/// while its session is open.
#[derive(Debug, Default)]
pub struct SteeringQueue {
    queues: Mutex<HashMap<String, VecDeque<SteeringMessage>>>,
}

impl SteeringQueue {
    /// An empty queue.
    pub fn new() -> SteeringQueue {
        SteeringQueue::default()
    }

    /// Starts accepting steering for `session_key`.
    pub fn open(&self, session_key: &str) {
        self.queues
            .lock()
            .entry(session_key.to_owned())
            .or_default();
    }

    /// Stops accepting steering for `session_key`, and hands back what was
    /// never read.
    pub fn close(&self, session_key: &str) -> Vec<SteeringMessage> {
        self.queues
            .lock()
            .remove(session_key)
            .map(Vec::from)
            .unwrap_or_default()
    }

    /// Queues a message for the next iteration of `session_key`'s turn.
    /// `false` when no turn is open to read it.
    pub fn push(&self, session_key: &str, message: SteeringMessage) -> bool {
        let mut queues = self.queues.lock();
        let Some(queue) = queues.get_mut(session_key) else {
            return false;
        };
        queue.push_back(message);
        true
    }

    /// How many are waiting on `session_key`.
    pub fn pending(&self, session_key: &str) -> usize {
        self.queues.lock().get(session_key).map_or(0, VecDeque::len)
    }

    /// Whether anything is waiting. Checked before the loop ends a turn.
    pub fn has_pending(&self, session_key: &str) -> bool {
        self.pending(session_key) > 0
    }

    /// The oldest waiting message, left in place until [`Self::pop`], so one
    /// that fails to be written is still there when the turn closes.
    pub fn peek(&self, session_key: &str) -> Option<SteeringMessage> {
        self.queues.lock().get(session_key)?.front().cloned()
    }

    /// Removes the oldest waiting message, once it is written.
    pub fn pop(&self, session_key: &str) {
        if let Some(queue) = self.queues.lock().get_mut(session_key) {
            queue.pop_front();
        }
    }
}

/// The prefixed text as it enters history.
pub fn steering_text(message: &SteeringMessage) -> String {
    format!("{STEERING_PREFIX}\n\n{}", message.content)
}
