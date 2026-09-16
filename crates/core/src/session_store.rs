//! Session and message persistence.
//!
//! One SQLite file holds everything DarkWire owns. Messages are **append-only**:
//! nothing ever updates a row in `messages`. That is not a stylistic
//! preference — a provider's prompt cache keys on an exact prefix, so editing
//! history invalidates the cache for every turn that follows and quietly
//! multiplies the cost of a long conversation. So history is *windowed* rather
//! than rewritten: a reader takes the tail it can afford and leaves the rows
//! alone.
//!
//! **`truncate_after` does not break that rule, and it is worth being precise
//! about why.** The rule forbids *rewriting* — an `UPDATE` on a `messages`
//! row, which changes a prefix the provider has already cached and invalidates
//! everything after it. Dropping a *suffix* changes no prefix: every retained
//! row is byte-identical, so the cached prefix stays warm and the conversation
//! simply re-diverges from the cut. That is the case a prompt cache is built
//! for. Regenerate and edit are therefore expressible; "change what the model
//! was told three turns ago and keep the answers" still is not.
//!
//! Rows rather than a JSONL file per session: appending a row satisfies the
//! same append-only constraint while giving pagination, listing, and
//! transactions for free — and removes the mtime-cache bookkeeping a
//! file-per-session store needs to avoid re-reading the whole transcript on
//! every access.

use std::sync::Arc;

use darkwire_protocol::json::Object;
use darkwire_protocol::messages::{ChatMessage, StopReason, StoredMessage, Usage};
use darkwire_protocol::subagent::subagent_runs_of;
use garde::Validate as _;
use indexmap::IndexMap;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Row, params, params_from_iter};
use serde_json::{Map, Value, json};

use crate::clock::Clock;
use crate::db::Database;
use crate::errors::{ErrorKind, Result, WireError};
use crate::history::{
    HistoryOptions, MessageWindow, SessionHistorySource, find_legal_end, session_history,
};
use crate::ids::DEFAULT_WORKSPACE_ID;
use crate::messages::text_of;
use crate::session_title::derive_session_title;
use crate::sqlite_row::{RowReader, parse_metadata};

/// The `sessions` table.
///
/// `workspace_id` has no `REFERENCES workspaces(id)`: the two tables are
/// created by two different stores in an order nothing guarantees. The
/// relationship is held in code instead, which is also what lets a *detached*
/// workspace's sessions keep resolving to their own files rather than falling
/// into another's.
///
/// No comment may appear inside a column list, here or below; see
/// [`crate::db`] for why. Each column's rationale therefore sits on the const.
pub const SESSIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS sessions (
  key                   TEXT    PRIMARY KEY,
  title                 TEXT    NOT NULL DEFAULT '',
  origin                TEXT    NOT NULL DEFAULT 'web',
  agent_id              TEXT,
  workspace_id          TEXT    NOT NULL DEFAULT 'default',
  created_at_ms         INTEGER NOT NULL,
  updated_at_ms         INTEGER NOT NULL,
  metadata_json         TEXT    NOT NULL DEFAULT '{}',
  next_seq              INTEGER NOT NULL DEFAULT 1
) STRICT;";

/// The index a workspace-scoped listing runs on.
pub const SESSIONS_WORKSPACE_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS sessions_workspace ON sessions(workspace_id, updated_at_ms DESC);";

/// The `messages` table. `payload_json` is the bare `ChatMessage`.
pub const MESSAGES_TABLE: &str = "CREATE TABLE IF NOT EXISTS messages (
  id            TEXT    PRIMARY KEY,
  session_key   TEXT    NOT NULL REFERENCES sessions(key) ON DELETE CASCADE,
  seq           INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  turn_id       TEXT,
  role          TEXT    NOT NULL,
  payload_json  TEXT    NOT NULL
) STRICT;";

/// What a turn cost, keyed by the id that groups its messages.
///
/// Here rather than in the migration ledger because the ledger is only for
/// altering tables that already exist; a new table is created by the store
/// that owns it. That is also what lets this one carry a real foreign key,
/// which `ADD COLUMN` cannot express — so deleting a session takes its stats
/// with it.
///
/// Usage is five integer columns rather than a JSON blob so that a session's
/// total is one `SUM(...) GROUP BY session_key` over a page of keys instead of
/// a query per row. `SUM` over all-`NULL` returns `NULL`, which is exactly
/// what the two optional usage fields mean.
///
/// The agent is recorded per turn rather than read from the session because a
/// session can be moved to another agent, and a transcript that then reported
/// every past turn as the new agent's work would be a lie about what ran.
///
/// `error` is why the turn stopped, when `stop_reason` is `error`. Here
/// rather than as a message row, because everything in `messages` is replayed
/// into every later provider request: an error appended to history would fail
/// its way into the prompt forever. `NULL` for every turn that did not fail.
///
/// `workspace_id` is where this turn actually ran, not where the session is
/// now. A session can be moved between workspaces mid-conversation, so the two
/// genuinely differ and only this column can say which files a given turn
/// could reach.
///
/// `generation_ms` is what the model spent generating and `first_token_ms` is
/// how long the turn waited to start. `NULL` on every turn recorded before
/// these were measured, which is what lets a rate fall back to the wall clock
/// rather than reporting nothing for old rows. `generation_tokens` is the
/// tokens produced inside `generation_ms`, which is not the same as
/// `completion_tokens`: a reply that arrives in one frame is charged for its
/// tokens and measured at zero, so it contributes to neither.
pub const TURN_STATS_TABLE: &str = "CREATE TABLE IF NOT EXISTS turn_stats (
  turn_id           TEXT    PRIMARY KEY,
  session_key       TEXT    NOT NULL REFERENCES sessions(key) ON DELETE CASCADE,
  agent_id          TEXT    NOT NULL DEFAULT '',
  workspace_id      TEXT    NOT NULL DEFAULT 'default',
  provider          TEXT    NOT NULL DEFAULT '',
  model             TEXT    NOT NULL DEFAULT '',
  started_at_ms     INTEGER NOT NULL,
  ended_at_ms       INTEGER NOT NULL,
  iterations        INTEGER NOT NULL DEFAULT 0,
  stop_reason       TEXT    NOT NULL DEFAULT '',
  prompt_tokens     INTEGER NOT NULL DEFAULT 0,
  completion_tokens INTEGER NOT NULL DEFAULT 0,
  total_tokens      INTEGER NOT NULL DEFAULT 0,
  cached_tokens     INTEGER,
  reasoning_tokens  INTEGER,
  generation_ms     INTEGER,
  generation_tokens INTEGER,
  first_token_ms    INTEGER,
  error             TEXT
) STRICT;";

/// `seq` is unique per session; the index is also what keyset reads walk.
pub const MESSAGES_SESSION_SEQ_INDEX: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS messages_session_seq ON messages(session_key, seq);";
/// Finds every message of one turn.
pub const MESSAGES_TURN_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS messages_turn ON messages(session_key, turn_id);";
/// The recency order the default listing uses.
pub const SESSIONS_UPDATED_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS sessions_updated ON sessions(updated_at_ms DESC);";
/// A session's turns, most recent first.
pub const TURN_STATS_SESSION_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS turn_stats_session ON turn_stats(session_key, ended_at_ms DESC);";

