//! The Bot API, as much of it as a chat channel needs.
//!
//! Hand-written over an HTTP seam, the way `darkwire-providers` writes the
//! `openai-chat` adapter, and for the same two reasons: the surface actually
//! used here is seven methods, and a dependency that wraps the whole API brings
//! its own update loop, its own session store and its own opinion about
//! middleware — none of which can be reconciled with a `ChannelManager` that
//! already owns the lifecycle.
//!
//! Three things in here are load-bearing:
//!
//!  - **The token is in the URL.** `api.telegram.org/bot<token>/sendMessage` is
//!    the Bot API's design, not a choice, and it means a logged request URL is a
//!    leaked credential. Nothing in this file logs a URL, and [`TelegramApiError`]
//!    carries the *method* rather than the address it was called at.
//!  - **The transport is injected.** Every channel test drives a fake, so no
//!    test opens a socket and the error paths — 429, 409, a body that is not
//!    JSON — are reachable without a network.
//!  - **A Telegram failure is a value, not a status code.** The API answers
//!    `200 {ok: false}` as readily as it answers `400`, so the unwrapping below
//!    reads `ok` rather than the HTTP status, and `retry_after` is lifted out of
//!    `parameters` where the rate limiter puts it.

use std::time::Duration;

use darkwire_core::{ErrorKind, WireError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::channel::BoxFuture;

/// One HTTP answer, narrowed to what this file reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// The status line.
    pub status: u16,
    /// The body, unparsed. Telegram answers JSON; a proxy may not.
    pub body: String,
}

/// The transport one [`BotApi`] speaks over.
///
/// Declared here rather than imported: the guarded fetch lives in
/// `darkwire-security`, which this crate does not depend on and should not start
/// depending on for one trait.
pub trait HttpClient: Send + Sync {
    /// Posts a JSON body and reads the answer whole.
    ///
    /// The token is threaded all the way in: a long poll parked on Telegram's
    /// side has to unwind at shutdown rather than hold the process open for its
    /// full timeout.
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        body: String,
        token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<HttpResponse, WireError>>;
}

/// The real transport.
#[derive(Debug, Clone)]
pub struct ReqwestHttpClient {
    client: reqwest::Client,
}

impl ReqwestHttpClient {
    /// A client whose request timeout leaves room for the longest long poll.
    ///
    /// Telegram holds `getUpdates` open for up to fifty seconds, so a timeout
    /// anywhere near that would turn every idle poll into an error and a
    /// backoff.
    pub fn new() -> Result<ReqwestHttpClient, WireError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_mins(2))
            .build()
            .map_err(|error| {
                WireError::new(
                    ErrorKind::Network,
                    format!("the Telegram HTTP client could not be built: {error}"),
                )
            })?;
        Ok(ReqwestHttpClient { client })
    }
}

impl HttpClient for ReqwestHttpClient {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        body: String,
        token: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<HttpResponse, WireError>> {
        Box::pin(async move {
            let request = self
                .client
                .post(url)
                .header("content-type", "application/json")
                .body(body);
            let response = tokio::select! {
                () = token.cancelled() => return Err(WireError::aborted("Telegram request")),
                response = request.send() => response,
            };
            let response = response.map_err(|error| {
                WireError::new(
                    ErrorKind::Network,
                    format!("the Telegram API could not be reached: {}", why(&error)),
                )
            })?;
            let status = response.status().as_u16();
            let body = tokio::select! {
                () = token.cancelled() => return Err(WireError::aborted("Telegram request")),
                body = response.text() => body,
            };
            let body = body.map_err(|error| {
                WireError::new(
                    ErrorKind::Network,
                    format!("the Telegram response could not be read: {}", why(&error)),
                )
            })?;
            Ok(HttpResponse { status, body })
        })
    }
}

/// Why a request failed, without saying where it was sent.
///
/// `reqwest`'s own message carries the URL, and the URL carries the bot token —
/// so neither the message nor a `source` chain may be passed through. The class
/// of failure is what a reader needs anyway; the address adds nothing they did
/// not configure themselves.
fn why(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "the request timed out"
    } else if error.is_connect() {
        "the connection failed"
    } else if error.is_decode() {
        "the response could not be decoded"
    } else if error.is_body() {
        "the body could not be read"
    } else {
        "the request failed"
    }
}

