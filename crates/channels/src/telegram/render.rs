//! Putting one outbound message on Telegram.
//!
//! Separate from the channel because it is a second job: the channel decides
//! *what* to say, and this decides how a chat app can be made to show it. Three
//! constraints shape everything here, and only the first is obvious.
//!
//!  - **4096 characters, and MarkdownV2 or nothing.** `format.rs` owns both; the
//!    one thing added here is the retry. A message Telegram will not parse is
//!    sent again as plain text rather than lost, and that fallback is what lets
//!    the formatter stay small — a gap in it costs formatting, not the answer.
//!
//!  - **A turn owns one message.** `metadata.turnId` is already stamped by the
//!    manager, so a turn's first `progress` is posted and everything after it
//!    edits that message: the answer arrives by filling in rather than by being
//!    repeated twice, once in pieces and once whole. `notice` and `error` always
//!    post fresh, because an answer must not overwrite the warning before it.
//!
//!  - **Every chat shares one delivery chain.** The manager's queues are keyed
//!    by *channel*, not by chat, so a send that slept on a `retry_after` would
//!    stall every other conversation in the install. Nothing here sleeps. A
//!    rate-limited `progress` is dropped — it is disposable by construction, and
//!    the `reply` behind it carries the same text — and anything else is retried
//!    once and then left to fail, which the manager logs and drops.

use std::sync::Arc;

use darkwire_core::clock::Clock;
use darkwire_core::message_bus::OutboundKind;
use tokio_util::sync::CancellationToken;

use crate::telegram::api::{
    BotApi, BotApiError, EditMessageInput, InlineKeyboardMarkup, SendMessageInput,
};
use crate::telegram::chats::ChatState;
use crate::telegram::format::{MAX_MESSAGE_CHARS, chunk_message, strip_markdown, to_markdown_v2};

/// A longer wait than this would hold up every other chat, so it is dropped.
const MAX_RETRY_AFTER_SEC: u64 = 1;

/// One thing to say, in one chat.
#[derive(Debug, Clone, Default)]
pub struct RenderRequest {
    /// Where it goes.
    pub chat_id: i64,
    /// What it says.
    pub text: String,
    /// Why it is being sent.
    pub kind: OutboundKind,
    /// Present for anything scoped to a turn. What groups the edits.
    pub turn_id: Option<String>,
    /// Attached to the last piece, when there is one.
    pub keyboard: Option<InlineKeyboardMarkup>,
    /// Quotes this message in the composer, so the next thing typed is an
    /// answer to it. For the two things a button cannot do: a new name, and a
    /// replacement one.
    pub force_reply: bool,
}

/// What rendering did to the chat's own live-message bookkeeping.
///
/// Returned rather than applied, because the renderer is handed a snapshot of
/// the chat state: the channel owns the book and is the one thing allowed to
/// write it, which is what keeps a lock off the await inside every send.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderOutcome {
    /// The first message id posted, when anything was posted.
    ///
    /// The caller wants that id for a card it will edit later — an approval
    /// becoming "Approved, waiting for the agent" — and gets `None` when the
    /// message was an edit, was skipped, or could not be sent at all.
    pub posted: Option<i64>,
    /// The message a running turn is now being written into.
    pub live_message_id: Option<i64>,
    /// The turn that message belongs to.
    pub live_turn_id: Option<String>,
    /// When that message was last edited.
    pub last_edit_ms: i64,
}

/// Puts messages on Telegram, and decides when not to.
pub struct TelegramRenderer {
    api: Arc<BotApi>,
    clock: Arc<dyn Clock>,
    /// The floor between two edits of one turn's message.
    edit_interval_ms: i64,
    channel_id: String,
}

impl std::fmt::Debug for TelegramRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramRenderer")
            .field("channel", &self.channel_id)
            .field("edit_interval_ms", &self.edit_interval_ms)
            .finish_non_exhaustive()
    }
}

