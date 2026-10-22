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
//! `darkwire-providers`, the same figure the context report prices, scaled by
//! the provider's own count of the last request (see [`calibration_of`]).

use std::collections::HashMap;

use darkwire_core::history::{
    DEFAULT_MAX_HISTORY_MESSAGES, find_legal_start, history_window_start,
};
use darkwire_core::session_store::{ReadMessages, StoredMessageRecord};
use darkwire_core::{Result, SessionStore};
use darkwire_protocol::{ChatMessage, ContentPart};
use darkwire_providers::estimate_message_tokens;

use crate::attachments::{MaterialiseOptions, attachment_tokens};

/// The share of what is left after the fixed costs that the history may use.
///
/// Room for what the scaled estimate still gets wrong: the rows added since the
/// provider last counted, which are priced at the old request's ratio and may
/// not share it. Without a provider count it is all that stands between a
/// character estimate and a context-length rejection.
pub const HISTORY_BUDGET_FACTOR: f64 = 0.9;

/// The lowest ratio the budget will believe.
///
/// The worst steady overcount of the estimate is English prose, at about 0.79.
/// A provider reporting less than this is more likely truncating what it was
/// sent (Ollama with a `num_ctx` below the configured window) than counting a
/// smaller prompt. Believing it would grow the budget, which grows the request,
/// which the server truncates further, so the budget stops here and the loop
/// says why. The meter still reports the provider's figure.
pub const MIN_BUDGET_RATIO: f64 = 0.75;

/// What a request costs before any history is added.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FixedCost {
    /// The window the whole request has to fit. `0` means unknown.
    pub context_window_tokens: u64,
    /// The system message, the runtime reminder and the tool schemas.
    pub prompt_tokens: usize,
    /// The completion the request asks room for.
    pub max_output_tokens: u64,
    /// Provider tokens per estimated token, or `None` before the provider has
    /// counted a request on this model.
    pub calibration: Option<f64>,
}

/// The tokens the history may take, in estimate units, or `None` when no
/// window is known.
///
/// The window is in the provider's tokens, so the fixed costs are scaled up to
/// meet it and what is left is scaled back down to the units rows are priced
/// in.
pub fn history_budget(cost: &FixedCost) -> Option<usize> {
    if cost.context_window_tokens == 0 {
        return None;
    }
    let ratio = cost
        .calibration
        .map_or(1.0, |ratio| ratio.max(MIN_BUDGET_RATIO));
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "token counts far below 2^53; the result is clamped at zero first"
    )]
    let budget = {
        let left = cost.context_window_tokens as f64
            - cost.prompt_tokens as f64 * ratio
            - cost.max_output_tokens as f64;
        (left.max(0.0) * HISTORY_BUDGET_FACTOR / ratio) as u64
    };
    Some(usize::try_from(budget).unwrap_or(usize::MAX))
}

/// Provider tokens per estimated token, from the newest step in `records` the
/// provider counted, when that step ran on `model`.
///
/// The newest only, and none when it ran on another model: a different
/// tokenizer's ratio says nothing about this one, and the first request on the
/// new model replaces it.
pub fn calibration_of(records: &[StoredMessageRecord], model: &str) -> Option<f64> {
    let newest = records
        .iter()
        .rev()
        .find_map(|record| match &record.message {
            ChatMessage::Assistant(assistant) => assistant.stats.as_ref()?.calibration.as_ref(),
            _ => None,
        })?;
    (newest.model == model).then(|| newest.ratio())
}

/// Token estimates of stored rows, by row id.
///
/// A stored row never changes, so a turn prices each one once. Without this
/// every iteration serialised the whole window again, twice: once to budget
/// the request and once to report the context.
///
/// An attachment is priced as the request carries it, not as the reference
/// the row stores: the file's text or a vision model's image, read off the
/// part's type and size without touching the disk.
#[derive(Debug)]
pub struct RowPrices {
    by_id: HashMap<String, usize>,
    attachments: MaterialiseOptions,
}

