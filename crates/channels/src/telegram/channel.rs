//! The Telegram channel: a long poll, an allowlist, and the four things a
//! channel does.
//!
//! Everything difficult is in a neighbouring module — the Bot API in `api.rs`,
//! escaping and chunking in `format.rs`, the command table in `commands.rs`,
//! buttons in `menus.rs`, the outbound policy in `render.rs` — so what is left
//! here is the lifecycle and the routing, which is the part worth being able to
//! read in one sitting.
//!
//! Four decisions are load-bearing:
//!
//!  - **`start()` does not await the loop.** The manager awaits
//!    [`crate::Channel::start`], so a method that ran the poll inline would
//!    never return and the server would never finish booting. It confirms the
//!    token, clears a stale webhook, registers the commands, and *then* spawns
//!    the loop; `stop()` is what awaits it.
//!
//!  - **A bad token fails `start()`.** That is the contract `channel.rs`
//!    states, and it is what makes a wrong credential a startup error rather
//!    than a channel that is silently dead.
//!
//!  - **Switching conversation is publishing a different key.** The manager
//!    derives a session from whatever key arrives, so `/new` and `/session`
//!    change this channel's own map and the next message lands on a different
//!    hub connection. There is no switch frame, and there deliberately is not
//!    one — see [`crate::ChannelControlFrame`].
//!
//!  - **Every inbound path goes through the allowlist**, messages and button
//!    presses alike. A press is an authorisation decision arriving from a
//!    person Telegram will happily let into any group the bot is in.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use darkwire_core::message_bus::{OutboundKind, OutboundMessage, PublishResult};
use darkwire_core::messages::text_part;
use darkwire_core::workspace_store::CreateWorkspace;
use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{ApprovalScope, ContentPart, ToolApproveMessage, ToolApproveTag, ToolRisk};
use parking_lot::Mutex;
use serde_json::{Map, Value};
use tokio::task::JoinHandle;

use crate::channel::{
    BoxFuture, Channel, ChannelContext, ChannelControl, ChannelControlFrame, ChannelFactory,
};
use crate::projection::{APPROVAL_METADATA_KEY, ApprovalDraftDetail};
use crate::telegram::access::{AccessList, Requester};
use crate::telegram::api::{
    BotApi, BotApiError, HttpClient, ReqwestHttpClient, TelegramCallbackQuery, TelegramMessage,
    TelegramUpdate, TelegramUser,
};
use crate::telegram::chats::{ChatBook, ChatState, Pending, default_session_key};
use crate::telegram::commands::{
    CommandInput, CommandResult, bot_commands, parse_command, run_command,
};
use crate::telegram::console::TelegramConsole;
use crate::telegram::menus::{
    CallbackLookup, CallbackPayload, CallbackRefusal, CallbackStore, MenuKind, PickerRow,
    approval_keyboard, confirm_keyboard, picker, picker_keyboard,
};
use crate::telegram::render::{RenderRequest, TelegramRenderer};
use crate::telegram::settings::{TelegramSettings, parse_telegram_settings};

/// Backoff after a failed poll: one second, doubling, capped at a minute.
const FIRST_BACKOFF_MS: u64 = 1000;
const MAX_BACKOFF_MS: u64 = 60_000;

/// How many conversations `/sessions` paging reads at once.
const PAGE_LISTING_LIMIT: usize = 100;

/// Mints the ids `/new` and `/branch` need.
pub type NewId = Arc<dyn Fn() -> String + Send + Sync>;

/// What the composition root supplies to build this channel.
pub struct TelegramChannelOptions {
    /// The bot token, resolved by whoever builds this.
    ///
    /// Not read from the settings block, and not read from the environment
    /// here: a [`ChannelContext`] has no vault by design, and the composition
    /// root is the one place that has both a vault and an environment.
    pub token: String,
    /// The stores and the catalogue the chat commands read.
    pub console: Arc<dyn TelegramConsole>,
    /// Ids for `/new` and `/branch`, injected so a test is not at the mercy of
    /// a uuid.
    pub new_id: NewId,
    /// The transport. Defaults to a real HTTP client; every test supplies its
    /// own.
    pub http: Option<Arc<dyn HttpClient>>,
    /// The id it registers under. A second bot needs a second id.
    pub id: String,
}