impl TelegramRenderer {
    /// A renderer over one bot.
    pub fn new(
        api: Arc<BotApi>,
        clock: Arc<dyn Clock>,
        edit_interval_ms: i64,
        channel_id: impl Into<String>,
    ) -> TelegramRenderer {
        TelegramRenderer {
            api,
            clock,
            edit_interval_ms,
            channel_id: channel_id.into(),
        }
    }

    /// Says one thing in one chat, and reports what that did to the chat state.
    pub async fn render(
        &self,
        request: &RenderRequest,
        chat: &ChatState,
        token: &CancellationToken,
    ) -> RenderOutcome {
        let mut outcome = RenderOutcome {
            posted: None,
            live_message_id: chat.live_message_id,
            live_turn_id: chat.live_turn_id.clone(),
            last_edit_ms: chat.last_edit_ms,
        };

        if request.kind == OutboundKind::Progress && !chat.prefs.progress {
            return outcome;
        }

        let body = if chat.prefs.markdown {
            to_markdown_v2(&request.text)
        } else {
            request.text.clone()
        };
        let pieces = chunk_message(&body, MAX_MESSAGE_CHARS);

        // One piece, on the message this turn already owns: an edit.
        if pieces.len() == 1 && owns(request, chat) {
            let piece = pieces.first().map_or("", String::as_str);
            self.edit(request, chat, piece, &mut outcome, token).await;
            return outcome;
        }

        // Anything else posts, and whatever the turn owned stops being live —
        // an answer that outgrew one message cannot keep collecting edits into
        // it.
        outcome.live_message_id = None;
        outcome.live_turn_id = None;
        self.post_all(request, chat, &pieces, &mut outcome, token)
            .await;
        outcome
    }

    /// Rewrites a message this channel posted earlier. For a settled card.
    pub async fn update(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        markdown: bool,
        token: &CancellationToken,
    ) {
        let result = self
            .api
            .edit_message_text(
                &EditMessageInput {
                    chat_id,
                    message_id,
                    text: if markdown {
                        to_markdown_v2(text)
                    } else {
                        text.to_owned()
                    },
                    markdown,
                    reply_markup: None,
                },
                token,
            )
            .await;
        if let Err(error) = result {
            if error
                .api()
                .is_some_and(super::api::TelegramApiError::is_not_modified)
            {
                return;
            }
            self.warn("telegram card update failed", &error);
        }
    }

    async fn post_all(
        &self,
        request: &RenderRequest,
        chat: &ChatState,
        pieces: &[String],
        outcome: &mut RenderOutcome,
        token: &CancellationToken,
    ) {
        let last = pieces.len().saturating_sub(1);
        for (index, piece) in pieces.iter().enumerate() {
            let posted = self
                .post(
                    request,
                    chat,
                    piece,
                    // Only the last piece carries the buttons, so a keyboard is
                    // not repeated once per chunk of a long card.
                    if index == last {
                        request.keyboard.clone()
                    } else {
                        None
                    },
                    token,
                )
                .await;
            if index == 0 {
                outcome.posted = posted;
            }
        }

        // A `progress` claims the message it just posted, so the rest of the
        // turn fills it in. A `reply` never does: the turn is over.
        if request.kind == OutboundKind::Progress
            && pieces.len() == 1
            && let Some(first) = outcome.posted
            && let Some(turn_id) = &request.turn_id
        {
            outcome.live_message_id = Some(first);
            outcome.live_turn_id = Some(turn_id.clone());
            outcome.last_edit_ms = self.clock.now_ms();
        }
    }