/// Every DDL statement the store runs on construction, in order.
pub const SCHEMA: &[&str] = &[
    SESSIONS_TABLE,
    SESSIONS_WORKSPACE_INDEX,
    MESSAGES_TABLE,
    TURN_STATS_TABLE,
    MESSAGES_SESSION_SEQ_INDEX,
    MESSAGES_TURN_INDEX,
    SESSIONS_UPDATED_INDEX,
    TURN_STATS_SESSION_INDEX,
];

/// Columns a database created by an older build does not have, and the
/// statement that adds each.
///
/// `CREATE TABLE IF NOT EXISTS` does nothing to a table that already exists,
/// so a column added to the schema reaches a fresh install and no other.
/// Deliberately not a migration framework: a versioned ledger for a handful
/// of `ALTER TABLE`s is a mechanism with one caller.
///
/// `error` is in the list because it was always meant to be. The read
/// tolerates a database written before that column existed — which was true
/// of the read and false of the write, since the insert names it and would
/// have failed with `no such column` on exactly the databases the tolerance
/// was about.
pub const TURN_STATS_LEDGER: &[(&str, &str)] = &[
    (
        "workspace_id",
        "ALTER TABLE turn_stats ADD COLUMN workspace_id TEXT NOT NULL DEFAULT 'default'",
    ),
    ("error", "ALTER TABLE turn_stats ADD COLUMN error TEXT"),
    (
        "generation_ms",
        "ALTER TABLE turn_stats ADD COLUMN generation_ms INTEGER",
    ),
    (
        "generation_tokens",
        "ALTER TABLE turn_stats ADD COLUMN generation_tokens INTEGER",
    ),
    (
        "first_token_ms",
        "ALTER TABLE turn_stats ADD COLUMN first_token_ms INTEGER",
    ),
];

/// One conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// The caller-chosen key, unique across origins.
    pub key: String,
    /// Empty until something names it.
    pub title: String,
    /// Channel that owns it — `web`, `telegram`, `automation`, an extension id.
    pub origin: String,
    /// The workspace this session's tools run inside.
    ///
    /// A turn resolves its jail from *this* value rather than from whatever
    /// the request said, which is what makes switching workspaces in the UI
    /// safe while a turn is still running. The only thing that moves it is an
    /// explicit [`SessionStore::update_session`], and a turn already running
    /// captured its jail when it started, so the move lands from the next
    /// turn.
    pub workspace_id: String,
    /// The agent bound to the conversation, if any.
    pub agent_id: Option<String>,
    /// When the row was created.
    pub created_at_ms: i64,
    /// Bumped by every append, clear and patch; the default listing order.
    pub updated_at_ms: i64,
    /// An untyped bag channels and extensions write into.
    pub metadata: Map<String, Value>,
}

/// A session as a listing reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummaryRecord {
    /// The row.
    pub session: SessionRecord,
    /// How many messages it holds.
    pub message_count: usize,
}

/// A persisted message plus its `seq`.
///
/// `seq` is storage's own concern — a stable, gap-free, per-session ordering
/// that survives identical timestamps, which `created_at_ms` does not: two
/// messages appended in the same millisecond are common when a turn emits
/// parallel tool results, and ordering by time alone makes their order
/// arbitrary. It is also the pagination cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMessageRecord {
    /// The row id.
    pub id: String,
    /// The session it belongs to.
    pub session_key: String,
    /// Position in the session, from 1.
    pub seq: i64,
    /// When it was written.
    pub created_at_ms: i64,
    /// Groups every message produced by one user turn, including tool traffic.
    pub turn_id: Option<String>,
    /// The message itself.
    pub message: ChatMessage,
}

/// Clamps a stored integer onto the wire's unsigned range. A negative value
/// cannot be written by this store, so the clamp is a type conversion rather
/// than a policy.
fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Clamps a wire integer onto SQLite's signed range.
fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// A length or page size as SQLite binds it.
fn len_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// A payload that cannot be serialised is an invariant failure, not a storage
/// one: every `ChatMessage` serialises.
fn json_error(error: serde_json::Error) -> WireError {
    WireError::new(ErrorKind::Internal, "Message could not be serialised").with_source(error)
}

/// Narrows a storage record to the wire shape the REST and WS layers publish.
pub fn to_stored_message(record: &StoredMessageRecord) -> StoredMessage {
    StoredMessage {
        id: record.id.clone(),
        session_key: record.session_key.clone(),
        seq: to_u64(record.seq),
        created_at_ms: to_u64(record.created_at_ms),
        turn_id: record.turn_id.clone(),
        message: record.message.clone(),
    }
}

/// Options for [`SessionStore::append`] and [`SessionStore::append_many`].
#[derive(Debug, Clone, Default)]
pub struct AppendOptions {
    /// Groups every message produced by one user turn, including tool traffic.
    pub turn_id: Option<String>,
}

/// What [`SessionStore::ensure_session`] uses when it creates the row.
#[derive(Debug, Clone, Default)]
pub struct CreateSession {
    /// Defaults to empty.
    pub title: Option<String>,
    /// Defaults to `web`.
    pub origin: Option<String>,
    /// Defaults to `default`. Honoured only when the row is actually created.
    pub workspace_id: Option<String>,
    /// No agent by default.
    pub agent_id: Option<String>,
    /// Defaults to an empty bag.
    pub metadata: Option<Map<String, Value>>,
}

/// A patch for [`SessionStore::update_session`]. `None` leaves a field alone.
#[derive(Debug, Clone, Default)]
pub struct UpdateSession {
    /// A new title.
    pub title: Option<String>,
    /// `Some(None)` clears the agent, `Some(Some(id))` binds one, `None`
    /// leaves it alone — the two have to stay distinguishable, or unsetting
    /// an agent becomes impossible through a patch that also touches any
    /// other field.
    pub agent_id: Option<Option<String>>,
    /// Moves the conversation to another workspace.
    ///
    /// The only thing that moves one. `ensure_session` deliberately cannot —
    /// see [`SessionRecord::workspace_id`] — so a turn, a socket frame and a
    /// scheduled run all leave an existing session where it is.
    pub workspace_id: Option<String>,
    /// Replaces the whole bag.
    pub metadata: Option<Map<String, Value>>,
}

/// Which column a session listing is ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionOrderBy {
    /// `updated_at_ms`, the recency order the list has always had.
    #[default]
    Updated,
    /// `created_at_ms`.
    Created,
    /// `title`, case-insensitively.
    Title,
}

/// A position in the `updated_at_ms DESC, key ASC` ordering
/// [`SessionStore::list_sessions`] uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCursor {
    /// The last row's `updated_at_ms`.
    pub updated_at_ms: i64,
    /// The last row's key.
    pub key: String,
}