/// What the Bot API said went wrong.
///
/// Its own type rather than a [`WireError`] because two of the three things
/// read off it are Telegram's wire and nobody else's: a 409 that means a second
/// poller, and the `message is not modified` description that an edit changing
/// nothing earns. Converting to a `WireError` at the channel boundary is what
/// keeps "never branch on a message substring" true of everything above this
/// file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramApiError {
    /// Telegram's `error_code`, or the HTTP status when it sent no body.
    pub code: u16,
    /// Seconds to wait, when this was a rate limit.
    pub retry_after_sec: Option<u64>,
    /// The API method, never the URL — the URL contains the bot token.
    pub method: String,
    /// What Telegram said.
    pub description: String,
}

impl std::fmt::Display for TelegramApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Telegram {} failed ({}): {}",
            self.method, self.code, self.description
        )
    }
}

impl std::error::Error for TelegramApiError {}

impl TelegramApiError {
    /// A second poller on the same token, or a webhook still registered.
    pub fn is_conflict(&self) -> bool {
        self.code == 409
    }

    /// The token is wrong or revoked. Fatal at startup, by design.
    pub fn is_unauthorized(&self) -> bool {
        self.code == 401
    }

    /// An edit that changed nothing.
    ///
    /// Normal rather than exceptional: a turn whose last delta added no visible
    /// text re-renders to the same string, and Telegram calls that a 400. The
    /// only signal it gives is the description, so this is the one place in the
    /// crate that reads one — and it stays here, on Telegram's own error type,
    /// rather than reaching a `WireError` where the rule is that nothing
    /// branches on a message.
    pub fn is_not_modified(&self) -> bool {
        self.description.contains("message is not modified")
    }
}

impl From<TelegramApiError> for WireError {
    fn from(error: TelegramApiError) -> WireError {
        let kind = match error.code {
            401 | 403 => ErrorKind::PermissionDenied,
            409 => ErrorKind::Conflict,
            429 => ErrorKind::RateLimited,
            _ => ErrorKind::Network,
        };
        let mut wire = WireError::new(kind, error.to_string())
            .with_detail("method", error.method)
            .with_detail("code", i64::from(error.code));
        if let Some(retry_after) = error.retry_after_sec {
            wire = wire.with_detail(
                "retryAfterSec",
                i64::try_from(retry_after).unwrap_or(i64::MAX),
            );
        }
        wire
    }
}

/// Either Telegram's own failure, or the transport's.
#[derive(Debug)]
pub enum BotApiError {
    /// The Bot API answered, and said no.
    Api(TelegramApiError),
    /// The request never got an answer.
    Transport(WireError),
}

impl BotApiError {
    /// The Telegram failure, when that is what this was.
    pub fn api(&self) -> Option<&TelegramApiError> {
        match self {
            BotApiError::Api(error) => Some(error),
            BotApiError::Transport(_) => None,
        }
    }

    /// Whether this was the abort a shutdown produces.
    pub fn is_aborted(&self) -> bool {
        matches!(self, BotApiError::Transport(error) if error.is_aborted())
    }
}

impl std::fmt::Display for BotApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BotApiError::Api(error) => error.fmt(f),
            BotApiError::Transport(error) => f.write_str(&error.message),
        }
    }
}

impl From<BotApiError> for WireError {
    fn from(error: BotApiError) -> WireError {
        match error {
            BotApiError::Api(api) => api.into(),
            BotApiError::Transport(transport) => transport,
        }
    }
}

/// What a Bot API call answers.
pub type BotResult<T> = Result<T, BotApiError>;

// The slice of Telegram's types this channel reads

/// A Telegram account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TelegramUser {
    /// The numeric id, which is what the allowlist matches on.
    pub id: i64,
    /// The `@name`, when the account has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// A chat: one person, a group, or a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramChat {
    /// The numeric id. Negative for a group.
    pub id: i64,
    /// `private`, `group`, `supergroup`, `channel`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// One of Telegram's own parses of a message's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramMessageEntity {
    /// `bot_command`, `mention`, `url`, …
    #[serde(rename = "type")]
    pub kind: String,
    /// Where it starts, in UTF-16 code units.
    pub offset: u32,
    /// How long it is, in UTF-16 code units.
    pub length: u32,
}

/// A message, as much of one as this channel reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramMessage {
    /// The id an edit addresses.
    pub message_id: i64,
    /// Where it was sent.
    pub chat: TelegramChat,
    /// Who sent it. Absent on a channel post.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<TelegramUser>,
    /// The text, when it has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Telegram's own parse of that text.
    #[serde(default)]
    pub entities: Vec<TelegramMessageEntity>,
}