impl std::fmt::Debug for TelegramChannelOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the token.
        f.debug_struct("TelegramChannelOptions")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl TelegramChannelOptions {
    /// Options for the channel registered as `telegram`.
    pub fn new(token: impl Into<String>, console: Arc<dyn TelegramConsole>, new_id: NewId) -> Self {
        TelegramChannelOptions {
            token: token.into(),
            console,
            new_id,
            http: None,
            id: "telegram".to_owned(),
        }
    }
}

/// The Telegram channel.
pub struct Telegram {
    id: String,
    me: std::sync::Weak<Telegram>,
    api: Arc<BotApi>,
    settings: TelegramSettings,
    access: AccessList,
    chats: Mutex<ChatBook>,
    menus: CallbackStore,
    renderer: TelegramRenderer,
    console: Arc<dyn TelegramConsole>,
    context: ChannelContext,
    new_id: NewId,
    username: Mutex<Option<String>>,
    /// The poll, so `stop()` can await it rather than racing the cancellation.
    polling: Mutex<Option<JoinHandle<()>>>,
    offset: AtomicI64,
}

impl std::fmt::Debug for Telegram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Telegram")
            .field("id", &self.id)
            .field("username", &*self.username.lock())
            .finish_non_exhaustive()
    }
}

/// `progress` is declared, so a turn fills one message in rather than posting
/// the answer twice. Whether it is *used* is per chat — see `/output`.
const TELEGRAM_ACCEPTS: &[OutboundKind] = &[
    OutboundKind::Reply,
    OutboundKind::Notice,
    OutboundKind::Error,
    OutboundKind::Progress,
];

impl Telegram {
    fn build(context: ChannelContext, options: &TelegramChannelOptions) -> Result<Arc<Telegram>> {
        let settings = parse_telegram_settings(&context.settings)?;
        let access = AccessList::new(&settings.allowlist, &settings.admins)?;
        let http = match &options.http {
            Some(http) => Arc::clone(http),
            None => Arc::new(ReqwestHttpClient::new()?) as Arc<dyn HttpClient>,
        };
        let api = Arc::new(BotApi::new(&options.token, &settings.api_base, http));
        let edit_interval_ms = i64::try_from(settings.edit_interval_ms).unwrap_or(i64::MAX);
        let id = context.id.clone();
        let clock = Arc::clone(&context.clock);
        let console = Arc::clone(&options.console);
        let new_id = Arc::clone(&options.new_id);

        Ok(Arc::new_cyclic(|me| Telegram {
            me: me.clone(),
            renderer: TelegramRenderer::new(
                Arc::clone(&api),
                Arc::clone(&clock),
                edit_interval_ms,
                id.clone(),
            ),
            menus: CallbackStore::new(clock),
            chats: Mutex::new(ChatBook::new(id.clone())),
            id,
            api,
            settings,
            access,
            console,
            context,
            new_id,
            username: Mutex::new(None),
            polling: Mutex::new(None),
            offset: AtomicI64::new(0),
        }))
    }

    /// Live only after `start()`. Exposed so a test can assert the banner.
    pub fn username(&self) -> Option<String> {
        self.username.lock().clone()
    }

    async fn connect(&self) -> Result<()> {
        // Refused rather than started, and the difference matters: a bot that
        // comes up answering nobody looks exactly like a bot with a broken
        // token, and one that comes up answering *anybody* is a shell on this
        // machine.
        if self.access.is_empty() {
            return Err(WireError::new(
                ErrorKind::Config,
                "channels.telegram.allowlist is empty, so this bot would answer nobody. \
                 Add your Telegram user id. Message the bot and read the log line for it.",
            ));
        }

        // A wrong token fails here, which fails the manager's `start()` and so
        // fails `darkwire serve`. That is the documented contract.
        let me = self.api.get_me(&self.context.token).await?;
        (*self.username.lock()).clone_from(&me.username);

        // The commonest 409 is a webhook left over from an earlier setup, and
        // it looks exactly like the serious one — a second process on the same
        // token — with none of the same cause. Clearing it turns that into a
        // non-event.
        self.api.delete_webhook(&self.context.token).await?;
        self.api
            .set_my_commands(&bot_commands(), &self.context.token)
            .await?;

        tracing::info!(
            channel = %self.id,
            username = ?me.username,
            allowed = self.access.members().len(),
            "telegram channel connected"
        );
        Ok(())
    }