/// Filters and paging for [`SessionStore::list_sessions`] and
/// [`SessionStore::count_sessions`].
#[derive(Debug, Clone, Default)]
pub struct ListSessions {
    /// Page size; defaults to 50.
    pub limit: Option<usize>,
    /// Rows to skip; defaults to 0. Prefer [`Self::after`] for anything a
    /// person pages through.
    pub offset: Option<usize>,
    /// Only sessions of this origin.
    pub origin: Option<String>,
    /// One origin to leave out, for a listing that is a shortlist rather than
    /// the record.
    ///
    /// The opposite of `origin` and not a replacement for it: this narrows by
    /// subtraction, so a caller that wants only conversations a person started
    /// can say so without enumerating every origin that is not `subagent`.
    /// Absent means nothing is excluded, which is what keeps the default
    /// listing the whole table — see [`session_filter`] for why that default
    /// is load-bearing.
    pub exclude_origin: Option<String>,
    /// Restricts the listing to one workspace.
    pub workspace_id: Option<String>,
    /// A case-insensitive substring of the title.
    ///
    /// Title only, and deliberately not message bodies: matching what was
    /// *said* in a conversation is an FTS5 index and a backfill, which is a
    /// feature rather than a clause. A blank or whitespace-only value is the
    /// same as none, so a cleared search box does not become `LIKE '%%'` on
    /// every row.
    ///
    /// `%` and `_` in the value are escaped, so searching for `100%` searches
    /// for `100%` rather than for everything.
    ///
    /// SQLite's `lower()` folds ASCII only, so a title outside it matches
    /// case-sensitively. Fixing that needs ICU, which is not compiled in.
    pub query: Option<String>,
    /// Defaults to `updated`, and `descending` defaults to true — together
    /// they are the recency order this list has always had.
    ///
    /// `messages` is deliberately not offered. The count is a correlated
    /// subquery, so ordering by it would run one scan of the messages table
    /// per session row.
    pub order_by: Option<SessionOrderBy>,
    /// Defaults to true.
    pub descending: Option<bool>,
    /// Keyset cursor: the `(updated_at_ms, key)` of the last row already seen.
    ///
    /// Preferred over `offset` for anything a user pages through, because the
    /// ordering key moves. A turn landing between two requests bumps a session
    /// to the front, which shifts every offset behind it by one and makes an
    /// offset-paged reader see one row twice and miss another. A keyset
    /// predicate asks for "strictly after this row in the sort order" instead,
    /// so a row that moves forward is one the reader has already passed and a
    /// row that does not move keeps its place.
    ///
    /// **Only meaningful in the default ordering**, because the predicate *is*
    /// that ordering written as a comparison. Combining it with `order_by` or
    /// an ascending sort errors rather than returning a plausible wrong page.
    pub after: Option<SessionCursor>,
}

/// A window over one session's messages, for [`SessionStore::messages`].
#[derive(Debug, Clone, Default)]
pub struct ReadMessages {
    /// Exclusive lower bound on `seq` — the pagination cursor.
    pub after_seq: Option<i64>,
    /// Exclusive upper bound on `seq`.
    pub before_seq: Option<i64>,
    /// At most this many rows; unbounded when absent.
    pub limit: Option<usize>,
    /// Takes the *last* `limit` messages rather than the first.
    pub from_end: bool,
}

/// What [`SessionStore::truncate_after`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TruncateResult {
    /// Where the cut actually landed, after snapping to a legal boundary.
    pub seq: i64,
    /// Rows removed.
    pub deleted: usize,
}

/// Overrides for [`SessionStore::fork_session`]; each inherits from the source
/// when absent.
#[derive(Debug, Clone, Default)]
pub struct ForkSession {
    /// Defaults to a fresh id from the store's id source.
    pub key: Option<String>,
    /// Defaults to the source's title, or one derived from its first message.
    pub title: Option<String>,
    /// Defaults to the source's workspace.
    pub workspace_id: Option<String>,
    /// Defaults to the source's agent.
    pub agent_id: Option<String>,
    /// Defaults to the source's origin.
    pub origin: Option<String>,
}

/// What [`SessionStore::fork_session`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkResult {
    /// The new session.
    pub session: SessionRecord,
    /// How many messages were copied.
    pub copied: usize,
    /// Where the fork actually cut, after snapping.
    pub seq: i64,
}

/// What one turn cost, recorded when it ends.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnStatsRecord {
    /// The id that groups the turn's messages.
    pub turn_id: String,
    /// The session the turn ran in.
    pub session_key: String,
    /// Which agent ran the turn. Empty on a turn recorded before agents
    /// existed.
    pub agent_id: String,
    /// Which workspace the turn ran in — the files it could actually reach.
    ///
    /// Not the session's current workspace: a conversation can be moved
    /// between them, and every turn before the move ran somewhere else.
    /// `default` on a turn recorded before this was captured.
    pub workspace_id: String,
    /// The provider that served it.
    pub provider: String,
    /// The model that served it.
    pub model: String,
    /// When the turn began.
    pub started_at_ms: i64,
    /// When the turn ended.
    pub ended_at_ms: i64,
    /// How many model round-trips it took.
    pub iterations: i64,
    /// Why it stopped.
    pub stop_reason: StopReason,
    /// Token accounting, summed over the turn's requests.
    pub usage: Usage,
    /// Time the model spent emitting tokens.
    ///
    /// `None` rather than zero on a turn recorded before this was measured,
    /// and on one no request could measure — the same distinction the two
    /// optional usage counters draw, and the one a rate needs in order to know
    /// it should fall back to the wall clock instead.
    pub generation_ms: Option<i64>,
    /// The completion tokens produced inside `generation_ms`, and only those.
    pub generation_tokens: Option<i64>,
    /// How long the turn waited for its first token, measured from the start
    /// of the turn so it can be read against `ended_at_ms - started_at_ms`.
    pub first_token_ms: Option<i64>,
    /// Why it stopped, when `stop_reason` is `error`.
    ///
    /// Absent on a turn that succeeded, and on any failure recorded before
    /// this was captured — nothing wrote it, and nothing can reconstruct it.
    pub error: Option<String>,
}

const READ: RowReader = RowReader::new("sessions");

fn read_usage(row: &Row<'_>) -> Usage {
    Usage {
        prompt_tokens: to_u64(READ.optional_int(row, "prompt_tokens").unwrap_or(0)),
        completion_tokens: to_u64(READ.optional_int(row, "completion_tokens").unwrap_or(0)),
        total_tokens: to_u64(READ.optional_int(row, "total_tokens").unwrap_or(0)),
        cached_tokens: READ.optional_int(row, "cached_tokens").map(to_u64),
        reasoning_tokens: READ.optional_int(row, "reasoning_tokens").map(to_u64),
    }
}

fn stop_reason_text(reason: StopReason) -> String {
    match serde_json::to_value(reason) {
        Ok(Value::String(text)) => text,
        _ => String::new(),
    }
}

fn parse_stop_reason(text: &str) -> Result<StopReason> {
    serde_json::from_value(Value::String(text.to_owned())).map_err(|_| {
        WireError::new(ErrorKind::Storage, format!("Not a stop reason: \"{text}\""))
            .with_detail("store", "sessions")
            .with_detail("column", "stop_reason")
    })
}

fn row_to_turn_stats(row: &Row<'_>) -> Result<TurnStatsRecord> {
    Ok(TurnStatsRecord {
        turn_id: READ.string(row, "turn_id")?,
        session_key: READ.string(row, "session_key")?,
        agent_id: READ.string(row, "agent_id")?,
        workspace_id: READ.string(row, "workspace_id")?,
        provider: READ.string(row, "provider")?,
        model: READ.string(row, "model")?,
        started_at_ms: READ.int(row, "started_at_ms")?,
        ended_at_ms: READ.int(row, "ended_at_ms")?,
        iterations: READ.int(row, "iterations")?,
        stop_reason: parse_stop_reason(&READ.string(row, "stop_reason")?)?,
        usage: read_usage(row),
        generation_ms: READ.optional_int(row, "generation_ms"),
        generation_tokens: READ.optional_int(row, "generation_tokens"),
        first_token_ms: READ.optional_int(row, "first_token_ms"),
        // Tolerant on purpose: a database written before this column existed
        // has no `error` in the row at all, and that is a turn with no
        // recorded reason rather than a corrupt one.
        error: READ.optional_string(row, "error"),
    })
}

