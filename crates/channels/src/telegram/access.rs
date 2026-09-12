//! Who is allowed to drive this install from Telegram.
//!
//! This is the security boundary, and it is worth being blunt about what is on
//! the other side of it: a bot username is discoverable, and behind this bot is
//! an agent with the operator's credentials, their workspace and — depending on
//! the permission map — `exec`. An unguarded bot is a remote shell that anybody
//! who guesses `@something_bot` can reach.
//!
//! So the rules here are the strict ones:
//!
//!  - **An empty allowlist denies everyone, and the channel refuses to start.**
//!    Deny-by-default is only half of it: a channel that started and silently
//!    answered nobody would look identical to a broken token, so the refusal is
//!    what turns a misconfiguration into a sentence at startup.
//!  - **The sender is checked, not the chat.** In a group, being in the room is
//!    not permission — the group has to be listed *and* so does the person
//!    typing. Checking the chat alone would hand the agent to everyone else in
//!    it.
//!  - **A button press is checked exactly like a message.** Anyone in a group
//!    can tap a button the bot posted, so an approval answered from an inline
//!    keyboard is an authorisation decision arriving from an unauthenticated
//!    source unless it goes through here too.
//!
//! It is its own module, rather than a few lines inside the channel, so that
//! the crate's coverage gate measures it on its own and a new branch here has
//! to bring a test with it.

use std::collections::HashSet;

use ghostai_core::{ErrorKind, GhostError, Result};
use parking_lot::Mutex;

/// The most refused ids that earn a log line before the channel goes quiet.
///
/// Bounded, because the ids arriving here are chosen by whoever is knocking.
const MAX_REPORTED: usize = 1000;

/// One parsed allowlist entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedParty {
    /// The Telegram id. Positive for a user, negative for a group.
    pub id: i64,
    /// Whatever the operator wrote after the pipe. For logs, never matched on.
    pub label: Option<String>,
}

/// Who is asking, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requester {
    /// `from.id` — the person, on a message or on a button press.
    pub user_id: i64,
    /// `chat.id` — equal to `user_id` in a private chat, negative in a group.
    pub chat_id: i64,
}

/// Reads `<id>` or `<id>|<label>`.
///
/// A malformed entry fails rather than being skipped. Skipping it would narrow
/// the allowlist by one without saying so, and the symptom — one person the bot
/// has stopped answering — is a long way from the typo that caused it.
pub fn parse_allowlist(entries: &[String]) -> Result<Vec<AllowedParty>> {
    entries
        .iter()
        .map(|entry| {
            let (raw_id, rest) = match entry.split_once('|') {
                Some((head, tail)) => (head, tail),
                None => (entry.as_str(), ""),
            };
            let id: i64 = raw_id.trim().parse().map_err(|_| malformed(entry))?;
            if id == 0 {
                return Err(malformed(entry));
            }
            let label = rest.trim();
            Ok(AllowedParty {
                id,
                label: (!label.is_empty()).then(|| label.to_owned()),
            })
        })
        .collect()
}

fn malformed(entry: &str) -> GhostError {
    GhostError::new(
        ErrorKind::Config,
        format!(
            "channels.telegram.allowlist entry \"{entry}\" is not a Telegram id. \
             Use the numeric id, optionally as \"<id>|<label>\"."
        ),
    )
}

/// The allowlist, as the channel consults it.
///
/// Built once at startup so a malformed entry fails there rather than on the
/// first message from the person it was meant to admit.
#[derive(Debug)]
pub struct AccessList {
    allowed: HashSet<i64>,
    admin_ids: HashSet<i64>,
    parties: Vec<AllowedParty>,
    /// Ids already logged, so a stranger cannot fill the disk. Capped.
    reported: Mutex<HashSet<i64>>,
}

impl AccessList {
    /// Parses both lists, or says which entry is wrong.
    pub fn new(allowlist: &[String], admins: &[String]) -> Result<AccessList> {
        let parties = parse_allowlist(allowlist)?;
        let allowed = parties.iter().map(|party| party.id).collect();
        let admin_ids = parse_allowlist(admins)?
            .into_iter()
            .map(|party| party.id)
            .collect();
        Ok(AccessList {
            allowed,
            admin_ids,
            parties,
            reported: Mutex::new(HashSet::new()),
        })
    }

    /// Everyone on the list, for the startup log line.
    pub fn members(&self) -> &[AllowedParty] {
        &self.parties
    }

    /// Whether the list admits nobody at all.
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }

    /// Whether this request may be acted on at all.
    ///
    /// A private chat needs only the sender. A group needs the group *and* the
    /// sender, because membership of a room is not a decision the operator
    /// made.
    pub fn permits(&self, requester: Requester) -> bool {
        if !self.allowed.contains(&requester.user_id) {
            return false;
        }
        let is_group = requester.chat_id != requester.user_id;
        !is_group || self.allowed.contains(&requester.chat_id)
    }

    /// Whether this request may run a command that reaches past its own chat.
    ///
    /// An empty admin list means every allowed sender is one, so the
    /// distinction costs a single-operator install nothing.
    pub fn admits(&self, requester: Requester) -> bool {
        if !self.permits(requester) {
            return false;
        }
        self.admin_ids.is_empty() || self.admin_ids.contains(&requester.user_id)
    }

    /// Whether this refusal is the first from that id, and so worth a log line.
    ///
    /// Silence is the right reply to a stranger — answering confirms the bot is
    /// live and spends the rate limit on them — but silence with no trace leaves
    /// the operator no way to admit the person they actually meant to. One line
    /// per id is the whole onboarding path: message the bot, read the log, add
    /// the id, restart.
    pub fn should_report(&self, user_id: i64) -> bool {
        let mut reported = self.reported.lock();
        if reported.len() >= MAX_REPORTED || reported.contains(&user_id) {
            return false;
        }
        reported.insert(user_id);
        true
    }
}
