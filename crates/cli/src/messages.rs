//! Naming a message from a prompt.
//!
//! The web addresses a message by clicking it. A terminal has to say which one,
//! and the scheme is the crux of whether `/edit` and `/regenerate` are usable
//! or a guessing game.
//!
//! `seq` is the address, because it already is one everywhere else: it is
//! storage's per-session ordering, the REST pagination cursor, and what
//! `turn.regenerate` and `user.edit` carry on the wire. A second scheme
//! invented for the terminal would be a second thing to keep in step.
//!
//! A negative reference counts back over **user messages only**. `-1` is the
//! last thing you said, `-2` the one before — not the last two rows, because
//! rows include assistant turns and tool results, and nobody counts backwards
//! over a tool result. That distinction is the whole reason this is a function
//! rather than arithmetic at the call site.
//!
//! `/messages` is what makes the scheme usable rather than a guess: it prints
//! the seqs, so the number in `/edit 12` is one that was read rather than
//! counted.

use darkwire_core::session_store::ReadMessages;
use darkwire_core::{ErrorKind, Result, SessionStore, WireError, text_of};

/// How many rows `/messages` prints when no count is given.
pub const DEFAULT_MESSAGE_LINES: usize = 12;

/// How far a listing looks back for the user messages a negative ref counts.
const LOOKBACK: usize = 400;

/// Resolves a message reference to a `seq`.
///
/// `None` means `-1`: the common case is re-running or rewriting the last thing
/// said, and making that the default is what keeps `/regenerate` a word rather
/// than a word and a number.
pub fn resolve_seq(
    store: &SessionStore,
    session_key: &str,
    reference: Option<&str>,
) -> Result<i64> {
    let trimmed = reference.unwrap_or("").trim();
    let raw: i64 = if trimmed.is_empty() {
        -1
    } else {
        trimmed.parse().map_err(|_| not_a_reference(trimmed))?
    };

    if raw == 0 {
        return Err(not_a_reference(trimmed));
    }

    if raw > 0 {
        let found = store.messages(
            session_key,
            &ReadMessages {
                after_seq: Some(raw - 1),
                before_seq: Some(raw + 1),
                ..ReadMessages::default()
            },
        )?;
        if found.is_empty() {
            return Err(WireError::new(
                ErrorKind::NotFound,
                format!("No message {raw} in this session"),
            )
            .with_detail("seq", raw));
        }
        return Ok(raw);
    }

    let spoken: Vec<i64> = store
        .messages(
            session_key,
            &ReadMessages {
                limit: Some(LOOKBACK),
                from_end: true,
                ..ReadMessages::default()
            },
        )?
        .into_iter()
        .filter(|record| record.message.tag() == "user")
        .map(|record| record.seq)
        .collect();

    // `-1` is the last element, `-2` the one before it.
    let back = raw.unsigned_abs();
    let available = spoken.len();
    let index = usize::try_from(back)
        .ok()
        .and_then(|back| available.checked_sub(back));
    let Some(seq) = index.and_then(|index| spoken.get(index)) else {
        let message = if available == 0 {
            "You have not said anything in this session yet".to_owned()
        } else {
            format!("Only {available} of your messages are in this session")
        };
        return Err(WireError::new(ErrorKind::NotFound, message)
            .with_detail("offset", raw)
            .with_detail("available", i64::try_from(available).unwrap_or(i64::MAX)));
    };
    Ok(*seq)
}

/// The refusal for a reference that is not one.
fn not_a_reference(trimmed: &str) -> WireError {
    WireError::new(
        ErrorKind::InvalidInput,
        format!("Not a message reference: {trimmed}"),
    )
    .with_detail("ref", trimmed)
}

/// One row of the `/messages` listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageLine {
    /// The address to pass to `/edit` or `/regenerate`.
    pub seq: i64,
    /// `user`, `assistant`, `tool` or `system`.
    pub role: &'static str,
    /// The message's text, with every other part dropped.
    pub text: String,
}

/// Every stored message in a session, oldest first, whole.
///
/// What the prompt replays when it opens on a conversation that already exists.
/// [`MessageLine`] is the `/messages` listing's shape and drops what it cannot
/// print in one row: the reasoning beside an answer, the calls a turn made. A
/// prompt coming back to a session wants those, so it reads the messages
/// themselves.
///
/// Unbounded on purpose: the point is to come back to the session as you left
/// it, and the transcript applies its own limit to a conversation too long to
/// hold.
pub fn session_messages(
    store: &SessionStore,
    session_key: &str,
) -> Result<Vec<darkwire_core::session_store::StoredMessageRecord>> {
    store.messages(session_key, &ReadMessages::default())
}

/// Everything said in a session, oldest first.
///
/// The `/messages` listing, one row a message with everything else dropped.
/// Tool traffic is in here and the caller decides what to do with it.
pub fn session_history(store: &SessionStore, session_key: &str) -> Result<Vec<MessageLine>> {
    Ok(store
        .messages(session_key, &ReadMessages::default())?
        .into_iter()
        .map(|record| MessageLine {
            seq: record.seq,
            role: record.message.tag(),
            text: text_of(&record.message),
        })
        .collect())
}

/// The last `count` messages, oldest first, as the listing renders them.
pub fn recent_messages(
    store: &SessionStore,
    session_key: &str,
    count: usize,
) -> Result<Vec<MessageLine>> {
    Ok(store
        .messages(
            session_key,
            &ReadMessages {
                limit: Some(count),
                from_end: true,
                ..ReadMessages::default()
            },
        )?
        .into_iter()
        .map(|record| MessageLine {
            seq: record.seq,
            role: record.message.tag(),
            text: text_of(&record.message),
        })
        .collect())
}