    /// `getUpdates` until the manager's token fires.
    ///
    /// The token reaches the request, so a poll that is parked on Telegram's
    /// side unwinds at shutdown rather than holding the process open for its
    /// full timeout.
    async fn poll(self: Arc<Self>) {
        let mut backoff_ms: u64 = 0;

        while !self.context.token.is_cancelled() {
            let updates = self
                .api
                .get_updates(
                    self.offset.load(Ordering::Acquire),
                    self.settings.poll_timeout_sec,
                    &self.context.token,
                )
                .await;

            match updates {
                Ok(updates) => {
                    backoff_ms = 0;
                    for update in updates {
                        // Before handling, not after: an update that failed its
                        // way out of here would otherwise be redelivered
                        // forever.
                        self.offset.store(update.update_id + 1, Ordering::Release);
                        self.handle(update).await;
                    }
                }
                Err(error) => {
                    if self.context.token.is_cancelled() {
                        return;
                    }
                    backoff_ms = if backoff_ms == 0 {
                        FIRST_BACKOFF_MS
                    } else {
                        backoff_ms.saturating_mul(2)
                    }
                    .min(MAX_BACKOFF_MS);
                    self.report_poll_failure(&error, backoff_ms);
                    if darkwire_core::sleep(Duration::from_millis(backoff_ms), &self.context.token)
                        .await
                        .is_err()
                    {
                        // The sleep is cut short only by the shutdown token.
                        return;
                    }
                }
            }
        }
    }

    fn report_poll_failure(&self, error: &BotApiError, backoff_ms: u64) {
        if error
            .api()
            .is_some_and(super::api::TelegramApiError::is_conflict)
        {
            // Survived `deleteWebhook`, so this is a second process polling the
            // same bot. There is no clever recovery — the two would take turns
            // stealing each other's updates — and the log line is the fix.
            tracing::error!(
                channel = %self.id,
                error = %error,
                backoff_ms,
                "another process is polling this bot; only one may"
            );
            return;
        }
        tracing::warn!(channel = %self.id, error = %error, backoff_ms, "telegram poll failed");
    }

    /// One update. Never fails: the poll loop is the only reader of the queue.
    async fn handle(&self, update: TelegramUpdate) {
        let outcome = if let Some(message) = update.message {
            self.on_message(message).await
        } else if let Some(query) = update.callback_query {
            self.on_callback(query).await
        } else {
            Ok(())
        };
        if let Err(error) = outcome {
            tracing::error!(
                channel = %self.id,
                error = %error.message,
                "telegram update could not be handled"
            );
        }
    }

    async fn on_message(&self, message: TelegramMessage) -> Result<()> {
        let (Some(from), Some(text)) = (message.from.clone(), message.text.clone()) else {
            return Ok(());
        };
        if text.is_empty() {
            return Ok(());
        }

        let chat_id = message.chat.id;
        let requester = Requester {
            user_id: from.id,
            chat_id,
        };
        if !self.access.permits(requester) {
            self.refuse(&from, chat_id);
            return Ok(());
        }

        let chat = self.chats.lock().snapshot(chat_id);
        let username = self.username();

        // A question the chat was asked is answered before anything else looks
        // at the message. Ahead of the command parse on purpose: a workspace
        // may perfectly well be called `/help`, and what was asked for was a
        // name, not a command.
        if chat.pending.is_some() {
            let pending = self.chats.lock().take_pending(chat_id);
            if let Some(pending) = pending {
                let answer = self
                    .answer(pending, text.trim())
                    .unwrap_or_else(|error| CommandResult::say(error.message));
                self.say(chat_id, answer).await;
                return Ok(());
            }
        }

        let Some(command) = parse_command(&text, &message.entities, username.as_deref()) else {
            // An ordinary message. The manager stamps the Telegram message id
            // as the frame's idempotency key, so a redelivered update is acked
            // rather than run twice.
            let result = self.context.publish(crate::channel::ChannelInbound {
                session_key: chat.session_key.clone(),
                sender_id: from.id.to_string(),
                content: vec![text_part(text)],
                metadata: target_metadata(chat_id),
                id: Some(format!("{chat_id}:{}", message.message_id)),
            });
            if !matches!(result, PublishResult::Accepted { .. }) {
                self.say_rejected(chat_id, &result).await;
            }
            return Ok(());
        };

        let outcome = self
            .run(
                requester,
                &chat,
                command.name.as_str(),
                command.args,
                command.tail,
            )
            .await;
        self.say(chat_id, outcome).await;
        Ok(())
    }

