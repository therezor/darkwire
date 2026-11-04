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
//!    failed `progress` is dropped, since it is disposable by construction and
//!    the `reply` behind it carries the same text. Anything else that meets a
//!    long rate limit or a dropped connection comes back as [`Deferred`], and
//!    the channel retries it on that chat's own backlog.

use std::sync::Arc;

use darkwire_core::clock::Clock;
use darkwire_core::message_bus::OutboundKind;
use tokio_util::sync::CancellationToken;

use crate::telegram::api::{
    BotApi, BotApiError, EditMessageInput, InlineKeyboardMarkup, SendMessageInput,
};
use crate::telegram::chats::ChatState;
use crate::telegram::format::{MAX_MESSAGE_CHARS, chunk_message, strip_markdown, to_markdown_v2};

/// A longer wait than this would hold up every other chat, so it is deferred.
const MAX_RETRY_AFTER_SEC: u64 = 1;

/// How long to wait after a request got no answer at all.
const TRANSPORT_BACKOFF_MS: u64 = 2_000;

/// What could not be posted yet, and when to try again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferred {
    /// What Telegram asked for, or a short backoff after a dropped connection.
    pub after_ms: u64,
    /// The pieces still to post, formatted as they were, in order.
    pub pieces: Vec<String>,
}

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
    /// The turn's pieces posted before its live message.
    pub live_settled: usize,
    /// When that message was last edited.
    pub last_edit_ms: i64,
    /// What is left to post once the chat may be written to again.
    pub deferred: Option<Deferred>,
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
        let mut outcome = Self::unchanged(chat);

        if request.kind == OutboundKind::Progress && !chat.prefs.progress {
            return outcome;
        }

        let body = if chat.prefs.markdown {
            to_markdown_v2(&request.text)
        } else {
            request.text.clone()
        };
        let pieces = chunk_message(&body, MAX_MESSAGE_CHARS);

        // The turn already owns a message, which holds piece `settled`; the ones
        // before it are posted and final.
        let settled = chat.live_settled;
        if owns(request, chat) && pieces.len() > settled {
            if pieces.len() == settled + 1 {
                self.edit(request, chat, &pieces[settled], &mut outcome, token)
                    .await;
                return outcome;
            }
            // The answer outgrew its message: finish that one and post only
            // what is new, rather than the whole answer again. A progress
            // inside the debounce waits for the next one, which carries it.
            if request.kind == OutboundKind::Progress
                && self.clock.now_ms() - chat.last_edit_ms < self.edit_interval_ms
            {
                return outcome;
            }
            if let Some(message_id) = chat.live_message_id {
                let finished = self
                    .edit_text(
                        request.chat_id,
                        message_id,
                        &pieces[settled],
                        chat.prefs.markdown,
                        token,
                    )
                    .await;
                if let Err(error) = finished
                    && !error
                        .api()
                        .is_some_and(super::api::TelegramApiError::is_not_modified)
                {
                    self.warn("telegram edit failed", &error);
                }
            }
            outcome.live_message_id = None;
            outcome.live_turn_id = None;
            self.post_all(request, chat, &pieces[settled + 1..], &mut outcome, token)
                .await;
            if outcome.live_message_id.is_some() {
                outcome.live_settled = pieces.len() - 1;
            }
            return outcome;
        }

        // Anything else posts. A progress keeps its last piece live, so the
        // rest of the turn fills that one in.
        outcome.live_message_id = None;
        outcome.live_turn_id = None;
        outcome.live_settled = 0;
        self.post_all(request, chat, &pieces, &mut outcome, token)
            .await;
        if outcome.live_message_id.is_some() {
            outcome.live_settled = pieces.len().saturating_sub(1);
        }
        outcome
    }

    /// Posts the pieces a [`Deferred`] left, in order.
    pub async fn resume(
        &self,
        request: &RenderRequest,
        chat: &ChatState,
        pieces: &[String],
        token: &CancellationToken,
    ) -> RenderOutcome {
        let mut outcome = Self::unchanged(chat);
        self.post_all(request, chat, pieces, &mut outcome, token)
            .await;
        outcome
    }

    fn unchanged(chat: &ChatState) -> RenderOutcome {
        RenderOutcome {
            posted: None,
            live_message_id: chat.live_message_id,
            live_turn_id: chat.live_turn_id.clone(),
            live_settled: chat.live_settled,
            last_edit_ms: chat.last_edit_ms,
            deferred: None,
        }
    }

    /// Rewrites a message this channel posted earlier. For a settled card, or
    /// one whose buttons come back.
    pub async fn update(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        markdown: bool,
        keyboard: Option<InlineKeyboardMarkup>,
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
                    reply_markup: keyboard,
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
        let mut last_posted = None;
        for (index, piece) in pieces.iter().enumerate() {
            let posted = match self
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
                .await
            {
                Posted::Sent(id) => Some(id),
                Posted::Dropped => None,
                // The rest waits with it, so the pieces still land in order.
                Posted::Later(after_ms) => {
                    outcome.deferred = Some(Deferred {
                        after_ms,
                        pieces: pieces[index..].to_vec(),
                    });
                    break;
                }
            };
            if index == 0 {
                outcome.posted = posted;
            }
            last_posted = posted;
        }

        // A `progress` claims the last message it posted, so the rest of the
        // turn fills it in. A `reply` never does: the turn is over.
        if request.kind == OutboundKind::Progress
            && let Some(newest) = last_posted
            && let Some(turn_id) = &request.turn_id
        {
            outcome.live_message_id = Some(newest);
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
    ) -> Posted {
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
            Ok(message_id) => Posted::Sent(message_id),
            Err(error) => {
                if let Some(after_ms) = later(&error, request.kind) {
                    return Posted::Later(after_ms);
                }
                if !retryable(&error, request.kind) {
                    return Posted::Dropped;
                }
                // Deliberately without a parse mode. The commonest reason a
                // message is refused is a construct the formatter mis-escaped,
                // and re-sending the same bytes would be refused the same way.
                match self
                    .send(
                        request.chat_id,
                        &plain(text, chat.prefs.markdown),
                        false,
                        keyboard.as_ref(),
                        request.force_reply,
                        token,
                    )
                    .await
                {
                    Ok(message_id) => Posted::Sent(message_id),
                    Err(retried) => {
                        if let Some(after_ms) = later(&retried, request.kind) {
                            return Posted::Later(after_ms);
                        }
                        self.warn("telegram send failed", &retried);
                        Posted::Dropped
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
                    text: text.to_owned(),
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
            .edit_text(
                request.chat_id,
                message_id,
                text,
                chat.prefs.markdown,
                token,
            )
            .await;
        match result {
            Ok(()) => outcome.last_edit_ms = self.clock.now_ms(),
            // Identical text is normal rather than a fault: a delta that added
            // no visible characters re-renders to the same string.
            Err(error)
                if error
                    .api()
                    .is_some_and(super::api::TelegramApiError::is_not_modified) => {}
            Err(error) if request.kind == OutboundKind::Reply => {
                self.land_reply(request, chat, text, &error, outcome, token)
                    .await;
            }
            Err(error) => self.warn("telegram edit failed", &error),
        }

        if request.kind == OutboundKind::Reply {
            outcome.live_message_id = None;
            outcome.live_turn_id = None;
        }
    }

    async fn edit_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        markdown: bool,
        token: &CancellationToken,
    ) -> Result<(), BotApiError> {
        self.api
            .edit_message_text(
                &EditMessageInput {
                    chat_id,
                    message_id,
                    text: text.to_owned(),
                    markdown,
                    reply_markup: None,
                },
                token,
            )
            .await
    }

    /// Makes sure a turn's answer arrives when the edit that carried it was
    /// refused. Left alone, the chat would keep the last progress text as if it
    /// were the answer.
    ///
    /// The same fallback `post` has, then one more: a message that cannot be
    /// edited at all (deleted, too old) gets the answer as a new message.
    async fn land_reply(
        &self,
        request: &RenderRequest,
        chat: &ChatState,
        text: &str,
        error: &BotApiError,
        outcome: &mut RenderOutcome,
        token: &CancellationToken,
    ) {
        if let Some(after_ms) = later(error, request.kind) {
            outcome.deferred = Some(Deferred {
                after_ms,
                pieces: vec![text.to_owned()],
            });
            return;
        }
        if !retryable(error, request.kind) {
            self.warn("telegram edit failed", error);
            return;
        }
        let Some(message_id) = chat.live_message_id else {
            return;
        };
        let retried = self
            .edit_text(
                request.chat_id,
                message_id,
                &plain(text, chat.prefs.markdown),
                false,
                token,
            )
            .await;
        match retried {
            Ok(()) => {
                outcome.last_edit_ms = self.clock.now_ms();
                return;
            }
            Err(retried)
                if retried
                    .api()
                    .is_some_and(super::api::TelegramApiError::is_not_modified) =>
            {
                return;
            }
            Err(retried) => self.warn("telegram edit failed; posting the answer instead", &retried),
        }
        self.post_all(request, chat, &[text.to_owned()], outcome, token)
            .await;
    }

    fn warn(&self, message: &str, error: &BotApiError) {
        // Structured, and never the request URL — it carries the bot token.
        tracing::warn!(channel = %self.channel_id, error = %error, "{message}");
    }
}

/// How long to leave a send that may work later, or `None` to settle it now.
/// A long rate limit is never waited out here: the sleep would sit on the
/// chain every other conversation is queued behind.
fn later(error: &BotApiError, kind: OutboundKind) -> Option<u64> {
    if kind == OutboundKind::Progress {
        return None;
    }
    match error.api() {
        None => Some(TRANSPORT_BACKOFF_MS),
        Some(api) => api
            .retry_after_sec
            .filter(|seconds| *seconds > MAX_RETRY_AFTER_SEC)
            .map(|seconds| seconds.saturating_mul(1000)),
    }
}

/// Whether to try once more at once, as plain text.
fn retryable(error: &BotApiError, kind: OutboundKind) -> bool {
    error.api().is_some() && kind != OutboundKind::Progress
}

/// How one send ended.
enum Posted {
    Sent(i64),
    Dropped,
    /// Worth trying again after this many milliseconds.
    Later(u64),
}

/// The text to send without a parse mode. Only a body that came out of
/// `to_markdown_v2` carries escapes to undo; plain text is sent as it stands.
fn plain(text: &str, escaped: bool) -> String {
    if escaped {
        strip_markdown(text)
    } else {
        text.to_owned()
    }
}

/// Whether this message belongs to the turn the live message is holding.
fn owns(request: &RenderRequest, chat: &ChatState) -> bool {
    matches!(request.kind, OutboundKind::Reply | OutboundKind::Progress)
        && request.turn_id.is_some()
        && chat.live_turn_id == request.turn_id
        && chat.live_message_id.is_some()
}