fn row_to_session(row: &Row<'_>) -> Result<SessionRecord> {
    Ok(SessionRecord {
        key: READ.string(row, "key")?,
        title: READ.string(row, "title")?,
        origin: READ.string(row, "origin")?,
        workspace_id: READ.string(row, "workspace_id")?,
        agent_id: READ.optional_string(row, "agent_id"),
        created_at_ms: READ.int(row, "created_at_ms")?,
        updated_at_ms: READ.int(row, "updated_at_ms")?,
        metadata: parse_metadata(&READ.string(row, "metadata_json")?),
    })
}

fn row_to_message(session_key: &str, row: &Row<'_>) -> Result<StoredMessageRecord> {
    let seq = READ.int(row, "seq")?;
    let payload = READ.string(row, "payload_json")?;
    let message = serde_json::from_str::<ChatMessage>(&payload).map_err(|error| {
        WireError::new(
            ErrorKind::Storage,
            "Stored message failed schema validation",
        )
        .with_detail("sessionKey", session_key)
        .with_detail("seq", seq)
        .with_detail("issues", error.to_string())
    })?;
    Ok(StoredMessageRecord {
        id: READ.string(row, "id")?,
        session_key: session_key.to_owned(),
        seq,
        created_at_ms: READ.int(row, "created_at_ms")?,
        turn_id: READ.optional_string(row, "turn_id"),
        message,
    })
}