    /// Builds the command's world and runs it.
    async fn run(
        &self,
        requester: Requester,
        chat: &ChatState,
        name: &str,
        args: Vec<String>,
        tail: String,
    ) -> CommandResult {
        let chat_id = requester.chat_id;
        let session_key = chat.session_key.clone();
        let control = move |frame: ChannelControlFrame| {
            self.control(&session_key, chat_id, frame);
        };
        let attach = move |key: &str| {
            self.chats.lock().attach(chat_id, key);
        };
        let set_pref = move |field: &str, value: bool| {
            let mut book = self.chats.lock();
            let state = book.for_chat(chat_id);
            if field == "progress" {
                state.prefs.progress = value;
            } else {
                state.prefs.markdown = value;
            }
        };
        let new_id = Arc::clone(&self.new_id);
        let mint = move || new_id();

        let input = CommandInput {
            args,
            tail,
            chat_id,
            chat: chat.clone(),
            console: self.console.as_ref(),
            menus: &self.menus,
            channel_id: self.id.clone(),
            // The person typing, not the chat: in a group those differ, and
            // reading the chat id as a user id would hand an admin verb to
            // anybody in a room whose id happens to be on the admin list.
            is_admin: self.access.admits(requester),
            control: &control,
            attach: &attach,
            set_pref: &set_pref,
            new_id: &mint,
        };
        run_command(name, &input).await
    }

    async fn on_callback(&self, query: TelegramCallbackQuery) -> Result<()> {
        let Some(chat_id) = query.message.as_ref().map(|message| message.chat.id) else {
            self.api
                .answer_callback_query(&query.id, None, &self.context.token)
                .await?;
            return Ok(());
        };

        // The same check a message gets. Anybody in a group can press a button
        // the bot posted, so without this an approval is answerable by a
        // stranger.
        if !self.access.permits(Requester {
            user_id: query.from.id,
            chat_id,
        }) {
            self.refuse(&query.from, chat_id);
            self.api
                .answer_callback_query(&query.id, Some("Not for you."), &self.context.token)
                .await?;
            return Ok(());
        }

        let found = self
            .menus
            .take(query.data.as_deref().unwrap_or(""), chat_id);
        let payload = match found {
            CallbackLookup::Found(payload) => payload,
            CallbackLookup::Refused(reason) => {
                let text = match reason {
                    CallbackRefusal::WrongChat => "That button belongs to another chat.",
                    CallbackRefusal::Expired => "That menu has expired. Ask again.",
                };
                self.api
                    .answer_callback_query(&query.id, Some(text), &self.context.token)
                    .await?;
                return Ok(());
            }
        };

        let said = self.apply(payload, chat_id, query.from.id).await?;
        // Always answered, including on a refusal: an unanswered query leaves
        // the button spinning until it times out, which reads as a bot that has
        // hung.
        self.api
            .answer_callback_query(&query.id, None, &self.context.token)
            .await?;
        if let (Some(said), Some(message_id)) = (
            said,
            query.message.as_ref().map(|message| message.message_id),
        ) {
            // The card becomes its own outcome rather than a second message
            // under it.
            let markdown = self.chats.lock().snapshot(chat_id).prefs.markdown;
            self.renderer
                .update(chat_id, message_id, &said, markdown, &self.context.token)
                .await;
        }
        Ok(())
    }