impl Default for RowPrices {
    /// Images priced as a vision model reads them.
    fn default() -> RowPrices {
        RowPrices::new(true)
    }
}

impl RowPrices {
    /// Prices for an agent that does, or does not, send images.
    pub fn new(images: bool) -> RowPrices {
        RowPrices {
            by_id: HashMap::new(),
            attachments: MaterialiseOptions {
                images,
                ..MaterialiseOptions::default()
            },
        }
    }

    /// What `record` costs in a request.
    pub fn of(&mut self, record: &StoredMessageRecord) -> usize {
        if let Some(&tokens) = self.by_id.get(&record.id) {
            return tokens;
        }
        let content = match &record.message {
            ChatMessage::User(user) => user.content.as_slice(),
            ChatMessage::Assistant(assistant) => assistant.content.as_slice(),
            ChatMessage::System(_) | ChatMessage::Tool(_) => &[],
        };
        let attached: usize = content
            .iter()
            .map(|part| match part {
                ContentPart::File(file) => attachment_tokens(file, self.attachments),
                _ => 0,
            })
            .sum();
        let tokens = estimate_message_tokens(&record.message) + attached;
        self.by_id.insert(record.id.clone(), tokens);
        tokens
    }
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
    prices: &mut RowPrices,
) -> usize {
    let floor = match opening_seq {
        None => records.len(),
        Some(seq) => match records.iter().position(|record| record.seq == seq) {
            Some(floor) => floor,
            None => return 0,
        },
    };
    let sizes: Vec<usize> = records.iter().map(|record| prices.of(record)).collect();
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
    let messages: Vec<&ChatMessage> = records[start..]
        .iter()
        .map(|record| &record.message)
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
/// boundaries and the opening row are found by `turn_id` and `seq`. The
/// window `history_for_llm` keeps is a suffix, so its start index is all that
/// is needed to cut the rows.
///
/// # Errors
///
/// When the store cannot be read.
pub fn windowed_history(
    store: &SessionStore,
    session_key: &str,
    opening_seq: Option<i64>,
    budget: Option<usize>,
    prices: &mut RowPrices,
) -> Result<WindowedHistory> {
    let records = read_history(store, session_key)?;
    Ok(window_records(records, opening_seq, budget, prices))
}

/// The rows [`windowed_history`] windows: the newest, up to the message cap.
///
/// The store half on its own, so an async caller can run it on the blocking
/// pool and window the rows after.
///
/// # Errors
///
/// When the store cannot be read.
pub fn read_history(store: &SessionStore, session_key: &str) -> Result<Vec<StoredMessageRecord>> {
    store.messages(
        session_key,
        &ReadMessages {
            after_seq: Some(0),
            limit: Some(DEFAULT_MAX_HISTORY_MESSAGES),
            from_end: true,
            ..ReadMessages::default()
        },
    )
}

/// [`windowed_history`] over rows already read.
pub fn window_records(
    mut records: Vec<StoredMessageRecord>,
    opening_seq: Option<i64>,
    budget: Option<usize>,
    prices: &mut RowPrices,
) -> WindowedHistory {
    let messages: Vec<&ChatMessage> = records.iter().map(|record| &record.message).collect();
    let first = history_window_start(&messages, DEFAULT_MAX_HISTORY_MESSAGES);
    records.drain(..first);
    let start = budget.map_or(0, |budget| {
        token_window_start(&records, opening_seq, budget, prices)
    });
    records.drain(..start);
    WindowedHistory {
        records,
        trimmed: start > 0,
    }
}

impl WindowedHistory {
    /// The messages alone, as a request carries them.
    pub fn messages(&self) -> Vec<ChatMessage> {
        self.records
            .iter()
            .map(|record| record.message.clone())
            .collect()
    }

    /// [`WindowedHistory::messages`], moved out rather than copied.
    pub fn into_messages(self) -> Vec<ChatMessage> {
        self.records
            .into_iter()
            .map(|record| record.message)
            .collect()
    }
}
