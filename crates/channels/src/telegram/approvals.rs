//! The approval cards this channel posted and can still edit.
//!
//! A card is answered in one of three places: its own buttons, another client
//! on the same conversation, or its deadline. The projection says when the
//! second and third happen. This remembers which message to edit when it does,
//! and what the card said, so a refused rule can put the buttons back.

use std::fmt::Write as _;
use std::sync::Arc;

use darkwire_core::Clock;
use darkwire_protocol::{CommandPolicy, ExecRule, ToolRisk};
use darkwire_security::{format_argv, preferred_rule};
use indexmap::IndexMap;
use parking_lot::Mutex;

use crate::projection::ApprovalDraftDetail;

/// How many open cards the process remembers at once.
///
/// A card normally settles within its deadline, so this only bounds a flood of
/// prompts nothing answered and nothing settled.
pub const MAX_APPROVAL_CARDS: usize = 200;

/// The longest command a card shows.
///
/// A card can land in a group chat, and a command can carry a token on its
/// command line. The cap keeps most of a secret out of the room.
pub const MAX_COMMAND_CHARS: usize = 80;

/// The longest rule a button shows. Telegram cuts a long button label anyway.
const MAX_RULE_CHARS: usize = 32;

/// One posted card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalCard {
    /// Where it was posted.
    pub chat_id: i64,
    /// The message to edit.
    pub message_id: i64,
    /// The conversation the answer goes to.
    pub session_key: String,
    /// What the projection said about the call.
    pub detail: ApprovalDraftDetail,
    /// The card's text as posted, before any outcome replaced it.
    pub text: String,
    /// What the card should end on, once a button here answered it.
    pub answered: Option<String>,
}

/// The open cards, oldest first.
pub struct ApprovalCards {
    clock: Arc<dyn Clock>,
    cards: Mutex<IndexMap<String, ApprovalCard>>,
}

impl std::fmt::Debug for ApprovalCards {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalCards")
            .field("open", &self.cards.lock().len())
            .finish_non_exhaustive()
    }
}

impl ApprovalCards {
    /// None open.
    pub fn new(clock: Arc<dyn Clock>) -> ApprovalCards {
        ApprovalCards {
            clock,
            cards: Mutex::new(IndexMap::new()),
        }
    }

    /// Remembers a card, dropping any past its deadline and then the oldest.
    pub fn put(&self, card: ApprovalCard) {
        let now = u64::try_from(self.clock.now_ms()).unwrap_or(0);
        let mut cards = self.cards.lock();
        cards.retain(|_, open| open.detail.expires_at_ms > now);
        cards.insert(card.detail.call_id.clone(), card);
        while cards.len() > MAX_APPROVAL_CARDS {
            cards.shift_remove_index(0);
        }
    }

    /// The card for `call_id`, left open.
    pub fn get(&self, call_id: &str) -> Option<ApprovalCard> {
        self.cards.lock().get(call_id).cloned()
    }

    /// The card for `call_id`, now closed.
    pub fn take(&self, call_id: &str) -> Option<ApprovalCard> {
        self.cards.lock().shift_remove(call_id)
    }

    /// Records what a button here answered, and whether it knew the card.
    pub fn answer(&self, call_id: &str, outcome: Option<String>) -> bool {
        self.cards
            .lock()
            .get_mut(call_id)
            .map(|card| card.answered = outcome)
            .is_some()
    }

    /// Cards open. Exposed for the bound test.
    pub fn len(&self) -> usize {
        self.cards.lock().len()
    }

    /// Whether none are open.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The card an approval shows.
///
/// The tool, its risk band and, for `exec`, the command capped at
/// [`MAX_COMMAND_CHARS`]. Nothing else the model wrote.
pub fn approval_text(text: &str, approval: &ApprovalDraftDetail) -> String {
    let mut card = format!(
        "🔐 {text}\n\ntool: `{}` · risk: {}",
        approval.name,
        risk_words(approval.risk)
    );
    if let Some(command) = &approval.command {
        let _ = write!(
            card,
            "\ncommand: `{}`",
            code_span(&command.argv, MAX_COMMAND_CHARS)
        );
    }
    card
}

/// The rule "Always" saves for this command, when one may be saved from here.
///
/// A shell is never offered one, because a rule for it would cover every
/// program. The server refuses one anyway.
pub fn offered_rule(command: Option<&CommandPolicy>) -> Option<ExecRule> {
    let command = command?;
    if command.shell {
        return None;
    }
    preferred_rule(&command.argv)
}

/// A rule as a button shows it.
pub fn rule_label(rule: &ExecRule) -> String {
    format!(
        "✅ Always: {}",
        truncate(&format_argv(&rule.argv), MAX_RULE_CHARS)
    )
}

/// An argv inside backticks: capped, and with no backtick of its own to end
/// the span early.
fn code_span(argv: &[String], max: usize) -> String {
    truncate(&format_argv(argv), max).replace('`', "'")
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

fn risk_words(risk: ToolRisk) -> &'static str {
    match risk {
        ToolRisk::Safe => "safe",
        ToolRisk::Write => "write",
        ToolRisk::Exec => "exec",
        ToolRisk::Network => "network",
    }
}