    /// Acts on a pressed button, and says what it did.
    async fn apply(
        &self,
        payload: CallbackPayload,
        chat_id: i64,
        user_id: i64,
    ) -> Result<Option<String>> {
        let session_key = self.chats.lock().snapshot(chat_id).session_key;

        match payload {
            CallbackPayload::Approve {
                call_id,
                session_key: approval_session,
                approved,
                scope,
            } => {
                self.control(
                    &approval_session,
                    chat_id,
                    ChannelControlFrame::ToolApprove(ToolApproveMessage {
                        tag: ToolApproveTag,
                        call_id,
                        approved,
                        scope,
                    }),
                );
                Ok(Some(if approved {
                    format!("Approved {}. Waiting for the agent.", scope_words(scope))
                } else {
                    "Denied.".to_owned()
                }))
            }

            CallbackPayload::Session {
                session_key: chosen,
            } => {
                self.chats.lock().attach(chat_id, &chosen);
                Ok(Some(format!("Attached to `{chosen}`.")))
            }

            CallbackPayload::Agent { agent_id } => {
                self.ensure(&session_key)?;
                self.console.store().update_session(
                    &session_key,
                    darkwire_core::session_store::UpdateSession {
                        agent_id: Some(Some(agent_id.clone())),
                        ..Default::default()
                    },
                )?;
                Ok(Some(format!("This session now runs on `{agent_id}`.")))
            }

            CallbackPayload::Model { model_id } => {
                if !self.access.admits(Requester { user_id, chat_id }) {
                    return Ok(Some(
                        "Choosing a model is for an administrator of this install.".to_owned(),
                    ));
                }
                self.console.set_model(&model_id);
                Ok(Some(format!("Now running `{model_id}`.")))
            }

            CallbackPayload::Workspace { workspace_id } => {
                self.ensure(&session_key)?;
                self.console.store().update_session(
                    &session_key,
                    darkwire_core::session_store::UpdateSession {
                        workspace_id: Some(workspace_id.clone()),
                        ..Default::default()
                    },
                )?;
                Ok(Some(format!("This session now lives in `{workspace_id}`.")))
            }

            // Two more of their own, for the same reason as the workspace six:
            // one of them posts a question and the other answers it.
            CallbackPayload::DeleteAsk { .. } | CallbackPayload::Delete { .. } => {
                self.delete(payload, chat_id, &session_key).await
            }

            CallbackPayload::TasksClear {
                session_key: plan_of,
            } => {
                self.console.store().set_tasks(&plan_of, &[])?;
                Ok(Some("Task list cleared.".to_owned()))
            }
            CallbackPayload::Output { field } => {
                let mut book = self.chats.lock();
                let state = book.for_chat(chat_id);
                let (name, next) = if field == "progress" {
                    state.prefs.progress = !state.prefs.progress;
                    ("progress", state.prefs.progress)
                } else {
                    state.prefs.markdown = !state.prefs.markdown;
                    ("markdown", state.prefs.markdown)
                };
                Ok(Some(format!("{name}: {}", if next { "on" } else { "off" })))
            }

            // Six arms of their own, because `apply` is one match over every
            // button this channel posts and the workspace manager is most of
            // them.
            CallbackPayload::WorkspaceNew
            | CallbackPayload::WorkspaceRename { .. }
            | CallbackPayload::WorkspaceRemoveAsk { .. }
            | CallbackPayload::WorkspaceRemove { .. }
            | CallbackPayload::WorkspaceMoveAsk { .. }
            | CallbackPayload::WorkspaceMove { .. } => self.workspace(payload, chat_id).await,

            CallbackPayload::Page { menu, offset } => self.page(menu, offset, chat_id).await,
        }
    }

