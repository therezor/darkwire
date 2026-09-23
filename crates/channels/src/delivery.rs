//! One channel's delivery queue: strictly ordered, bounded, and shed with care.
//!
//! The bus in front of it is bounded, but a channel that has stopped taking
//! messages (a stuck extension, a Telegram request waiting out its timeout)
//! would otherwise let this queue grow for as long as the hub keeps talking.
//! So it is bounded twice:
//!
//!  - **At the bound**, only what loses nothing is shed. A progress message is
//!    the answer so far, so a newer one (or the reply) for the same turn makes
//!    it redundant. An update settles an approval card, and one whose card has
//!    already left the queue is the one message the channel can do without.
//!    Everything else is kept past the bound, with a warning.
//!  - **At the hard cap**, something that matters has to go, because memory is
//!    the thing being protected. The oldest progress goes first, then the
//!    oldest notice, and every loss is logged.
//!
//! Nothing is ever reordered: shedding removes a message, and a replacement is
//! pushed at the back, so a notice sent before a newer progress still arrives
//! before it.

use std::collections::VecDeque;

use darkwire_core::message_bus::{OutboundKind, OutboundMessage};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::projection::{APPROVAL_METADATA_KEY, APPROVAL_SETTLED_METADATA_KEY};

/// Where a draft's turn id travels on an outbound message.
pub(crate) const TURN_ID_METADATA_KEY: &str = "turnId";

/// How many messages one channel may have waiting before shedding starts.
pub const DELIVERY_QUEUE_BOUND: usize = 256;

/// How many it may hold at all. Past this, messages that matter are dropped.
pub const DELIVERY_QUEUE_HARD_CAP: usize = 4 * DELIVERY_QUEUE_BOUND;

struct State {
    items: VecDeque<OutboundMessage>,
    /// Set once the queue crosses its bound, so a stuck channel logs one line
    /// rather than one per message. Cleared when it drains back under.
    over: bool,
}

pub(crate) struct DeliveryQueue {
    channel_id: String,
    state: Mutex<State>,
    ready: Notify,
    closed: CancellationToken,
}

fn call_id<'a>(message: &'a OutboundMessage, key: &str) -> Option<&'a str> {
    message.metadata.get(key)?.get("callId")?.as_str()
}

/// The approval a card asks for, when this message is one.
fn card_of(message: &OutboundMessage) -> Option<&str> {
    if message.kind != OutboundKind::Notice {
        return None;
    }
    call_id(message, APPROVAL_METADATA_KEY)
}

/// The approval an update settles, when this message is one.
fn settles(message: &OutboundMessage) -> Option<&str> {
    if message.kind != OutboundKind::Update {
        return None;
    }
    call_id(message, APPROVAL_SETTLED_METADATA_KEY)
}

fn turn_of(message: &OutboundMessage) -> Option<&Value> {
    message.metadata.get(TURN_ID_METADATA_KEY)
}

fn same_turn(a: &OutboundMessage, b: &OutboundMessage) -> bool {
    a.session_key == b.session_key && turn_of(a) == turn_of(b)
}

/// An update is kept while its card is still waiting to be sent: without it,
/// that card would go out and stay open forever.
fn is_sheddable_update(items: &VecDeque<OutboundMessage>, message: &OutboundMessage) -> bool {
    let Some(settled) = settles(message) else {
        return false;
    };
    !items
        .iter()
        .any(|queued| queued.session_key == message.session_key && card_of(queued) == Some(settled))
}

impl DeliveryQueue {
    pub(crate) fn new(channel_id: &str) -> DeliveryQueue {
        DeliveryQueue {
            channel_id: channel_id.to_owned(),
            state: Mutex::new(State {
                items: VecDeque::new(),
                over: false,
            }),
            ready: Notify::new(),
            closed: CancellationToken::new(),
        }
    }

    /// Queues one message behind the rest. Never waits.
    pub(crate) fn push(&self, message: OutboundMessage) {
        {
            let mut state = self.state.lock();
            if state.items.len() >= DELIVERY_QUEUE_BOUND && !self.shed(&mut state.items, &message) {
                return;
            }
            state.items.push_back(message);
            let queued = state.items.len();
            if queued > DELIVERY_QUEUE_BOUND && !state.over {
                state.over = true;
                tracing::warn!(
                    channel = %self.channel_id,
                    queued,
                    "a channel is not keeping up; holding its messages past the queue bound"
                );
            }
            if queued > DELIVERY_QUEUE_HARD_CAP {
                self.evict(&mut state.items);
            }
        }
        self.ready.notify_one();
    }

    /// Makes room at the bound without losing anything. Answers whether the
    /// incoming message should still be queued.
    fn shed(&self, items: &mut VecDeque<OutboundMessage>, incoming: &OutboundMessage) -> bool {
        if matches!(incoming.kind, OutboundKind::Progress | OutboundKind::Reply) {
            let before = items.len();
            items.retain(|queued| {
                !(queued.kind == OutboundKind::Progress && same_turn(queued, incoming))
            });
            if items.len() < before {
                return true;
            }
        }
        let oldest = items
            .iter()
            .position(|queued| is_sheddable_update(items, queued));
        if let Some(index) = oldest {
            items.remove(index);
            tracing::debug!(channel = %self.channel_id, "shed an approval update");
            return true;
        }
        if is_sheddable_update(items, incoming) {
            tracing::debug!(channel = %self.channel_id, "shed an approval update");
            return false;
        }
        true
    }

    /// Drops one message at the hard cap: the oldest progress, then the oldest
    /// notice, then the oldest card, then the oldest of anything.
    fn evict(&self, items: &mut VecDeque<OutboundMessage>) {
        let index = items
            .iter()
            .position(|queued| queued.kind == OutboundKind::Progress)
            .or_else(|| {
                items.iter().position(|queued| {
                    queued.kind == OutboundKind::Notice && card_of(queued).is_none()
                })
            })
            .or_else(|| items.iter().position(|queued| card_of(queued).is_some()))
            .unwrap_or(0);
        let Some(dropped) = items.remove(index) else {
            return;
        };
        // An update for a card that will never be sent has nothing to edit.
        if let Some(card) = card_of(&dropped) {
            items.retain(|queued| {
                !(queued.session_key == dropped.session_key && settles(queued) == Some(card))
            });
        }
        tracing::warn!(
            channel = %self.channel_id,
            queued = items.len(),
            kind = ?dropped.kind,
            "a channel's queue is full; dropped its oldest message"
        );
    }

    /// The next message, or `None` once the queue is closed and empty.
    pub(crate) async fn next(&self) -> Option<OutboundMessage> {
        loop {
            {
                let mut state = self.state.lock();
                if let Some(message) = state.items.pop_front() {
                    if state.items.len() <= DELIVERY_QUEUE_BOUND {
                        state.over = false;
                    }
                    return Some(message);
                }
            }
            if self.closed.is_cancelled() {
                return None;
            }
            tokio::select! {
                () = self.ready.notified() => {}
                () = self.closed.cancelled() => {}
            }
        }
    }

    /// Ends the queue once what it holds has been taken.
    pub(crate) fn close(&self) {
        self.closed.cancel();
    }
}
