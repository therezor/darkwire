//! Opaque pagination cursors.
//!
//! Cursor rather than offset for a reader moving *through* a listing, because
//! these listings move under one: messages are appended while a transcript is
//! being read, and a turn landing anywhere bumps its session to the front of the
//! session list. An offset shifts with them, so an offset-paged reader sees one
//! row twice and misses another — on exactly the long conversations where it
//! matters.
//!
//! **Two endpoints also accept an offset, and that is not a retreat from the
//! paragraph above.** `sessions.list` and `automation.runs` are read by numbered
//! pagers as well — the sessions management screen and a job's run history — and
//! a numbered pager is not the reader that argument is about. It does not walk
//! the list; it jumps to page 7 and acts on a row there, which is a position it
//! has never visited and therefore has no cursor for. Reordering under it costs
//! it the guarantee that consecutive pages do not overlap, which is a guarantee
//! it was never using: it is filtering to a handful of rows and clicking one.
//!
//! The two modes are alternatives and [`assert_one_paging_mode`] refuses the
//! pair. Which one a caller gets is decided by which it sends: the sidebar and
//! every sequential reader send `cursor`, a pager sends `offset`, and a cursor
//! is only *issued* while the listing is in the ordering that cursor addresses.
//!
//! The encoding is base64url'd JSON, and the point of it is that it is *opaque*.
//! A cursor that reads as `42` is a cursor a client will do arithmetic on, and
//! the moment it does, the server can no longer change what a cursor addresses
//! without breaking it. The protocol says "echo back `nextCursor` verbatim"; this
//! makes that the only usable option rather than merely the documented one.
//!
//! A cursor that does not decode is a 400, never a silent restart from the top.
//! Silently ignoring it would page a client through the same first page forever.

use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::errors::HttpError;

/// base64url without padding on the way out, and indifferent to it on the way
/// back: a cursor that survived a round trip through a client that re-padded it
/// is still a cursor this server issued.
const BASE64URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Every failure mode — not base64, not JSON, truncated by a URL shortener — is
/// the same thing to the caller: a cursor this server did not issue.
fn malformed() -> HttpError {
    HttpError::bad_request("Malformed pagination cursor")
}

/// Where a message listing resumes: the `seq` of the last row already sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageCursor {
    /// The last `seq` the caller has already been sent.
    pub seq: i64,
}

/// Where a session listing resumes, in the `updatedAtMs DESC, key ASC` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListCursor {
    /// The last row's `updated_at_ms`.
    pub updated_at_ms: i64,
    /// The last row's key, which breaks a tie on the timestamp.
    pub key: String,
}

/// Where a notification listing resumes, in the `createdAtMs DESC, id ASC`
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationCursor {
    /// The last row's `created_at_ms`.
    pub created_at_ms: i64,
    /// The last row's id, which breaks a tie on the timestamp.
    pub id: String,
}

/// Where a run listing resumes, in the `startedAtMs DESC, id ASC` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationRunCursor {
    /// The last row's `started_at_ms`.
    pub started_at_ms: i64,
    /// The last row's id, which breaks a tie on the timestamp.
    pub id: String,
}

/// Refuses a request that names both paging modes.
///
/// A cursor addresses a position in the sort order and an offset counts rows
/// from the top, so a request carrying both is asking for a page relative to a
/// page. There is no reading of that which is more correct than the others, and
/// a precedence rule would mean one of the two parameters is silently ignored —
/// which looks exactly like a server that paged wrongly.
pub fn assert_one_paging_mode(cursor: Option<&str>, offset: Option<u32>) -> Result<(), HttpError> {
    if cursor.is_some() && offset.is_some() {
        return Err(HttpError::bad_request(
            "Send either a cursor or an offset, not both",
        ));
    }
    Ok(())
}

fn encode<T: Serialize>(payload: &T) -> String {
    // Serialising a struct of scalars cannot fail; an empty cursor would be
    // refused on the way back in, which is the same answer a corrupt one gets.
    let json = serde_json::to_vec(payload).unwrap_or_default();
    BASE64URL.encode(json)
}

fn decode<T: for<'de> Deserialize<'de>>(cursor: &str) -> Result<T, HttpError> {
    let bytes = BASE64URL.decode(cursor).map_err(|_| malformed())?;
    let raw: Value = serde_json::from_slice(&bytes).map_err(|_| malformed())?;
    if !raw.is_object() {
        return Err(malformed());
    }
    serde_json::from_value(raw).map_err(|_| malformed())
}