    fn ensure(&self, session_key: &str) -> Result<()> {
        self.console.store().ensure_session(
            session_key,
            darkwire_core::session_store::CreateSession {
                origin: Some(self.id.clone()),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    /// What a typed answer to a posted question does.
    ///
    /// A blank line is an answer too, and the answer is "never mind": there is
    /// no way to cancel a `force_reply` except by sending something, so the
    /// empty one has to mean that.
    fn answer(&self, pending: Pending, text: &str) -> Result<CommandResult> {
        if text.is_empty() {
            return Ok(CommandResult::say("Never mind."));
        }
        match pending {
            Pending::NewWorkspace => {
                let created = self.console.workspaces().create(CreateWorkspace {
                    name: text.to_owned(),
                    ..CreateWorkspace::default()
                })?;
                Ok(CommandResult::say(format!("Created `{}`.", created.id)))
            }
            Pending::RenameWorkspace { id } => {
                self.console.workspaces().rename(&id, text)?;
                Ok(CommandResult::say(format!("Renamed `{id}` to “{text}”.")))
            }
        }
    }

    /// Asking whether to delete a conversation, and doing it.
    async fn delete(
        &self,
        payload: CallbackPayload,
        chat_id: i64,
        session_key: &str,
    ) -> Result<Option<String>> {
        match payload {
            CallbackPayload::DeleteAsk {
                session_key: doomed,
                title,
            } => {
                let keyboard = confirm_keyboard(
                    chat_id,
                    &self.menus,
                    CallbackPayload::Delete {
                        session_key: doomed,
                    },
                    None,
                );
                self.render(
                    chat_id,
                    RenderRequest {
                        chat_id,
                        text: format!("Delete “{title}”? This cannot be undone."),
                        kind: OutboundKind::Notice,
                        turn_id: None,
                        keyboard: Some(keyboard),
                        force_reply: false,
                    },
                )
                .await;
                Ok(None)
            }

            CallbackPayload::Delete {
                session_key: doomed,
            } => {
                self.console.store().delete_session(&doomed)?;
                if session_key == doomed {
                    self.chats
                        .lock()
                        .attach(chat_id, default_session_key(&self.id, chat_id));
                }
                Ok(Some("Deleted.".to_owned()))
            }
            // Every other payload is handled by the caller, which is the only
            // thing that routes here.
            _ => Ok(None),
        }
    }

    /// What one of the workspace manager's buttons does.
    ///
    /// Split out of [`Self::apply`] rather than inlined there: that match
    /// covers every button this channel posts, and these six are most of them.
    async fn workspace(&self, payload: CallbackPayload, chat_id: i64) -> Result<Option<String>> {
        match payload {
            // The two a tap cannot do: a name has to be typed, so the button
            // posts a prompt the chat's own keyboard opens on, and the next
            // message answers it. See `Pending`.
            CallbackPayload::WorkspaceNew => {
                self.chats.lock().ask(chat_id, Pending::NewWorkspace);
                self.prompt(chat_id, "What should the new workspace be called?")
                    .await;
                Ok(None)
            }

            CallbackPayload::WorkspaceRename { workspace_id } => {
                let was = self
                    .console
                    .workspaces()
                    .get(&workspace_id)?
                    .map_or_else(|| workspace_id.clone(), |row| row.name);
                self.chats.lock().ask(
                    chat_id,
                    Pending::RenameWorkspace {
                        id: workspace_id.clone(),
                    },
                );
                self.prompt(chat_id, &format!("What should “{was}” be called instead?"))
                    .await;
                Ok(None)
            }

            CallbackPayload::WorkspaceRemoveAsk { workspace_id } => {
                // The refusal before the question rather than after it: a
                // workspace its sessions still name cannot go, and asking
                // first would be asking about something that will not happen.
                let held = self.console.store().count_by_workspace(&workspace_id)?;
                if held > 0 {
                    return Ok(Some(format!(
                        "`{workspace_id}` still holds {held} sessions. Move them first."
                    )));
                }
                let keyboard = confirm_keyboard(
                    chat_id,
                    &self.menus,
                    CallbackPayload::WorkspaceRemove {
                        workspace_id: workspace_id.clone(),
                    },
                    None,
                );
                self.post(
                    chat_id,
                    format!("Detach `{workspace_id}`? The files stay where they are."),
                    Some(keyboard),
                )
                .await;
                Ok(None)
            }

            CallbackPayload::WorkspaceRemove { workspace_id } => {
                self.console.workspaces().delete(&workspace_id)?;
                Ok(Some(format!("Detached `{workspace_id}`.")))
            }

            CallbackPayload::WorkspaceMoveAsk { workspace_id } => {
                let elsewhere: Vec<PickerRow> = self
                    .console
                    .workspaces()
                    .list()?
                    .iter()
                    .filter(|row| row.id != workspace_id)
                    .map(|row| PickerRow {
                        label: row.name.clone(),
                        current: false,
                        payload: CallbackPayload::WorkspaceMove {
                            from: workspace_id.clone(),
                            to: row.id.clone(),
                        },
                    })
                    .collect();
                if elsewhere.is_empty() {
                    return Ok(Some(
                        "There is nowhere else to move them. Make another workspace first."
                            .to_owned(),
                    ));
                }
                let keyboard = picker(&elsewhere, MenuKind::Workspaces, chat_id, &self.menus);
                self.post(
                    chat_id,
                    format!("Move everything in `{workspace_id}` where?"),
                    Some(keyboard),
                )
                .await;
                Ok(None)
            }

            CallbackPayload::WorkspaceMove { from, to } => {
                let moved = self.console.store().reassign_workspace(&from, &to)?;
                Ok(Some(format!("Moved {moved} sessions to `{to}`.")))
            }
            // Every other payload is handled by the caller, which is the only
            // thing that routes here.
            _ => Ok(None),
        }
    }

    /// Posts a message, with buttons under it when there are any.
    async fn post(
        &self,
        chat_id: i64,
        text: String,
        keyboard: Option<crate::telegram::api::InlineKeyboardMarkup>,
    ) {
        self.render(
            chat_id,
            RenderRequest {
                chat_id,
                text,
                kind: OutboundKind::Notice,
                turn_id: None,
                keyboard,
                force_reply: false,
            },
        )
        .await;
    }

    /// Posts a question the chat's keyboard opens on, quoting it.
    async fn prompt(&self, chat_id: i64, text: &str) {
        self.render(
            chat_id,
            RenderRequest {
                chat_id,
                text: text.to_owned(),
                kind: OutboundKind::Notice,
                turn_id: None,
                keyboard: None,
                force_reply: true,
            },
        )
        .await;
    }

    /// Another screen of a listing, rebuilt rather than remembered.
    async fn page(&self, menu: MenuKind, offset: usize, chat_id: i64) -> Result<Option<String>> {
        if !matches!(menu, MenuKind::Sessions | MenuKind::Agents) {
            // `models` and `workspaces` are re-asked rather than paged: both
            // are short enough that a second `/model` costs less than the
            // state.
            return Ok(Some("Ask again for the next page.".to_owned()));
        }

        let chat = self.chats.lock().snapshot(chat_id);
        let rows: Vec<PickerRow> = if menu == MenuKind::Sessions {
            self.console
                .store()
                .list_sessions(&darkwire_core::session_store::ListSessions {
                    origin: Some(self.id.clone()),
                    limit: Some(PAGE_LISTING_LIMIT),
                    ..Default::default()
                })?
                .iter()
                .map(|summary| PickerRow {
                    label: format!(
                        "{} · {}",
                        if summary.session.title.is_empty() {
                            &summary.session.key
                        } else {
                            &summary.session.title
                        },
                        summary.message_count
                    ),
                    current: summary.session.key == chat.session_key,
                    payload: CallbackPayload::Session {
                        session_key: summary.session.key.clone(),
                    },
                })
                .collect()
        } else {
            self.console
                .agents()
                .iter()
                .map(|agent| PickerRow {
                    label: format!("{} · {}", agent.label, agent.model),
                    current: false,
                    payload: CallbackPayload::Agent {
                        agent_id: agent.id.clone(),
                    },
                })
                .collect()
        };

        let keyboard = picker_keyboard(
            &rows,
            menu,
            chat_id,
            &self.menus,
            offset,
            crate::telegram::menus::DEFAULT_PAGE_SIZE,
        );
        self.render(
            chat_id,
            RenderRequest {
                chat_id,
                text: "Which one?".to_owned(),
                kind: OutboundKind::Notice,
                turn_id: None,
                keyboard: Some(keyboard),
                force_reply: false,
            },
        )
        .await;
        Ok(None)
    }

    // Small things

    fn control(&self, session_key: &str, chat_id: i64, frame: ChannelControlFrame) {
        self.context.control(ChannelControl {
            session_key: session_key.to_owned(),
            target: Some(chat_id.to_string()),
            frame,
        });
    }

    /// Renders one request and writes the live-message bookkeeping back.
    async fn render(&self, chat_id: i64, request: RenderRequest) -> Option<i64> {
        let chat = self.chats.lock().snapshot(chat_id);
        let outcome = self
            .renderer
            .render(&request, &chat, &self.context.token)
            .await;
        let mut book = self.chats.lock();
        let state = book.for_chat(chat_id);
        state.live_message_id = outcome.live_message_id;
        state.live_turn_id = outcome.live_turn_id;
        state.last_edit_ms = outcome.last_edit_ms;
        outcome.posted
    }

    async fn say(&self, chat_id: i64, outcome: CommandResult) {
        self.render(
            chat_id,
            RenderRequest {
                chat_id,
                text: outcome.text,
                kind: OutboundKind::Notice,
                turn_id: None,
                keyboard: outcome.keyboard,
                force_reply: false,
            },
        )
        .await;
    }

    async fn say_rejected(&self, chat_id: i64, result: &PublishResult) {
        let text = match result {
            PublishResult::RateLimited { .. } => "Slow down a moment. That was too fast.",
            _ => "Busy right now. Try again shortly.",
        };
        self.render(
            chat_id,
            RenderRequest {
                chat_id,
                text: text.to_owned(),
                kind: OutboundKind::Notice,
                turn_id: None,
                keyboard: None,
                force_reply: false,
            },
        )
        .await;
    }

    /// A stranger, dropped.
    ///
    /// Silently, because a reply confirms the bot is live and spends the rate
    /// limit on whoever is knocking — and logged exactly once per id, because
    /// the ids are theirs to choose and a line each would be a way to fill a
    /// disk. That one line is the whole onboarding path: message the bot, read
    /// the log, add the id, restart.
    fn refuse(&self, from: &TelegramUser, chat_id: i64) {
        if !self.access.should_report(from.id) {
            return;
        }
        tracing::warn!(
            channel = %self.id,
            user_id = from.id,
            username = ?from.username,
            chat_id,
            "telegram message from a sender not on the allowlist"
        );
    }
}

impl Channel for Telegram {
    fn id(&self) -> &str {
        &self.id
    }

    fn accepts(&self) -> &[OutboundKind] {
        TELEGRAM_ACCEPTS
    }

    fn start(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.connect().await?;
            // Not awaited: the manager awaits this method.
            if let Some(me) = self.me.upgrade() {
                *self.polling.lock() = Some(tokio::spawn(me.poll()));
            }
            Ok(())
        })
    }

    fn stop(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let handle = self.polling.lock().take();
            if let Some(handle) = handle {
                // Awaited rather than aborted: the manager's token has already
                // fired, so the loop is on its way out, and letting it finish
                // is what stops a shutdown from racing an in-flight update.
                let _ = handle.await;
            }
            Ok(())
        })
    }

    /// The manager's outbound pump, once per message it decided we accept.
    fn send(&self, message: OutboundMessage) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let Ok(chat_id) = message.target.parse::<i64>() else {
                return Ok(());
            };

            let text: String = message
                .content
                .iter()
                .map(|part| match part {
                    ContentPart::Text(text) => text.text.as_str(),
                    _ => "",
                })
                .collect();

            if let Some(approval) = approval_of(&message.metadata) {
                let keyboard = approval_keyboard(
                    &approval.call_id,
                    &message.session_key,
                    chat_id,
                    &self.menus,
                    i64::try_from(approval.expires_at_ms).unwrap_or(i64::MAX),
                );
                self.render(
                    chat_id,
                    RenderRequest {
                        chat_id,
                        text: approval_text(&text, &approval),
                        kind: message.kind,
                        turn_id: None,
                        keyboard: Some(keyboard),
                        force_reply: false,
                    },
                )
                .await;
                return Ok(());
            }

            self.render(
                chat_id,
                RenderRequest {
                    chat_id,
                    text,
                    kind: message.kind,
                    turn_id: message
                        .metadata
                        .get("turnId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    keyboard: None,
                    force_reply: false,
                },
            )
            .await;
            Ok(())
        })
    }
}

