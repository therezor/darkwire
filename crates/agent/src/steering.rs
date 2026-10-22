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
//!
//! A session's queue is **open** only while a turn on it can still read it.
//! A push the queue refuses, because no turn is open or the queue is full, is
//! the caller's to deliver some other way, which is what keeps a steer from
//! being lost: the hub runs it as the next turn instead.

use std::collections::{HashMap, VecDeque};

use parking_lot::Mutex;

pub use darkwire_core::messages::STEERING_PREFIX;

/// How many pending messages one session may hold.
///
/// A bound rather than a courtesy: the queue fills from a socket and drains
/// from a loop that may be blocked in a slow tool, so without one a client that
/// sends faster than the loop iterates grows it without limit.
pub const MAX_PENDING_STEER: usize = 16;

/// One correction, waiting for the next iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteeringMessage {
    /// The sender's id for it, echoed when the loop reads it.
    pub id: String,
    /// What the user typed.
    pub content: String,
    /// Wall-clock epoch milliseconds, from the caller's clock.
    pub received_at_ms: i64,
}

/// Corrections waiting for the turns they belong to.
///
/// Shared behind an `Arc` by a loop and whatever transport pushes into it, so
/// the interior mutability is the type's rather than every caller's. A key is
/// present exactly while its session is open.
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

    /// Starts accepting steering for `session_key`. A turn calls this before
    /// anything can steer it.
    pub fn open(&self, session_key: &str) {
        self.queues
            .lock()
            .entry(session_key.to_owned())
            .or_default();
    }

    /// Stops accepting steering for `session_key`, and hands back what no
    /// iteration read.
    pub fn close(&self, session_key: &str) -> Vec<SteeringMessage> {
        self.queues
            .lock()
            .remove(session_key)
            .map(Vec::from)
            .unwrap_or_default()
    }

    /// Queues a message for the next iteration of `session_key`'s turn.
    ///
    /// `false` when no turn is open to read it, or when the queue is full. The
    /// newest correction is the one the user is waiting on, so nothing already
    /// queued is dropped to make room: the refusal goes back to the sender.
    pub fn push(&self, session_key: &str, message: SteeringMessage) -> bool {
        let mut queues = self.queues.lock();
        let Some(queue) = queues.get_mut(session_key) else {
            return false;
        };
        if queue.len() >= self.max_pending {
            tracing::warn!(
                session_key,
                max_pending = self.max_pending,
                "steering queue full, refused a message"
            );
            return false;
        }
        queue.push_back(message);
        true
    }

    /// Whether anything is waiting. Checked before the loop ends a turn.
    pub fn has_pending(&self, session_key: &str) -> bool {
        self.queues
            .lock()
            .get(session_key)
            .is_some_and(|queue| !queue.is_empty())
    }

    /// Takes everything queued for the session. The session stays open.
    pub fn drain(&self, session_key: &str) -> Vec<SteeringMessage> {
        self.queues
            .lock()
            .get_mut(session_key)
            .map(|queue| queue.drain(..).collect())
            .unwrap_or_default()
    }

    /// Sessions currently open.
    pub fn len(&self) -> usize {
        self.queues.lock().len()
    }

    /// Whether no session is open.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The prefixed text as it enters history.
pub fn steering_text(message: &SteeringMessage) -> String {
    format!("{STEERING_PREFIX}\n\n{}", message.content)
}
