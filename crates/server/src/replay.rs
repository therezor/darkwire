//! The per-session replay ring.
//!
//! A tab that refreshes mid-turn has to come back to the turn it left, not to a
//! blank transcript with a spinner nobody will ever clear. The server therefore
//! keeps the last `server.replayBufferSize` events it emitted for a session, and
//! a reconnecting client asks for everything after the last `seq` it rendered.
//!
//! Three properties are what make it usable, and each one is a decision:
//!
//!  - **It stores emitted events, not stored messages.** An in-flight turn has
//!    no persisted assistant message yet — its text exists only as deltas — so
//!    replaying from the session store would rebuild everything except the part
//!    the user is watching.
//!  - **It answers with a `complete` flag rather than a best guess.** Handing
//!    back a tail that starts at `last_seq + 4` looks like a successful replay
//!    and silently loses three events. The gap is detectable here and nowhere
//!    else, so it is reported here.
//!  - **A client ahead of the buffer is a gap too.** After a restart the counter
//!    starts at zero again, and a client resuming at `seq 57` would otherwise be
//!    told it had missed nothing. Its history is gone; that is precisely the case
//!    `complete: false` exists for.

use ghostai_protocol::ws::ServerMessage;

/// What a client resuming at some `seq` still needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplaySlice {
    /// Everything retained after the requested `seq`, in emission order.
    pub messages: Vec<ServerMessage>,
    /// False when the buffer could not account for every `seq` after it.
    pub complete: bool,
}

/// A bounded, in-order window over one session's emitted events.
///
/// Only events that carry a `seq` belong here: `connected`, `pong` and `error`
/// are connection-level, and [`ServerMessage::seq`] is what tells the two apart.
/// A message without one is refused by [`ReplayBuffer::push`] rather than stored
/// with a number invented for it.
#[derive(Debug)]
pub struct ReplayBuffer {
    max_entries: usize,
    entries: Vec<ServerMessage>,
    /// The highest `seq` ever pushed, tracked separately from the entries.
    ///
    /// A buffer sized zero retains nothing but still has to be able to say
    /// whether a resuming client missed anything, and after enough pushes the
    /// entries no longer remember where the sequence started.
    last_appended_seq: u64,
}

impl ReplayBuffer {
    /// A ring retaining at most `capacity` frames.
    pub fn new(capacity: usize) -> ReplayBuffer {
        ReplayBuffer {
            max_entries: capacity,
            entries: Vec::new(),
            last_appended_seq: 0,
        }
    }

    /// How many frames this retains at most.
    pub fn capacity(&self) -> usize {
        self.max_entries
    }

    /// How many frames it is holding.
    pub fn size(&self) -> usize {
        self.entries.len()
    }

    /// The highest `seq` emitted for this session, retained or not.
    pub fn last_seq(&self) -> u64 {
        self.last_appended_seq
    }

    /// Retains one emitted frame.
    ///
    /// A message with no `seq` is not part of any session's replayable history
    /// and is ignored, which is the same rule the turn log and the client apply.
    pub fn push(&mut self, message: ServerMessage) {
        let Some(seq) = message.seq() else {
            return;
        };
        self.last_appended_seq = seq;
        if self.max_entries == 0 {
            return;
        }
        self.entries.push(message);
        if self.entries.len() > self.max_entries {
            let excess = self.entries.len() - self.max_entries;
            self.entries.drain(..excess);
        }
    }

    /// What a client resuming at `last_seq` still needs.
    ///
    /// `complete` answers one question — "is what follows the whole gap?" — and
    /// the caller decides what to do about a `false`. It is not the same as a
    /// non-empty slice: a client that has already seen everything gets an empty,
    /// complete one.
    pub fn after(&self, last_seq: u64) -> ReplaySlice {
        if last_seq >= self.last_appended_seq {
            // Equal means nothing was missed. Greater means this client saw a
            // sequence this buffer never emitted — a restart, or another server.
            return ReplaySlice {
                messages: Vec::new(),
                complete: last_seq == self.last_appended_seq,
            };
        }

        let messages: Vec<ServerMessage> = self
            .entries
            .iter()
            .filter(|entry| entry.seq().is_some_and(|seq| seq > last_seq))
            .cloned()
            .collect();
        let complete = messages
            .first()
            .and_then(ServerMessage::seq)
            .is_some_and(|seq| seq == last_seq + 1);
        ReplaySlice { messages, complete }
    }

    /// Drops every retained frame, keeping the sequence position.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