/// Where a reply to this message goes.
fn target_metadata(chat_id: i64) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert("target".to_owned(), Value::String(chat_id.to_string()));
    metadata
}

/// The approval detail the projection put on an outbound message, if it did.
///
/// Read leniently rather than deserialized whole: the metadata bag is untyped
/// by design, and a channel that refused to render an approval because one
/// optional field was the wrong shape would hang the turn it was meant to
/// unblock.
fn approval_of(metadata: &Map<String, Value>) -> Option<ApprovalDraftDetail> {
    let detail = metadata.get(APPROVAL_METADATA_KEY)?.as_object()?;
    let call_id = detail.get("callId")?.as_str()?.to_owned();
    let name = detail.get("name")?.as_str()?.to_owned();
    let risk = detail
        .get("risk")
        .and_then(Value::as_str)
        .and_then(|risk| serde_json::from_value(Value::String(risk.to_owned())).ok())
        .unwrap_or(ToolRisk::Safe);
    Some(ApprovalDraftDetail {
        call_id,
        name,
        risk,
        expires_at_ms: detail
            .get("expiresAtMs")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

/// The card an approval shows.
///
/// The tool and its risk band, and nothing the model wrote. The arguments never
/// leave the projection — see the note there — so there is nothing here to leak
/// into the one place a human is being asked to make a judgement.
fn approval_text(text: &str, approval: &ApprovalDraftDetail) -> String {
    format!(
        "🔐 {text}\n\ntool: `{}` · risk: {}",
        approval.name,
        risk_words(approval.risk)
    )
}

fn risk_words(risk: ToolRisk) -> &'static str {
    match risk {
        ToolRisk::Safe => "safe",
        ToolRisk::Write => "write",
        ToolRisk::Exec => "exec",
        ToolRisk::Network => "network",
    }
}

fn scope_words(scope: ApprovalScope) -> &'static str {
    match scope {
        ApprovalScope::Once => "once",
        ApprovalScope::Session => "for this session",
    }
}

/// The factory the manager registers.
///
/// Built by the composition root, which is the only place with a vault to read
/// the token from and a runtime to build the console over.
pub fn telegram_channel(options: TelegramChannelOptions) -> ChannelFactory {
    let id = options.id.clone();
    let options = Arc::new(options);
    ChannelFactory::new(
        id,
        Arc::new(move |context| {
            Telegram::build(context, &options).map(|channel| channel as Arc<dyn Channel>)
        }),
    )
}