/// A button press.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramCallbackQuery {
    /// The id `answerCallbackQuery` quotes back.
    pub id: String,
    /// Who pressed it.
    pub from: TelegramUser,
    /// The token the button carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// The message the button is attached to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<TelegramMessage>,
}

/// One item from `getUpdates`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramUpdate {
    /// The cursor the next poll advances past.
    pub update_id: i64,
    /// A message somebody sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<TelegramMessage>,
    /// A button somebody pressed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_query: Option<TelegramCallbackQuery>,
}

/// One button. `callback_data` is capped at 64 *bytes* by Telegram.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineKeyboardButton {
    /// What it says.
    pub text: String,
    /// The token it carries.
    pub callback_data: String,
}

/// A grid of buttons under a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct InlineKeyboardMarkup {
    /// Rows, each a row of buttons.
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

/// One entry of Telegram's own `/` menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotCommand {
    /// Lowercase, no slash, no spaces.
    pub command: String,
    /// Under 256 characters, which `setMyCommands` enforces.
    pub description: String,
}

/// Telegram's one formatting mode this channel uses.
pub const PARSE_MODE: &str = "MarkdownV2";

/// What one `sendMessage` carries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SendMessageInput {
    /// Where it goes.
    pub chat_id: i64,
    /// What it says.
    pub text: String,
    /// `false` for the plain-text retry, which omits the field entirely.
    pub markdown: bool,
    /// The buttons under it, when it has any.
    pub reply_markup: Option<InlineKeyboardMarkup>,
    /// Opens the chat's keyboard with this message quoted, so the next thing
    /// typed is plainly an answer to it.
    ///
    /// The one thing a button cannot do is supply a name. Mutually exclusive
    /// with `reply_markup` in the API, and here too: a prompt asking to be
    /// typed into has nothing to tap.
    pub force_reply: bool,
}

/// What one `editMessageText` carries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EditMessageInput {
    /// Where it is.
    pub chat_id: i64,
    /// Which message.
    pub message_id: i64,
    /// What it should say now.
    pub text: String,
    /// `false` for the plain-text retry.
    pub markdown: bool,
    /// The buttons under it, when it has any.
    pub reply_markup: Option<InlineKeyboardMarkup>,
}

/// Reads `{ok, result, description, parameters}` without trusting any of it.
fn unwrap(method: &str, status: u16, body: &str) -> BotResult<Value> {
    let Ok(parsed) = serde_json::from_str::<Value>(body) else {
        // A proxy's HTML error page, or a truncated body. The status is all
        // there is to report, and it is more use than a JSON parse error.
        return Err(BotApiError::Api(TelegramApiError {
            code: status,
            retry_after_sec: None,
            method: method.to_owned(),
            description: "the response body was not JSON".to_owned(),
        }));
    };

    let envelope = parsed.as_object().cloned().unwrap_or_default();
    if envelope.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(envelope.get("result").cloned().unwrap_or(Value::Null));
    }

    let retry_after = envelope
        .get("parameters")
        .and_then(Value::as_object)
        .and_then(|parameters| parameters.get("retry_after"))
        .and_then(Value::as_u64);

    Err(BotApiError::Api(TelegramApiError {
        // Telegram answers `200 {ok: false}` as happily as it answers `400`, so
        // its own code wins when there is one.
        code: envelope
            .get("error_code")
            .and_then(Value::as_u64)
            .and_then(|code| u16::try_from(code).ok())
            .unwrap_or(status),
        retry_after_sec: retry_after,
        method: method.to_owned(),
        description: envelope
            .get("description")
            .and_then(Value::as_str)
            .map_or_else(|| format!("HTTP {status}"), str::to_owned),
    }))
}

/// The Bot API over one token.
pub struct BotApi {
    token: String,
    api_base: String,
    http: std::sync::Arc<dyn HttpClient>,
}