/// The wire shape of a message cursor. Short keys because nothing reads them.
#[derive(Serialize, Deserialize)]
struct WireMessage {
    s: i64,
}

/// The wire shape of a session cursor.
#[derive(Serialize, Deserialize)]
struct WireSession {
    u: i64,
    k: String,
}

/// The wire shape of a notification cursor.
#[derive(Serialize, Deserialize)]
struct WireNotification {
    c: i64,
    i: String,
}

/// The wire shape of an automation-run cursor.
#[derive(Serialize, Deserialize)]
struct WireRun {
    s: i64,
    i: String,
}

/// A cursor naming the last message already sent.
pub fn encode_message_cursor(cursor: &MessageCursor) -> String {
    encode(&WireMessage { s: cursor.seq })
}

/// Reads a message cursor, or refuses one this server did not issue.
pub fn decode_message_cursor(cursor: &str) -> Result<MessageCursor, HttpError> {
    let wire: WireMessage = decode(cursor)?;
    Ok(MessageCursor { seq: wire.s })
}

/// A cursor naming the last session row already sent.
pub fn encode_session_cursor(cursor: &SessionListCursor) -> String {
    encode(&WireSession {
        u: cursor.updated_at_ms,
        k: cursor.key.clone(),
    })
}

/// Reads a session cursor, or refuses one this server did not issue.
pub fn decode_session_cursor(cursor: &str) -> Result<SessionListCursor, HttpError> {
    let wire: WireSession = decode(cursor)?;
    if wire.k.is_empty() {
        return Err(malformed());
    }
    Ok(SessionListCursor {
        updated_at_ms: wire.u,
        key: wire.k,
    })
}

/// A cursor naming the last notification already sent.
pub fn encode_notification_cursor(cursor: &NotificationCursor) -> String {
    encode(&WireNotification {
        c: cursor.created_at_ms,
        i: cursor.id.clone(),
    })
}

/// Reads a notification cursor, or refuses one this server did not issue.
pub fn decode_notification_cursor(cursor: &str) -> Result<NotificationCursor, HttpError> {
    let wire: WireNotification = decode(cursor)?;
    if wire.i.is_empty() {
        return Err(malformed());
    }
    Ok(NotificationCursor {
        created_at_ms: wire.c,
        id: wire.i,
    })
}

/// A cursor naming the last automation run already sent.
pub fn encode_automation_run_cursor(cursor: &AutomationRunCursor) -> String {
    encode(&WireRun {
        s: cursor.started_at_ms,
        i: cursor.id.clone(),
    })
}

/// Reads an automation-run cursor, or refuses one this server did not issue.
pub fn decode_automation_run_cursor(cursor: &str) -> Result<AutomationRunCursor, HttpError> {
    let wire: WireRun = decode(cursor)?;
    if wire.i.is_empty() {
        return Err(malformed());
    }
    Ok(AutomationRunCursor {
        started_at_ms: wire.s,
        id: wire.i,
    })
}

/// One over-fetched page, split into the rows to send and the cursor to follow.
///
/// Every listing here asks its store for `limit + 1` rows. The extra row is not
/// data — it is the answer to "is there a next page", which a keyset cursor
/// cannot know any other way, and it is dropped rather than returned. That rule
/// was written out at four call sites, three lines each, and the failure mode if
/// one of them drifts is a pager that offers a next page that is empty or hides
/// one that is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// The rows to send.
    pub rows: Vec<T>,
    /// The cursor a sequential reader follows, when there is a next page and
    /// this listing's ordering is one a cursor can address.
    pub next_cursor: Option<String>,
}

/// Splits an over-fetched page and issues the cursor that follows it.
///
/// `issue` is for the one listing that can page under more than one ordering. A
/// cursor encodes a position in *an* ordering; handing one back for a different
/// sort would hand back a cursor that cannot be followed, so sessions sorted by
/// title issue none and the pager uses `total` instead.
pub fn paginate<T>(
    mut rows: Vec<T>,
    limit: usize,
    encode_last: impl FnOnce(&T) -> String,
    issue: bool,
) -> Page<T> {
    let over_fetched = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = match rows.last() {
        Some(last) if issue && over_fetched => Some(encode_last(last)),
        _ => None,
    };
    Page { rows, next_cursor }
}
