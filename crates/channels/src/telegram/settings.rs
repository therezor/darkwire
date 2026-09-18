//! `channels.telegram`, parsed by the channel that reads it.
//!
//! The channels config is deliberately a loose object precisely so this can
//! live here rather than in `darkwire-protocol`: a channel owns its own block,
//! and a bad one is reported by refusing to start rather than by behaving oddly
//! later.
//!
//! The bot token is deliberately **not** here. It is resolved by whoever builds
//! the factory, from the credential vault first — a `ChannelContext` has no
//! vault and no environment by design, and a token is the one setting that
//! should not be sitting in a world-readable JSON file.

use darkwire_core::{ErrorKind, Result, WireError};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Telegram's own ceiling on a long poll is 50 seconds. Thirty is long enough
/// that the bot is not re-asking all day and short enough that `stop()` is not
/// waiting on it.
const DEFAULT_POLL_TIMEOUT_SEC: u32 = 30;

/// Telegram allows roughly one message per second per chat, and every channel
/// shares one delivery chain — so an edit storm in one conversation is a stall
/// in all of them.
const DEFAULT_EDIT_INTERVAL_MS: u64 = 2000;

/// Overridden only by a test or a proxy.
const DEFAULT_API_BASE: &str = "https://api.telegram.org";

/// The longest long poll Telegram will hold open.
const MAX_POLL_TIMEOUT_SEC: u32 = 50;

/// A parsed `channels.telegram` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramSettings {
    /// Whether the manager starts this channel at all.
    pub enabled: bool,
    /// Who may talk to this bot: `<telegramId>` or `<telegramId>|<label>`.
    ///
    /// One list for both kinds of id, which works because Telegram numbers them
    /// apart: a user id is positive and a group id is negative. A negative entry
    /// admits that group, and inside one **both** have to be listed — the group
    /// and the person typing. A list of chats alone would hand the agent to
    /// everyone else in the room.
    ///
    /// The label half is for whoever reads the config file and the logs;
    /// nothing matches on it.
    pub allowlist: Vec<String>,
    /// Who may run the commands that reach past their own conversation.
    ///
    /// `/model` moves this process onto another model for *every* surface, and
    /// `/workspace rm|move` rewrites where sessions live. Empty means everyone
    /// on the allowlist, so a single-operator install never notices this
    /// exists.
    pub admins: Vec<String>,
    /// The agent a conversation started here is bound to.
    pub agent_id: Option<String>,
    /// The workspace a conversation started here is created in.
    pub workspace_id: Option<String>,
    /// How long one `getUpdates` waits before answering empty.
    pub poll_timeout_sec: u32,
    /// The floor between two edits of the same turn's message.
    pub edit_interval_ms: u64,
    /// The Bot API root.
    pub api_base: String,
}

impl Default for TelegramSettings {
    fn default() -> TelegramSettings {
        TelegramSettings {
            enabled: true,
            allowlist: Vec::new(),
            admins: Vec::new(),
            agent_id: None,
            workspace_id: None,
            poll_timeout_sec: DEFAULT_POLL_TIMEOUT_SEC,
            edit_interval_ms: DEFAULT_EDIT_INTERVAL_MS,
            api_base: DEFAULT_API_BASE.to_owned(),
        }
    }
}

/// The block as it arrives, before the ranges are checked.
///
/// Unknown keys are ignored rather than refused: the channels config is a loose
/// object, and an operator whose file carries a key from a newer build should
/// get a running bot rather than a refusal.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSettings {
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    allowlist: Vec<String>,
    #[serde(default)]
    admins: Vec<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default = "default_poll_timeout")]
    poll_timeout_sec: u32,
    #[serde(default = "default_edit_interval")]
    edit_interval_ms: u64,
    #[serde(default = "default_api_base")]
    api_base: String,
}

fn yes() -> bool {
    true
}

fn default_poll_timeout() -> u32 {
    DEFAULT_POLL_TIMEOUT_SEC
}

fn default_edit_interval() -> u64 {
    DEFAULT_EDIT_INTERVAL_MS
}

fn default_api_base() -> String {
    DEFAULT_API_BASE.to_owned()
}

fn unusable(detail: impl std::fmt::Display) -> WireError {
    WireError::new(
        ErrorKind::Config,
        format!("channels.telegram is not usable: {detail}"),
    )
}

/// Every entry of a list is non-empty, or the list names which field is wrong.
fn require_non_empty(field: &str, entries: &[String]) -> Result<()> {
    if entries.iter().any(String::is_empty) {
        return Err(unusable(format!(
            "{field}: every entry must be a Telegram id"
        )));
    }
    Ok(())
}

/// An optional string is absent or non-empty, never present and blank.
fn require_named(field: &str, value: Option<&String>) -> Result<()> {
    if value.is_some_and(String::is_empty) {
        return Err(unusable(format!("{field}: must not be empty")));
    }
    Ok(())
}

/// Reads the block, or says what is wrong with it.
///
/// Fails rather than falling back to defaults: a channel that quietly ignored a
/// misspelled `allowlist` would come up answering nobody, or — worse, if the
/// misspelling were the other way — answering everybody.
pub fn parse_telegram_settings(settings: &Map<String, Value>) -> Result<TelegramSettings> {
    let value = Value::Object(settings.clone());
    let raw: RawSettings = serde_path_to_error::deserialize(&value).map_err(|error| {
        let path = error.path().to_string();
        let path = if path.is_empty() {
            "(root)".to_owned()
        } else {
            path
        };
        unusable(format!("{path}: {}", error.into_inner()))
    })?;

    require_non_empty("allowlist", &raw.allowlist)?;
    require_non_empty("admins", &raw.admins)?;
    require_named("agentId", raw.agent_id.as_ref())?;
    require_named("workspaceId", raw.workspace_id.as_ref())?;

    if raw.poll_timeout_sec < 1 || raw.poll_timeout_sec > MAX_POLL_TIMEOUT_SEC {
        return Err(unusable(format!(
            "pollTimeoutSec: must be between 1 and {MAX_POLL_TIMEOUT_SEC}"
        )));
    }
    if raw.api_base.is_empty() {
        return Err(unusable("apiBase: must not be empty"));
    }

    Ok(TelegramSettings {
        enabled: raw.enabled,
        allowlist: raw.allowlist,
        admins: raw.admins,
        agent_id: raw.agent_id,
        workspace_id: raw.workspace_id,
        poll_timeout_sec: raw.poll_timeout_sec,
        edit_interval_ms: raw.edit_interval_ms,
        api_base: raw.api_base,
    })
}