impl std::fmt::Debug for BotApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the token, and never a URL built from it.
        f.debug_struct("BotApi")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl BotApi {
    /// The API rooted at `api_base`, speaking over `http`.
    pub fn new(
        token: impl Into<String>,
        api_base: &str,
        http: std::sync::Arc<dyn HttpClient>,
    ) -> BotApi {
        BotApi {
            token: token.into(),
            api_base: api_base.trim_end_matches('/').to_owned(),
            http,
        }
    }

    /// Confirms the token and gives us the username to strip from commands.
    pub async fn get_me(&self, token: &CancellationToken) -> BotResult<TelegramUser> {
        let result = self.call("getMe", &json!({}), token).await?;
        decode("getMe", result)
    }

    /// Clears a webhook left over from an earlier setup.
    ///
    /// Called once at start, because a registered webhook makes every
    /// `getUpdates` a 409 — and that particular 409 looks exactly like the
    /// serious one (a second process polling the same bot) with none of the
    /// same cause.
    pub async fn delete_webhook(&self, token: &CancellationToken) -> BotResult<()> {
        self.call("deleteWebhook", &json!({}), token).await?;
        Ok(())
    }

    /// Registers the `/` menu.
    pub async fn set_my_commands(
        &self,
        commands: &[BotCommand],
        token: &CancellationToken,
    ) -> BotResult<()> {
        self.call("setMyCommands", &json!({ "commands": commands }), token)
            .await?;
        Ok(())
    }

    /// The long poll.
    pub async fn get_updates(
        &self,
        offset: i64,
        timeout_sec: u32,
        token: &CancellationToken,
    ) -> BotResult<Vec<TelegramUpdate>> {
        let result = self
            .call(
                "getUpdates",
                &json!({
                    "offset": offset,
                    "timeout": timeout_sec,
                    // Everything else — edits, channel posts, join events — is
                    // traffic this channel has no answer for, and asking for it
                    // only makes the offset advance over updates nobody reads.
                    "allowed_updates": ["message", "callback_query"],
                }),
                token,
            )
            .await?;
        // A result that is not a list is not an error worth unwinding a poll
        // loop for: the next poll asks again from the same offset.
        Ok(serde_json::from_value(result).unwrap_or_default())
    }

    /// Posts one message.
    pub async fn send_message(
        &self,
        input: &SendMessageInput,
        token: &CancellationToken,
    ) -> BotResult<TelegramMessage> {
        let mut body = Map::new();
        body.insert("chat_id".to_owned(), json!(input.chat_id));
        body.insert("text".to_owned(), json!(input.text));
        if input.markdown {
            body.insert("parse_mode".to_owned(), json!(PARSE_MODE));
        }
        if let Some(markup) = &input.reply_markup {
            body.insert("reply_markup".to_owned(), json!(markup));
        } else if input.force_reply {
            body.insert(
                "reply_markup".to_owned(),
                json!({ "force_reply": true, "selective": true }),
            );
        }
        let result = self
            .call("sendMessage", &Value::Object(body), token)
            .await?;
        decode("sendMessage", result)
    }

    /// Rewrites one message.
    pub async fn edit_message_text(
        &self,
        input: &EditMessageInput,
        token: &CancellationToken,
    ) -> BotResult<()> {
        let mut body = Map::new();
        body.insert("chat_id".to_owned(), json!(input.chat_id));
        body.insert("message_id".to_owned(), json!(input.message_id));
        body.insert("text".to_owned(), json!(input.text));
        if input.markdown {
            body.insert("parse_mode".to_owned(), json!(PARSE_MODE));
        }
        if let Some(markup) = &input.reply_markup {
            body.insert("reply_markup".to_owned(), json!(markup));
        }
        self.call("editMessageText", &Value::Object(body), token)
            .await?;
        Ok(())
    }

    /// Answers a button press.
    ///
    /// Always called, including on a refusal: an unanswered `callback_query`
    /// leaves the button spinning in the client until it times out, which reads
    /// as a bot that has hung rather than one that said no.
    pub async fn answer_callback_query(
        &self,
        id: &str,
        text: Option<&str>,
        token: &CancellationToken,
    ) -> BotResult<()> {
        let mut body = Map::new();
        body.insert("callback_query_id".to_owned(), json!(id));
        if let Some(text) = text {
            body.insert("text".to_owned(), json!(text));
        }
        self.call("answerCallbackQuery", &Value::Object(body), token)
            .await?;
        Ok(())
    }

    async fn call(
        &self,
        method: &str,
        body: &Value,
        token: &CancellationToken,
    ) -> BotResult<Value> {
        let url = format!("{}/bot{}/{method}", self.api_base, self.token);
        let response = self
            .http
            .post_json(&url, body.to_string(), token)
            .await
            .map_err(BotApiError::Transport)?;
        unwrap(method, response.status, &response.body)
    }
}

/// Reads one of Telegram's own shapes out of a `result`.
fn decode<T: serde::de::DeserializeOwned>(method: &str, result: Value) -> BotResult<T> {
    serde_json::from_value(result).map_err(|error| {
        BotApiError::Api(TelegramApiError {
            code: 0,
            retry_after_sec: None,
            method: method.to_owned(),
            description: format!("the response was not the shape {method} promises: {error}"),
        })
    })
}