    async fn post(
        &self,
        request: &RenderRequest,
        chat: &ChatState,
        text: &str,
        keyboard: Option<InlineKeyboardMarkup>,
        token: &CancellationToken,
    ) -> Option<i64> {
        match self
            .send(
                request.chat_id,
                text,
                chat.prefs.markdown,
                keyboard.as_ref(),
                request.force_reply,
                token,
            )
            .await
        {
            Ok(message_id) => Some(message_id),
            Err(error) => {
                if !self.retryable(&error, request.kind) {
                    return None;
                }
                // Deliberately without a parse mode. The commonest reason a
                // message is refused is a construct the formatter mis-escaped,
                // and re-sending the same bytes would be refused the same way.
                match self
                    .send(
                        request.chat_id,
                        text,
                        false,
                        keyboard.as_ref(),
                        request.force_reply,
                        token,
                    )
                    .await
                {
                    Ok(message_id) => Some(message_id),
                    Err(retried) => {
                        self.warn("telegram send failed", &retried);
                        None
                    }
                }
            }
        }
    }

    async fn send(
        &self,
        chat_id: i64,
        text: &str,
        markdown: bool,
        keyboard: Option<&InlineKeyboardMarkup>,
        force_reply: bool,
        token: &CancellationToken,
    ) -> Result<i64, BotApiError> {
        let message = self
            .api
            .send_message(
                &SendMessageInput {
                    chat_id,
                    text: if markdown {
                        text.to_owned()
                    } else {
                        strip_markdown(text)
                    },
                    markdown,
                    reply_markup: keyboard.cloned(),
                    force_reply,
                },
                token,
            )
            .await?;
        Ok(message.message_id)
    }

    async fn edit(
        &self,
        request: &RenderRequest,
        chat: &ChatState,
        text: &str,
        outcome: &mut RenderOutcome,
        token: &CancellationToken,
    ) {
        let Some(message_id) = chat.live_message_id else {
            return;
        };

        // Telegram allows roughly one message per second per chat. A `reply`
        // always lands, because it is the answer; an intermediate `progress` is
        // skipped rather than queued, since the next one carries everything it
        // did.
        if request.kind == OutboundKind::Progress
            && self.clock.now_ms() - chat.last_edit_ms < self.edit_interval_ms
        {
            return;
        }

        let result = self
            .api
            .edit_message_text(
                &EditMessageInput {
                    chat_id: request.chat_id,
                    message_id,
                    text: if chat.prefs.markdown {
                        text.to_owned()
                    } else {
                        strip_markdown(text)
                    },
                    markdown: chat.prefs.markdown,
                    reply_markup: None,
                },
                token,
            )
            .await;
        match result {
            Ok(()) => outcome.last_edit_ms = self.clock.now_ms(),
            // Identical text is normal rather than a fault: a delta that added
            // no visible characters re-renders to the same string.
            Err(error) => {
                if !error
                    .api()
                    .is_some_and(super::api::TelegramApiError::is_not_modified)
                {
                    self.warn("telegram edit failed", &error);
                }
            }
        }

        if request.kind == OutboundKind::Reply {
            outcome.live_message_id = None;
            outcome.live_turn_id = None;
        }
    }

    /// Whether to try once more, or let this one go.
    ///
    /// A rate limit longer than a second is never waited out: the sleep would
    /// sit on the chain every other conversation is queued behind.
    fn retryable(&self, error: &BotApiError, kind: OutboundKind) -> bool {
        let Some(api) = error.api() else {
            return false;
        };
        if let Some(retry_after) = api.retry_after_sec
            && retry_after > MAX_RETRY_AFTER_SEC
        {
            tracing::warn!(
                channel = %self.channel_id,
                kind = ?kind,
                retry_after_sec = retry_after,
                "telegram rate limited; dropped rather than stalling every chat"
            );
            return false;
        }
        kind != OutboundKind::Progress
    }

    fn warn(&self, message: &str, error: &BotApiError) {
        // Structured, and never the request URL — it carries the bot token.
        tracing::warn!(channel = %self.channel_id, error = %error, "{message}");
    }
}

/// Whether this message belongs to the turn the live message is holding.
fn owns(request: &RenderRequest, chat: &ChatState) -> bool {
    matches!(request.kind, OutboundKind::Reply | OutboundKind::Progress)
        && request.turn_id.is_some()
        && chat.live_turn_id == request.turn_id
        && chat.live_message_id.is_some()
}
