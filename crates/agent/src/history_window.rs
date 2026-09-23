//! Fitting the history to the agent's context window.
//!
//! The message cap in `darkwire-core` bounds how many rows are read. It says
//! nothing about how large they are: five hundred one-line exchanges and fifty
//! pasted logs are the same count and nowhere near the same request. This
//! trims the window a second time, by estimated tokens, so a long session
//! stops failing on context length instead of being rescued by the provider
//! ladder after a wasted round trip.
//!
//! The cut only lands where a turn opens, so the model never sees half of an
//! earlier exchange, and never past the message that opened the turn being
//! answered. Tokens are the body-shaped estimate from
//! `darkwire-providers`, the same figure the context report prices.

use darkwire_core::history::{
    DEFAULT_MAX_HISTORY_MESSAGES, HistoryOptions, find_legal_start, history_for_llm,
};
use darkwire_core::session_store::{ReadMessages, StoredMessageRecord};
use darkwire_core::{Result, SessionStore};
use darkwire_protocol::ChatMessage;
use darkwire_providers::estimate_message_tokens;

/// The share of what is left after the fixed costs that the history may use.
///
/// The estimate is characters over four, and a real tokenizer can count a
/// dense line higher. The margin is what keeps that error from turning into a
/// context-length rejection.
pub const HISTORY_BUDGET_FACTOR: f64 = 0.9;

/// What a request costs before any history is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FixedCost {
    /// The window the whole request has to fit. `0` means unknown.
    pub context_window_tokens: u64,
    /// The system message, the runtime reminder and the tool schemas.
    pub prompt_tokens: usize,
    /// The completion the request asks room for.
    pub max_output_tokens: u64,
}

/// The tokens the history may take, or `None` when no window is known.
pub fn history_budget(cost: &FixedCost) -> Option<usize> {
    if cost.context_window_tokens == 0 {
        return None;
    }
    let prompt = u64::try_from(cost.prompt_tokens).unwrap_or(u64::MAX);
    let left = cost
        .context_window_tokens
        .saturating_sub(prompt)
        .saturating_sub(cost.max_output_tokens);
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a token count far below 2^53, scaled by a factor below one"
    )]
    let budget = (left as f64 * HISTORY_BUDGET_FACTOR) as u64;
    Some(usize::try_from(budget).unwrap_or(usize::MAX))
}

/// Whether the window may start at `index`: a user message that opens a turn.
///
/// Rows of one turn share a `turn_id`, so a steering message inside a turn is
/// not a boundary. A row with no turn id falls back to any user message.
fn opens_turn(records: &[StoredMessageRecord], index: usize) -> bool {
    let record = &records[index];
    if !matches!(record.message, ChatMessage::User(_)) {
        return false;
    }
    let Some(turn_id) = record.turn_id.as_deref() else {
        return true;
    };
    index == 0 || records[index - 1].turn_id.as_deref() != Some(turn_id)
}

/// The first index of `records` the budget lets the request carry.
///
/// Walks back from the newest row. Everything from the row with
/// `opening_seq` on is kept whatever it costs, because that is the turn being
/// answered. Older turns are added whole while they fit. When the opening row
/// is not in `records` the whole window belongs to the current turn, and
/// nothing is cut. `None` is a turn not yet stored, as a preview measures it:
/// every row is an older turn.
pub fn token_window_start(
    records: &[StoredMessageRecord],
    opening_seq: Option<i64>,
    budget: usize,
) -> usize {
    let floor = match opening_seq {
        None => records.len(),
        Some(seq) => match records.iter().position(|record| record.seq == seq) {
            Some(floor) => floor,
            None => return 0,
        },
    };
    let sizes: Vec<usize> = records
        .iter()
        .map(|record| estimate_message_tokens(&record.message))
        .collect();
    let mut used: usize = sizes[floor..].iter().sum();
    let mut start = floor;
    for index in (0..floor).rev() {
        used = used.saturating_add(sizes[index]);
        if used > budget {
            break;
        }
        if opens_turn(records, index) {
            start = index;
        }
    }
    if start == 0 {
        return 0;
    }
    let messages: Vec<ChatMessage> = records[start..]
        .iter()
        .map(|record| record.message.clone())
        .collect();
    start + find_legal_start(&messages)
}

/// The history one request carries, and whether the budget dropped any of it.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowedHistory {
    /// The stored rows, oldest first, tool results untruncated.
    pub records: Vec<StoredMessageRecord>,
    /// Whether older turns were left out to fit the window.
    pub trimmed: bool,
}

/// Reads the window a request carries: the message cap first, then the budget.
///
/// The loop and the context report both read through this, so the bar shows
/// what the next request sends. Rows rather than messages, because the turn
/// boundaries and the opening row are found by `turn_id` and `seq`.
/// `history_for_llm` only trims from the front when tool results are left
/// whole, so its output is a suffix of the rows and each message can be
/// matched back to its row by position.
///
/// # Errors
///
/// When the store cannot be read.
pub fn windowed_history(
    store: &SessionStore,
    session_key: &str,
    opening_seq: Option<i64>,
    budget: Option<usize>,
) -> Result<WindowedHistory> {
    let records = store.messages(
        session_key,
        &ReadMessages {
            after_seq: Some(0),
            limit: Some(DEFAULT_MAX_HISTORY_MESSAGES),
            from_end: true,
            ..ReadMessages::default()
        },
    )?;
    let all: Vec<ChatMessage> = records
        .iter()
        .map(|record| record.message.clone())
        .collect();
    let kept = history_for_llm(
        &all,
        &HistoryOptions {
            max_tool_result_chars: 0,
            ..HistoryOptions::default()
        },
    )
    .len();
    let mut records = records;
    records.drain(..all.len() - kept);
    let start = budget.map_or(0, |budget| {
        token_window_start(&records, opening_seq, budget)
    });
    records.drain(..start);
    Ok(WindowedHistory {
        records,
        trimmed: start > 0,
    })
}

impl WindowedHistory {
    /// The messages alone, as a request carries them.
    pub fn messages(&self) -> Vec<ChatMessage> {
        self.records
            .iter()
            .map(|record| record.message.clone())
            .collect()
    }
}