/// A `LIKE` operand that means "contains this literal text".
///
/// `%`, `_` and the escape character itself are escaped, so a search for
/// `100%` looks for `100%` rather than matching every row. Lowercased here to
/// pair with the `lower(s.title)` on the other side of the comparison.
fn like_pattern(query: &str) -> String {
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for ch in query.to_lowercase().chars() {
        if matches!(ch, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

/// The `WHERE` fragment a session listing runs under, and its bindings.
struct SessionFilter {
    sql: &'static str,
    bindings: Vec<SqlValue>,
}

fn text_or_null(value: Option<&str>) -> SqlValue {
    value.map_or(SqlValue::Null, |text| SqlValue::Text(text.to_owned()))
}

/// The filter `list_sessions` and `count_sessions` share.
///
/// One function rather than the same clauses typed twice, because the two
/// callers must agree about what they are looking at or a pager reports a
/// total for a different set than the rows under it.
///
/// **Every origin is listed unless one is named.** Hiding `subagent` and
/// `automation` unless asked for by name — on the reasoning that neither is a
/// conversation a person had — produces a transcript with no way in: a
/// scheduled run's session is excluded from the sidebar, and the run history
/// beside the job shows its *output* without linking to the turn that produced
/// it, so the one place a stuck run could be diagnosed is unreachable from the
/// UI. Provenance is a column, not a filter.
///
/// The cost is real and worth naming: a job on a five-minute interval mints a
/// session per run, so a list sorted by recency carries a lot of them. That is
/// a presentation problem — a badge, a filter — and this is the wrong layer to
/// solve it by pretending the rows are not there.
///
/// **`exclude_origin` is that filter, and it is opt-in.** It is what the
/// sidebar asks for to keep delegated runs out of a list of thirty
/// conversations, and it is not a re-run of the mistake above for one reason:
/// the default is unchanged. A caller that names nothing still sees every
/// origin, so every transcript stays reachable from the screen whose job is to
/// reach them. The exclusion narrows one view; it does not decide what exists.
///
/// Why here rather than after the fetch: the caller asks for a bounded page,
/// so a client-side filter would let a burst of delegations eat the budget —
/// thirty rows in, five conversations shown. The count shares this predicate,
/// so `total` describes the same set.
fn session_filter(options: &ListSessions) -> SessionFilter {
    // Blank is the same as absent: a cleared search box must not become
    // `LIKE '%%'`, which is a full scan that matches everything.
    let pattern = options
        .query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(like_pattern);
    let origin = text_or_null(options.origin.as_deref());
    let exclude = text_or_null(options.exclude_origin.as_deref());
    let workspace = text_or_null(options.workspace_id.as_deref());
    let pattern = text_or_null(pattern.as_deref());

    SessionFilter {
        // Each clause is `? IS NULL OR …` so one prepared statement serves
        // every combination; the same value is bound twice because the
        // bindings are positional.
        sql: "(? IS NULL OR s.origin = ?)
          AND (? IS NULL OR s.origin <> ?)
          AND (? IS NULL OR s.workspace_id = ?)
          AND (? IS NULL OR lower(s.title) LIKE ? ESCAPE '\\')",
        bindings: vec![
            origin.clone(),
            origin,
            exclude.clone(),
            exclude,
            workspace.clone(),
            workspace,
            pattern.clone(),
            pattern,
        ],
    }
}

/// The recency order this list has always had, and the only one a cursor
/// addresses. See [`ListSessions::after`].
const DEFAULT_SESSION_ORDER: &str = "s.updated_at_ms DESC, s.key ASC";

/// The `ORDER BY` clause, always with `s.key ASC` behind it.
///
/// The tiebreak is not decoration: without it, rows equal on the chosen
/// column come back in whatever order the query planner produced last, so a
/// refetch reshuffles them under the reader.
fn session_order(options: &ListSessions) -> String {
    let column = match options.order_by.unwrap_or_default() {
        SessionOrderBy::Updated => "s.updated_at_ms",
        SessionOrderBy::Created => "s.created_at_ms",
        // `NOCASE` so `apollo` and `Apollo` sort together rather than in two
        // runs, matching how `WorkspaceStore` orders names.
        SessionOrderBy::Title => "s.title COLLATE NOCASE",
    };
    let direction = if options.descending.unwrap_or(true) {
        "DESC"
    } else {
        "ASC"
    };
    format!("{column} {direction}, s.key ASC")
}

/// A source of new row ids. Production feeds a UUIDv7 from the clock and a
/// random source; tests pass a counter so ids are stable.
pub type IdSource = Box<dyn Fn() -> String + Send + Sync>;

/// Sessions, messages and turn stats, over the shared connection.
pub struct SessionStore {
    db: Database,
    clock: Arc<dyn Clock>,
    new_id: IdSource,
}

impl std::fmt::Debug for SessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionStore").finish_non_exhaustive()
    }
}

fn no_such_session(key: &str) -> WireError {
    WireError::new(ErrorKind::NotFound, format!("No such session: {key}"))
        .with_detail("sessionKey", key)
}

impl SessionStore {
    /// Creates the tables if needed and adds any column an older build left
    /// out.
    pub fn new(db: Database, clock: Arc<dyn Clock>, new_id: IdSource) -> Result<SessionStore> {
        for statement in SCHEMA {
            db.execute_batch(statement)?;
        }
        let present = db.column_names("turn_stats")?;
        for (column, ddl) in TURN_STATS_LEDGER {
            if !present.iter().any(|name| name == column) {
                db.execute_batch(ddl)?;
            }
        }
        Ok(SessionStore { db, clock, new_id })
    }

    /// The connection this store shares, so a caller can join a transaction.
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// One session by key, or `None`.
    pub fn get_session(&self, key: &str) -> Result<Option<SessionRecord>> {
        let guard = self.db.lock();
        let mut statement = guard.prepare_cached("SELECT * FROM sessions WHERE key = ?")?;
        let mut rows = statement.query_and_then([key], row_to_session)?;
        rows.next().transpose()
    }

    /// Returns the session, creating it if absent.
    ///
    /// Idempotent: the create is `INSERT OR IGNORE`, so two channels racing to
    /// open the same session key both get the existing row rather than one of
    /// them failing on the primary key.
    pub fn ensure_session(&self, key: &str, options: CreateSession) -> Result<SessionRecord> {
        if key.is_empty() {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                "Session key must not be empty",
            ));
        }

        let now = self.clock.now_ms();
        let metadata = Value::Object(options.metadata.unwrap_or_default()).to_string();
        self.db.lock().execute(
            "INSERT OR IGNORE INTO sessions
         (key, title, origin, workspace_id, agent_id, created_at_ms, updated_at_ms, metadata_json)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                key,
                options.title.unwrap_or_default(),
                options.origin.unwrap_or_else(|| "web".to_owned()),
                // `OR IGNORE`, so this only lands when the row is created. A
                // turn arriving with a different workspace must not move a
                // conversation's files out from under it; moving one is an
                // explicit `update_session`.
                options
                    .workspace_id
                    .unwrap_or_else(|| DEFAULT_WORKSPACE_ID.to_owned()),
                options.agent_id,
                now,
                now,
                metadata,
            ],
        )?;

        self.get_session(key)?.ok_or_else(|| {
            WireError::new(
                ErrorKind::Storage,
                "Session vanished immediately after insert",
            )
            .with_detail("key", key)
        })
    }

    /// A page of sessions with their message counts.
    pub fn list_sessions(&self, options: &ListSessions) -> Result<Vec<SessionSummaryRecord>> {
        let filter = session_filter(options);
        let order = session_order(options);

        if options.after.is_some() && order != DEFAULT_SESSION_ORDER {
            // The keyset predicate below is the *default* ordering written as
            // a comparison. Applied under any other one it addresses a
            // position that does not exist there, and the page that comes
            // back looks entirely plausible — which is why this errors instead
            // of quietly ignoring one of the two arguments.
            return Err(WireError::new(
                ErrorKind::Storage,
                "A session cursor is only valid in the default ordering",
            )
            .with_detail(
                "orderBy",
                format!("{:?}", options.order_by.unwrap_or_default()).to_lowercase(),
            )
            .with_detail("descending", options.descending.unwrap_or(true)));
        }

        let cursor_ms = options.after.as_ref().map_or(SqlValue::Null, |cursor| {
            SqlValue::Integer(cursor.updated_at_ms)
        });
        let cursor_key = text_or_null(options.after.as_ref().map(|cursor| cursor.key.as_str()));
        let limit = len_to_i64(options.limit.unwrap_or(50));
        let offset = len_to_i64(options.offset.unwrap_or(0));

        let mut bindings = filter.bindings;
        bindings.extend([
            cursor_ms.clone(),
            cursor_ms.clone(),
            cursor_ms,
            cursor_key,
            SqlValue::Integer(limit),
            SqlValue::Integer(offset),
        ]);

        // The keyset predicate is that order written as a comparison: strictly
        // older, or the same instant and a key that sorts later.
        let sql = format!(
            "SELECT s.*, (SELECT COUNT(*) FROM messages m WHERE m.session_key = s.key) AS message_count
         FROM sessions s
        WHERE {}
          AND (? IS NULL
               OR s.updated_at_ms < ?
               OR (s.updated_at_ms = ? AND s.key > ?))
        ORDER BY {order}
        LIMIT ? OFFSET ?",
            filter.sql
        );

        let guard = self.db.lock();
        let mut statement = guard.prepare_cached(&sql)?;
        let rows = statement.query_and_then(params_from_iter(bindings.iter()), |row| {
            Ok(SessionSummaryRecord {
                session: row_to_session(row)?,
                message_count: usize::try_from(READ.int(row, "message_count")?).unwrap_or(0),
            })
        })?;
        rows.collect()
    }

    /// How many sessions the same filter matches, ignoring the page.
    ///
    /// What a numbered pager needs and a cursor does not: "Page 3 of 12"
    /// cannot be derived from a page of rows. It shares [`session_filter`]
    /// with [`Self::list_sessions`] rather than restating the predicate,
    /// because a count and a page that disagree about what they are counting
    /// produce a "Page 4 of 3" that only appears once a filter is applied — a
    /// bug that is invisible in every test that does not filter.
    pub fn count_sessions(&self, options: &ListSessions) -> Result<usize> {
        let filter = session_filter(options);
        let sql = format!("SELECT COUNT(*) AS n FROM sessions s WHERE {}", filter.sql);
        let guard = self.db.lock();
        let mut statement = guard.prepare_cached(&sql)?;
        let n: i64 =
            statement.query_row(params_from_iter(filter.bindings.iter()), |row| row.get("n"))?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// How many sessions name a workspace.
    ///
    /// Lives here rather than on `WorkspaceStore` because this table is this
    /// store's, and a registry reaching across to count rows it does not own
    /// is how two stores end up with two ideas of the same schema. Detaching a
    /// workspace is refused while this is non-zero.
    pub fn count_by_workspace(&self, workspace_id: &str) -> Result<usize> {
        self.count(
            "SELECT COUNT(*) AS n FROM sessions WHERE workspace_id = ?",
            workspace_id,
        )
    }

    /// Moves every session in one workspace to another, and reports how many.
    ///
    /// The escape hatch behind the delete refusal: an operator who wants a
    /// workspace gone anyway moves its conversations to the default first. It
    /// deliberately does not touch `updated_at_ms` — reassigning is
    /// bookkeeping, not activity, and bumping every row would reorder the
    /// session list.
    pub fn reassign_workspace(&self, from: &str, to: &str) -> Result<usize> {
        let changed = self.db.lock().execute(
            "UPDATE sessions SET workspace_id = ? WHERE workspace_id = ?",
            [to, from],
        )?;
        Ok(changed)
    }

    /// How many messages a session holds.
    pub fn message_count(&self, session_key: &str) -> Result<usize> {
        self.count(
            "SELECT COUNT(*) AS n FROM messages WHERE session_key = ?",
            session_key,
        )
    }

    fn count(&self, sql: &str, binding: &str) -> Result<usize> {
        let guard = self.db.lock();
        let mut statement = guard.prepare_cached(sql)?;
        let n: i64 = statement.query_row([binding], |row| row.get("n"))?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Appends one message and returns it as persisted.
    pub fn append(
        &self,
        session_key: &str,
        message: ChatMessage,
        options: &AppendOptions,
    ) -> Result<StoredMessageRecord> {
        let mut records = self.append_many(session_key, vec![message], options)?;
        if records.is_empty() {
            return Err(
                WireError::new(ErrorKind::Storage, "Append returned no record")
                    .with_detail("sessionKey", session_key),
            );
        }
        Ok(records.swap_remove(0))
    }

    /// Appends several messages in one transaction.
    ///
    /// An assistant turn and its tool results land together or not at all — a
    /// partial write is precisely the orphaned-tool-result state that the
    /// history walker then has to repair on every subsequent request.
    ///
    /// Validates each message on the way in, so a malformed one is rejected
    /// by the caller that produced it instead of surfacing later as a
    /// provider 400 from a session nobody can explain.
    pub fn append_many(
        &self,
        session_key: &str,
        messages: Vec<ChatMessage>,
        options: &AppendOptions,
    ) -> Result<Vec<StoredMessageRecord>> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }

        for (index, message) in messages.iter().enumerate() {
            if let Err(report) = message.validate() {
                return Err(WireError::new(
                    ErrorKind::InvalidInput,
                    "Message failed schema validation",
                )
                .with_detail("sessionKey", session_key)
                .with_detail("index", index)
                .with_detail("issues", report.to_string()));
            }
        }

        self.db.transaction(|conn| {
            self.ensure_session(session_key, CreateSession::default())?;
            let now = self.clock.now_ms();
            let block = len_to_i64(messages.len());

            let next_seq: i64 = conn
                .prepare_cached(
                    "UPDATE sessions SET next_seq = next_seq + ? WHERE key = ? RETURNING next_seq",
                )?
                .query_row(params![block, session_key], |row| row.get("next_seq"))
                .map_err(|error| {
                    WireError::new(ErrorKind::Storage, "Failed to reserve message sequence")
                        .with_detail("sessionKey", session_key)
                        .with_source(error)
                })?;
            // `next_seq` now points past the block just reserved.
            let first_seq = next_seq - block;

            let mut insert = conn.prepare_cached(
                "INSERT INTO messages (id, session_key, seq, created_at_ms, turn_id, role, payload_json)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
            )?;

            let mut records = Vec::with_capacity(messages.len());
            for (index, message) in messages.into_iter().enumerate() {
                let id = (self.new_id)();
                let seq = first_seq + len_to_i64(index);
                let payload = serde_json::to_string(&message).map_err(json_error)?;
                insert.execute(params![
                    id,
                    session_key,
                    seq,
                    now,
                    options.turn_id,
                    message.tag(),
                    payload,
                ])?;
                records.push(StoredMessageRecord {
                    id,
                    session_key: session_key.to_owned(),
                    seq,
                    created_at_ms: now,
                    turn_id: options.turn_id.clone(),
                    message,
                });
            }

            conn.prepare_cached("UPDATE sessions SET updated_at_ms = ? WHERE key = ?")?
                .execute(params![now, session_key])?;
            Ok(records)
        })
    }

    /// Reads persisted messages in `seq` order.
    ///
    /// A payload that no longer parses is a schema change that landed without
    /// a migration. It errors rather than skipping: silently dropping a
    /// message would break tool-call pairing for the turn it belonged to, and
    /// a loud failure at read time is far cheaper to diagnose than a provider
    /// 400 later.
    pub fn messages(
        &self,
        session_key: &str,
        options: &ReadMessages,
    ) -> Result<Vec<StoredMessageRecord>> {
        let after_seq = options.after_seq.unwrap_or(0);
        let limit = options.limit.map_or(-1, len_to_i64);
        let direction = if options.from_end { "DESC" } else { "ASC" };
        let sql = format!(
            "SELECT * FROM messages
        WHERE session_key = ? AND seq > ? AND (? IS NULL OR seq < ?)
        ORDER BY seq {direction}
        LIMIT ?"
        );

        let guard = self.db.lock();
        let mut statement = guard.prepare_cached(&sql)?;
        let mut records = statement
            .query_and_then(
                params![
                    session_key,
                    after_seq,
                    options.before_seq,
                    options.before_seq,
                    limit
                ],
                |row| row_to_message(session_key, row),
            )?
            .collect::<Result<Vec<_>>>()?;
        if options.from_end {
            records.reverse();
        }
        Ok(records)
    }

    /// The message list to send to a provider.
    ///
    /// A convenience over [`session_history`], which owns the decision — what
    /// a *model* is sent is not a persistence question. Kept as a method
    /// because every caller already has the store in hand.
    pub fn history(&self, session_key: &str, options: &HistoryOptions) -> Result<Vec<ChatMessage>> {
        session_history(self, session_key, options)
    }

    /// Patches a session, creating it first if needed, and returns the result.
    pub fn update_session(&self, session_key: &str, patch: UpdateSession) -> Result<SessionRecord> {
        let existing = self.ensure_session(session_key, CreateSession::default())?;
        let now = self.clock.now_ms();

        let next = SessionRecord {
            title: patch.title.unwrap_or(existing.title),
            agent_id: patch.agent_id.unwrap_or(existing.agent_id),
            workspace_id: patch.workspace_id.unwrap_or(existing.workspace_id),
            metadata: patch.metadata.unwrap_or(existing.metadata),
            updated_at_ms: now,
            key: existing.key,
            origin: existing.origin,
            created_at_ms: existing.created_at_ms,
        };

        self.db.lock().execute(
            "UPDATE sessions
          SET title = ?, agent_id = ?, workspace_id = ?, metadata_json = ?,
              updated_at_ms = ?
        WHERE key = ?",
            params![
                next.title,
                next.agent_id,
                next.workspace_id,
                Value::Object(next.metadata.clone()).to_string(),
                now,
                session_key,
            ],
        )?;

        Ok(next)
    }

    /// Re-points every conversation bound to one agent id at another.
    ///
    /// For renaming an agent, and only for that. A rename is the *same* agent
    /// under a new name, so the conversations it owns have to follow it —
    /// leaving them behind would drop every one of them onto the fallback
    /// path for an agent that never went anywhere.
    ///
    /// `turn_stats.agent_id` is deliberately untouched. That column records
    /// which agent ran each past turn, and it stays what it was written as:
    /// the rows are a record of what happened, not a pointer to what exists
    /// now. Same for the subagent lineage in session metadata, which
    /// snapshots the label beside the id precisely so a display never has to
    /// resolve one.
    ///
    /// Returns how many rows moved, which is the part of a rename that
    /// happens outside the settings tree and would otherwise have to be taken
    /// on trust.
    pub fn reassign_agent(&self, from: &str, to: &str) -> Result<usize> {
        self.reassign_agents(&[(from, to)])
    }

    /// The same, for every rename in one settings save, in one transaction.
    ///
    /// Transactional because a save can carry more than one rename and half
    /// of them landing is the state nobody can reason about: the settings
    /// tree would name agents that some conversations had followed and others
    /// had not, with nothing on disk saying which. All or none is the only
    /// answer that leaves the fallback able to describe what happened.
    ///
    /// This is the *second* of two stores a rename touches, and the order is
    /// deliberate. The config is written first, atomically, so a failure here
    /// leaves conversations pointing at ids that no longer resolve — which is
    /// the case the default-agent fallback exists for, and which re-running
    /// the rename repairs. The other order would move the conversations and
    /// then fail to record why, which nothing could repair because nothing
    /// would say it had happened.
    pub fn reassign_agents(&self, renames: &[(&str, &str)]) -> Result<usize> {
        if renames.is_empty() {
            return Ok(0);
        }
        self.db.transaction(|conn| {
            let now = self.clock.now_ms();
            let mut update = conn.prepare_cached(
                "UPDATE sessions SET agent_id = ?, updated_at_ms = ? WHERE agent_id = ?",
            )?;
            let mut moved = 0;
            for (from, to) in renames {
                if from == to {
                    continue;
                }
                moved += update.execute(params![to, now, from])?;
            }
            Ok(moved)
        })
    }

    /// Drops every message but keeps the session row.
    ///
    /// `next_seq` is *not* reset. Reusing sequence numbers after a clear would
    /// make a stale `after_seq` cursor held by a reconnecting client silently
    /// address the wrong messages; sequences are monotonic for the session's
    /// lifetime, and the gap is the point.
    pub fn clear_messages(&self, session_key: &str) -> Result<()> {
        self.db.transaction(|conn| {
            conn.prepare_cached("DELETE FROM messages WHERE session_key = ?")?
                .execute([session_key])?;
            conn.prepare_cached("UPDATE sessions SET updated_at_ms = ? WHERE key = ?")?
                .execute(params![self.clock.now_ms(), session_key])?;
            Ok(())
        })
    }

    /// The largest cut at or below `seq` that leaves no tool call unanswered.
    ///
    /// A cut through the middle of a tool exchange strands the `assistant`
    /// that declared the calls, which every provider rejects with a 400 — the
    /// mirror of the defect the history walker repairs at the other end of
    /// the window.
    ///
    /// `0` when no cut is legal: every message in the session is at or above
    /// seq 1, so a floor of zero means "keep nothing", which is what a session
    /// whose very first exchange is the unsplittable one has to fall back to.
    fn legal_seq(&self, session_key: &str, seq: i64) -> Result<i64> {
        let records = self.messages(
            session_key,
            &ReadMessages {
                after_seq: Some(0),
                before_seq: Some(seq.saturating_add(1)),
                ..ReadMessages::default()
            },
        )?;
        let messages: Vec<ChatMessage> = records.iter().map(|r| r.message.clone()).collect();
        let end = find_legal_end(&messages);

        if end == records.len() {
            return Ok(seq);
        }
        if end == 0 {
            return Ok(0);
        }
        Ok(records.get(end - 1).map_or(0, |record| record.seq))
    }

    /// Drops every message after `seq`, and reports where the cut actually
    /// landed.
    ///
    /// This is what regenerate and edit are built on: re-running a turn means
    /// forgetting the answers that followed the question. See the module
    /// header for why removing a suffix is compatible with the append-only
    /// rule that forbids rewriting a row.
    ///
    /// The cut is always snapped to a legal tool boundary. The caller has no
    /// information this store lacks with which to decide otherwise, and an
    /// unsnapped cut does not fail here — it fails as a provider 400 on the
    /// next turn, a long way from the code that caused it.
    ///
    /// `next_seq` is deliberately left alone, for the reason
    /// [`Self::clear_messages`] gives: a stale `after_seq` cursor held by a
    /// reconnecting client must never come back to address a different
    /// message. Sequences go sparse after a truncation, and the gap is the
    /// point.
    ///
    /// **Concurrency is the caller's problem**, because it has to be: this
    /// store cannot know a turn is running, and truncating under one would
    /// race the loop's own append. The hub guards with its busy flag; the
    /// CLI's REPL only reaches this at an idle prompt.
    pub fn truncate_after(&self, session_key: &str, seq: i64) -> Result<TruncateResult> {
        self.db.transaction(|conn| {
            if self.get_session(session_key)?.is_none() {
                return Err(no_such_session(session_key));
            }

            let cut = self.legal_seq(session_key, seq)?.max(0);
            let deleted = conn
                .prepare_cached("DELETE FROM messages WHERE session_key = ? AND seq > ?")?
                .execute(params![session_key, cut])?;

            // Nothing moved, so nothing should be bumped to the top of the
            // session list — a no-op truncation is not activity.
            if deleted > 0 {
                conn.prepare_cached("UPDATE sessions SET updated_at_ms = ? WHERE key = ?")?
                    .execute(params![self.clock.now_ms(), session_key])?;
            }

            Ok(TruncateResult { seq: cut, deleted })
        })
    }

    /// Copies a conversation up to `upto_seq` into a new session.
    ///
    /// What "branch" means here. The alternative — a `parent_seq` column and a
    /// message tree — buys sibling navigation at the cost of teaching every
    /// reader of the flat log about branches, including the CLI and the
    /// history walker. A fork is a session, so it appears in the sidebar,
    /// opens in the CLI and is deleted like any other, and the code that reads
    /// it needs to know nothing.
    ///
    /// Two decisions carry the weight:
    ///
    ///  - **Seqs are reseated densely from 1.** A fork is a new sequence
    ///    space; preserving the source's numbering would start a fresh
    ///    conversation at seq 4711 and leave the markers below pointing at
    ///    rows that are not there.
    ///  - **`turn_id` and `created_at_ms` are preserved.** Up to the cut the
    ///    fork *is* the same conversation, so it renders identically and its
    ///    turn stats — which are keyed by turn id — still describe the run
    ///    that produced it.
    ///
    /// Lineage goes in the metadata bag rather than a column: it costs no
    /// schema, no index and no query surface, and nothing needs to search by
    /// it.
    pub fn fork_session(
        &self,
        source_key: &str,
        upto_seq: i64,
        options: ForkSession,
    ) -> Result<ForkResult> {
        self.db.transaction(|conn| {
            let source = self
                .get_session(source_key)?
                .ok_or_else(|| no_such_session(source_key))?;

            let cut = self.legal_seq(source_key, upto_seq)?.max(0);
            let records = self.messages(
                source_key,
                &ReadMessages {
                    before_seq: Some(cut.saturating_add(1)),
                    ..ReadMessages::default()
                },
            )?;
            let now = self.clock.now_ms();
            let origin = options.origin.unwrap_or_else(|| source.origin.clone());
            let key = options.key.unwrap_or_else(|| (self.new_id)());

            if self.get_session(&key)?.is_some() {
                return Err(WireError::new(
                    ErrorKind::Conflict,
                    format!("Session already exists: {key}"),
                )
                .with_detail("sessionKey", key));
            }

            let title = match options.title {
                Some(title) => title,
                None if !source.title.is_empty() => source.title.clone(),
                None => records
                    .iter()
                    .find(|record| matches!(record.message, ChatMessage::User(_)))
                    .map_or_else(String::new, |first_user| {
                        derive_session_title(&text_of(&first_user.message))
                    }),
            };

            let mut metadata = source.metadata.clone();
            metadata.insert(
                "forkedFrom".to_owned(),
                json!({ "key": source_key, "seq": cut, "atMs": now }),
            );

            conn.prepare_cached(
                "INSERT INTO sessions
           (key, title, origin, workspace_id, agent_id, created_at_ms, updated_at_ms,
            metadata_json, next_seq)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )?
            .execute(params![
                key,
                title,
                origin,
                options
                    .workspace_id
                    .unwrap_or_else(|| source.workspace_id.clone()),
                options.agent_id.or_else(|| source.agent_id.clone()),
                source.created_at_ms,
                // Now, not the source's: a fork is something the user just
                // did, and the session list is ordered by this.
                now,
                Value::Object(metadata).to_string(),
                len_to_i64(records.len()) + 1,
            ])?;

            // `append_many` cannot be reused: it stamps one `now` and one
            // `turn_id` across the block, and both are being preserved per
            // row here.
            let mut insert = conn.prepare_cached(
                "INSERT INTO messages (id, session_key, seq, created_at_ms, turn_id, role, payload_json)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
            )?;
            for (index, record) in records.iter().enumerate() {
                insert.execute(params![
                    (self.new_id)(),
                    key,
                    len_to_i64(index) + 1,
                    record.created_at_ms,
                    record.turn_id,
                    record.message.tag(),
                    // Re-serialised from the parsed value rather than
                    // re-validated: `messages()` already parsed it on the way
                    // out.
                    serde_json::to_string(&record.message).map_err(json_error)?,
                ])?;
            }

            let session = self.get_session(&key)?.ok_or_else(|| {
                WireError::new(ErrorKind::Storage, "Fork vanished immediately after insert")
                    .with_detail("sessionKey", key.clone())
            })?;

            Ok(ForkResult {
                session,
                copied: records.len(),
                seq: cut,
            })
        })
    }

    /// Records what a turn cost.
    ///
    /// An upsert rather than a plain insert: a turn that ends twice — which
    /// the hub's own failure path can produce — must not fail on the primary
    /// key. The clause names every column, or it would silently keep the
    /// first write's value.
    pub fn record_turn_stats(&self, stats: &TurnStatsRecord) -> Result<()> {
        self.db.lock().execute(
            "INSERT INTO turn_stats
         (turn_id, session_key, agent_id, workspace_id, provider, model,
          started_at_ms, ended_at_ms,
          iterations, stop_reason, prompt_tokens, completion_tokens, total_tokens,
          cached_tokens, reasoning_tokens,
          generation_ms, generation_tokens, first_token_ms, error)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT(turn_id) DO UPDATE SET
         agent_id = excluded.agent_id,
         workspace_id = excluded.workspace_id,
         provider = excluded.provider, model = excluded.model,
         started_at_ms = excluded.started_at_ms, ended_at_ms = excluded.ended_at_ms,
         iterations = excluded.iterations, stop_reason = excluded.stop_reason,
         prompt_tokens = excluded.prompt_tokens,
         completion_tokens = excluded.completion_tokens,
         total_tokens = excluded.total_tokens, cached_tokens = excluded.cached_tokens,
         reasoning_tokens = excluded.reasoning_tokens,
         generation_ms = excluded.generation_ms,
         generation_tokens = excluded.generation_tokens,
         first_token_ms = excluded.first_token_ms,
         error = excluded.error",
            params![
                stats.turn_id,
                stats.session_key,
                stats.agent_id,
                stats.workspace_id,
                stats.provider,
                stats.model,
                stats.started_at_ms,
                stats.ended_at_ms,
                stats.iterations,
                stop_reason_text(stats.stop_reason),
                to_i64(stats.usage.prompt_tokens),
                to_i64(stats.usage.completion_tokens),
                to_i64(stats.usage.total_tokens),
                stats.usage.cached_tokens.map(to_i64),
                stats.usage.reasoning_tokens.map(to_i64),
                stats.generation_ms,
                stats.generation_tokens,
                stats.first_token_ms,
                stats.error,
            ],
        )?;
        Ok(())
    }

    /// A session's turns, most recent first; at most `limit` when given.
    pub fn turn_stats(
        &self,
        session_key: &str,
        limit: Option<usize>,
    ) -> Result<Vec<TurnStatsRecord>> {
        let guard = self.db.lock();
        let mut statement = guard.prepare_cached(
            "SELECT * FROM turn_stats
        WHERE session_key = ?
        ORDER BY ended_at_ms DESC, turn_id ASC
        LIMIT ?",
        )?;
        let rows = statement.query_and_then(
            params![session_key, limit.map_or(-1, len_to_i64)],
            row_to_turn_stats,
        )?;
        rows.collect()
    }

    /// Total usage per session, for a page of keys.
    ///
    /// One statement for the whole page rather than one per row — the session
    /// list reports this for every conversation it shows, and a query per row
    /// is the difference between a listing and fifty of them. The placeholder
    /// list varies with the page size, so a page of 50 and a page of 51
    /// prepare two statements; that is a handful of shapes and is not a reason
    /// to concatenate values into the SQL.
    ///
    /// A session with no recorded turns is absent rather than zeroed — the
    /// honest answer for a conversation whose turns predate the table.
    pub fn session_usage(&self, session_keys: &[&str]) -> Result<IndexMap<String, Usage>> {
        let mut totals = IndexMap::new();
        if session_keys.is_empty() {
            return Ok(totals);
        }

        let placeholders = vec!["?"; session_keys.len()].join(", ");
        let sql = format!(
            "SELECT session_key,
              SUM(prompt_tokens)     AS prompt_tokens,
              SUM(completion_tokens) AS completion_tokens,
              SUM(total_tokens)      AS total_tokens,
              SUM(cached_tokens)     AS cached_tokens,
              SUM(reasoning_tokens)  AS reasoning_tokens
         FROM turn_stats
        WHERE session_key IN ({placeholders})
        GROUP BY session_key"
        );

        let guard = self.db.lock();
        let mut statement = guard.prepare_cached(&sql)?;
        let rows = statement.query_and_then(params_from_iter(session_keys.iter()), |row| {
            Ok::<_, WireError>((READ.string(row, "session_key")?, read_usage(row)))
        })?;
        for row in rows {
            let (key, usage) = row?;
            totals.insert(key, usage);
        }
        Ok(totals)
    }

    /// Deletes the session, its messages, its turn stats — and its subagent
    /// runs. Returns whether a row existed.
    ///
    /// The first three are SQLite's cascade. The last one is not, and cannot
    /// be: the link to a subagent's session is a key inside the metadata bag,
    /// which is a JSON blob rather than a foreign key. Doing it here rather
    /// than leaving it to a caller is what keeps "delete this conversation"
    /// from leaving a row per delegation behind, invisible in every listing
    /// and reachable by nothing.
    ///
    /// One level, deliberately not recursive at the SQL layer but recursive by
    /// call: a subagent's own subagents are deleted when *it* is, because its
    /// row carries the same map. Depth is capped, so this terminates for the
    /// same reason delegation does.
    pub fn delete_session(&self, session_key: &str) -> Result<bool> {
        self.db.transaction(|conn| {
            let Some(session) = self.get_session(session_key)? else {
                return Ok(false);
            };

            let metadata: Object = session
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            for run in subagent_runs_of(&metadata).values() {
                // Not guarded on existence: a child that is already gone is
                // the normal case for a session deleted twice, and is not
                // worth distinguishing.
                self.delete_session(&run.session_key)?;
            }

            let deleted = conn
                .prepare_cached("DELETE FROM sessions WHERE key = ?")?
                .execute([session_key])?;
            Ok(deleted > 0)
        })
    }
}

impl SessionHistorySource for SessionStore {
    fn messages(&self, session_key: &str, window: &MessageWindow) -> Result<Vec<ChatMessage>> {
        let records = SessionStore::messages(
            self,
            session_key,
            &ReadMessages {
                after_seq: Some(to_i64(window.after_seq)),
                before_seq: None,
                limit: window.limit,
                from_end: window.from_end,
            },
        )?;
        Ok(records.into_iter().map(|record| record.message).collect())
    }
}
