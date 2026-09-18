//! The terminal's slash commands, as bot commands.
//!
//! One table, three readers: `/help` renders it, `setMyCommands` registers it so
//! Telegram's own `/` menu lists it, and [`run_command`] dispatches on it. That
//! is the discipline the CLI's own command table already holds, and for the same
//! reason: a second list beside this one eventually disagrees with it, and the
//! symptom is a command Telegram offers that does nothing. Each entry carries
//! its own handler, so there is no name here that dispatch could fail to know.
//!
//! **Nothing here sends anything.** A command returns text, optionally a
//! keyboard, and the channel does the sending — so a command is a pure-ish
//! function over a store and can be tested without a transport. It is the same
//! split the terminal makes, where `/edit` hands content back rather than
//! running a turn itself.
//!
//! Three commands differ from their terminal spelling on purpose, and each says
//! why at its own definition: `/exit`, `/output` and the admin-gated verbs.

use darkwire_core::messages::text_of;
use darkwire_core::session_store::{
    CreateSession, ForkSession, ListSessions, ReadMessages, SessionSummaryRecord, UpdateSession,
};
use darkwire_core::workspace_store::CreateWorkspace;
use darkwire_core::{ErrorKind, Result, SessionStore, WireError};
use darkwire_protocol::{
    EditMessage, EditTag, RegenerateMessage, RegenerateTag, StopTurnMessage, StopTurnTag,
};

use crate::channel::{BoxFuture, ChannelControlFrame};
use crate::telegram::api::{BotCommand, InlineKeyboardMarkup};
use crate::telegram::chats::{ChatState, default_session_key, new_session_key, owns_session_key};
use crate::telegram::console::TelegramConsole;
use crate::telegram::menus::{
    CallbackPayload, CallbackStore, MenuKind, PickerRow, confirm_keyboard, picker,
};

/// Rows `/messages` and `/stats` show when no count is given.
const DEFAULT_LINES: usize = 12;
/// How far back a negative message reference looks for what you said.
const LOOKBACK: usize = 400;
/// How much of a message body a listing shows before it stops.
const CLIP: usize = 90;

/// A frame on this chat's conversation. The channel supplies the envelope.
pub type ControlSink<'a> = &'a (dyn Fn(ChannelControlFrame) + Send + Sync);
/// Points the chat at another conversation.
pub type AttachSink<'a> = &'a (dyn Fn(&str) + Send + Sync);
/// Toggles one of the chat's rendering preferences.
pub type PrefSink<'a> = &'a (dyn Fn(&str, bool) + Send + Sync);
/// A fresh id, injected so a test is not at the mercy of a uuid.
pub type IdSink<'a> = &'a (dyn Fn() -> String + Send + Sync);

/// Everything one command may reach.
pub struct CommandInput<'a> {
    /// Whitespace-split arguments, without the command word.
    pub args: Vec<String>,
    /// Everything after the command word, untouched. `/rename` wants this.
    pub tail: String,
    /// The chat it was typed in.
    pub chat_id: i64,
    /// The chat's state, as of the moment the command arrived.
    ///
    /// A snapshot rather than a handle: a command that held a borrow of the
    /// chat book would hold it across every `await` in here, and the channel
    /// has to be able to render a reply into the same book meanwhile.
    pub chat: ChatState,
    /// The stores and the model catalogue.
    pub console: &'a dyn TelegramConsole,
    /// Where a keyboard's tokens are filed.
    pub menus: &'a CallbackStore,
    /// The id this channel publishes under.
    pub channel_id: String,
    /// Whether this sender may run a command that reaches past their own chat.
    pub is_admin: bool,
    /// A frame on this chat's conversation.
    pub control: ControlSink<'a>,
    /// Points the chat at another conversation.
    pub attach: AttachSink<'a>,
    /// Toggles one rendering preference.
    pub set_pref: PrefSink<'a>,
    /// A fresh id.
    pub new_id: IdSink<'a>,
}

impl std::fmt::Debug for CommandInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandInput")
            .field("args", &self.args)
            .field("tail", &self.tail)
            .field("chat_id", &self.chat_id)
            .field("is_admin", &self.is_admin)
            .finish_non_exhaustive()
    }
}

impl CommandInput<'_> {
    fn store(&self) -> &SessionStore {
        self.console.store()
    }

    fn session_key(&self) -> &str {
        &self.chat.session_key
    }

    fn arg(&self, index: usize) -> Option<&str> {
        self.args.get(index).map(String::as_str)
    }
}

/// What a command wants said.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandResult {
    /// The reply.
    pub text: String,
    /// The buttons under it, when it has any.
    pub keyboard: Option<InlineKeyboardMarkup>,
}

impl CommandResult {
    /// A plain reply.
    fn say(text: impl Into<String>) -> CommandResult {
        CommandResult {
            text: text.into(),
            keyboard: None,
        }
    }

    /// A reply with buttons under it.
    fn menu(text: impl Into<String>, keyboard: InlineKeyboardMarkup) -> CommandResult {
        CommandResult {
            text: text.into(),
            keyboard: Some(keyboard),
        }
    }
}

/// One command's handler.
type Run = for<'a> fn(&'a CommandInput<'a>) -> BoxFuture<'a, Result<CommandResult>>;

/// One row of the table.
struct TelegramCommand {
    /// Telegram's own spelling: lowercase, no slash, no spaces.
    name: &'static str,
    /// Extra words the parser reads, shown in `/help`. Empty when there are
    /// none.
    usage: &'static str,
    /// Registered with `setMyCommands`, so it must stay under 256 characters.
    description: &'static str,
    /// Reaches past this chat, so a non-admin is refused at run time.
    admin: bool,
    /// Aliases that dispatch here. Not registered with Telegram.
    aliases: &'static [&'static str],
    /// What it does.
    run: Run,
}

// Reading arguments

fn positive(value: Option<&str>, fallback: usize) -> usize {
    value
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|parsed| *parsed > 0)
        .unwrap_or(fallback)
}

fn clip(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= CLIP {
        return flat;
    }
    let head: String = flat.chars().take(CLIP - 1).collect();
    format!("{head}…")
}

fn invalid(message: impl Into<String>) -> WireError {
    WireError::new(ErrorKind::InvalidInput, message)
}

fn missing(message: impl Into<String>) -> WireError {
    WireError::new(ErrorKind::NotFound, message)
}

/// A message reference, as a `seq`.
///
/// The terminal's rule, reimplemented rather than imported: it lives in the CLI
/// crate, which this one cannot reach, and moving it down into `darkwire-core`
/// would touch the CLI for no gain to the CLI. Twenty lines is the cheaper of
/// the two.
///
/// A negative reference counts back over **user messages only** — `-1` is the
/// last thing you said, not the last row, because rows include assistant turns
/// and tool results and nobody counts backwards over a tool result.
pub fn resolve_seq(
    store: &SessionStore,
    session_key: &str,
    reference: Option<&str>,
) -> Result<i64> {
    let trimmed = reference.unwrap_or("").trim().to_owned();
    let raw: i64 = if trimmed.is_empty() {
        -1
    } else {
        trimmed.parse().map_err(|_| {
            invalid(format!(
                "Not a message reference: {trimmed}. \
                 Use a seq from /messages, or -1 for your last message."
            ))
        })?
    };
    if raw == 0 {
        return Err(invalid(
            "Not a message reference: 0. \
             Use a seq from /messages, or -1 for your last message."
                .to_owned(),
        ));
    }

    if raw > 0 {
        let window = store.messages(
            session_key,
            &ReadMessages {
                after_seq: Some(raw - 1),
                before_seq: Some(raw + 1),
                ..ReadMessages::default()
            },
        )?;
        if window.is_empty() {
            return Err(missing(format!("No message {raw} in this session.")));
        }
        return Ok(raw);
    }

    let spoken: Vec<_> = store
        .messages(
            session_key,
            &ReadMessages {
                limit: Some(LOOKBACK),
                from_end: true,
                ..ReadMessages::default()
            },
        )?
        .into_iter()
        .filter(|record| matches!(record.message, darkwire_protocol::ChatMessage::User(_)))
        .collect();

    let from_end = usize::try_from(-raw).unwrap_or(usize::MAX);
    let index = spoken.len().checked_sub(from_end);
    match index.and_then(|index| spoken.get(index)) {
        Some(record) => Ok(record.seq),
        None if spoken.is_empty() => Err(missing(
            "You have not said anything in this session yet.".to_owned(),
        )),
        None => Err(missing(format!(
            "Only {} of your messages are in this session.",
            spoken.len()
        ))),
    }
}

/// The conversations this channel owns, newest first.
fn own_sessions(input: &CommandInput<'_>, limit: usize) -> Result<Vec<SessionSummaryRecord>> {
    // By origin rather than by key prefix: the hub records the channel as the
    // session's origin, so this is an indexed column rather than a scan with a
    // `LIKE`.
    input.store().list_sessions(&ListSessions {
        origin: Some(input.channel_id.clone()),
        limit: Some(limit),
        ..ListSessions::default()
    })
}

fn title_of(session: &SessionSummaryRecord) -> String {
    if session.session.title.is_empty() {
        session.session.key.clone()
    } else {
        session.session.title.clone()
    }
}

/// The session's own workspace, or the one it would be created in.
fn workspace_of(input: &CommandInput<'_>) -> Result<String> {
    Ok(input
        .store()
        .get_session(input.session_key())?
        .map_or_else(|| "default".to_owned(), |session| session.workspace_id))
}

/// Creates the row if it is not there yet, then patches it.
fn ensure_then(input: &CommandInput<'_>, patch: UpdateSession) -> Result<()> {
    input.store().ensure_session(
        input.session_key(),
        CreateSession {
            origin: Some(input.channel_id.clone()),
            ..CreateSession::default()
        },
    )?;
    input.store().update_session(input.session_key(), patch)?;
    Ok(())
}

// The table

/// Wraps a synchronous handler as the boxed future the table stores.
macro_rules! sync_command {
    ($name:ident, $body:expr) => {
        fn $name<'a>(input: &'a CommandInput<'a>) -> BoxFuture<'a, Result<CommandResult>> {
            let run: fn(&CommandInput<'_>) -> Result<CommandResult> = $body;
            Box::pin(std::future::ready(run(input)))
        }
    };
}

sync_command!(run_help, |input| Ok(CommandResult::say(helped(
    input.is_admin
))));

// Telegram opens every first conversation with this, and until it existed the
// first thing a new chat ever saw was "No command `/start`". It is `/help` with
// a sentence in front rather than a second listing, because two lists disagree
// eventually.
sync_command!(run_start, |input| Ok(CommandResult::say(format!(
    "This chat is a DarkWire session. Send a message and the agent answers; \
     the conversation is kept, so you can pick it up later.\n\n{}",
    helped(input.is_admin)
))));

sync_command!(run_messages, |input| {
    let count = positive(input.arg(0), DEFAULT_LINES);
    let rows = input.store().messages(
        input.session_key(),
        &ReadMessages {
            limit: Some(count),
            from_end: true,
            ..ReadMessages::default()
        },
    )?;
    if rows.is_empty() {
        return Ok(CommandResult::say("Nothing said here yet."));
    }
    Ok(CommandResult::say(
        rows.iter()
            .map(|row| {
                format!(
                    "`{}` {}: {}",
                    row.seq,
                    role_of(&row.message),
                    clip(&text_of(&row.message))
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    ))
});

sync_command!(run_clear, |input| {
    input.store().clear_messages(input.session_key())?;
    Ok(CommandResult::say("History cleared."))
});

// Not "exit": there is no process to leave. Detaching is the analogous act —
// the next message starts somewhere fresh — and it keeps the command in
// Telegram's menu doing something rather than nothing.
sync_command!(run_exit, |input| {
    input.menus.forget(input.chat_id);
    (input.attach)(&default_session_key(&input.channel_id, input.chat_id));
    Ok(CommandResult::say(
        "Detached. The next message starts here again.",
    ))
});

sync_command!(run_sessions, |input| {
    let sessions = own_sessions(input, positive(input.arg(0), 20))?;
    if sessions.is_empty() {
        return Ok(CommandResult::say("No sessions here yet."));
    }
    let rows: Vec<PickerRow> = sessions
        .iter()
        .map(|session| PickerRow {
            label: format!("{} · {}", title_of(session), session.message_count),
            current: session.session.key == input.chat.session_key,
            payload: CallbackPayload::Session {
                session_key: session.session.key.clone(),
            },
        })
        .collect();
    Ok(CommandResult::menu(
        "Which session?",
        picker(&rows, MenuKind::Sessions, input.chat_id, input.menus),
    ))
});

sync_command!(run_new, |input| {
    let key = new_session_key(&input.channel_id, input.chat_id, &(input.new_id)());
    input.store().ensure_session(
        &key,
        CreateSession {
            origin: Some(input.channel_id.clone()),
            title: (!input.tail.is_empty()).then(|| input.tail.clone()),
            ..CreateSession::default()
        },
    )?;
    (input.attach)(&key);
    Ok(CommandResult::say(if input.tail.is_empty() {
        "Started a new session.".to_owned()
    } else {
        format!("Started “{}”.", input.tail)
    }))
});

sync_command!(run_session, |input| {
    if input.tail.is_empty() {
        let session = input.store().get_session(input.session_key())?;
        let count = input.store().message_count(input.session_key())?;
        let title = session.as_ref().map_or_else(
            || "(new)".to_owned(),
            |row| {
                if row.title.is_empty() {
                    row.key.clone()
                } else {
                    row.title.clone()
                }
            },
        );
        let workspace = session.map_or_else(|| "default".to_owned(), |row| row.workspace_id);
        return Ok(CommandResult::say(format!(
            "{title}\n`{}` · {count} messages · workspace {workspace}",
            input.chat.session_key
        )));
    }

    // Refused rather than namespaced. The manager would happily turn `web-abc`
    // into `telegram:web-abc` — a real, empty conversation that nothing
    // explains.
    if !owns_session_key(&input.channel_id, &input.tail) {
        return Err(invalid(
            "That session belongs to another channel. Use /sessions to pick one here.",
        ));
    }
    (input.attach)(&input.tail);
    Ok(CommandResult::say(format!("Attached to `{}`.", input.tail)))
});

sync_command!(run_rename, |input| {
    if input.tail.is_empty() {
        return Err(invalid("Usage: /rename <title>"));
    }
    ensure_then(
        input,
        UpdateSession {
            title: Some(input.tail.clone()),
            ..UpdateSession::default()
        },
    )?;
    Ok(CommandResult::say(format!("Renamed to “{}”.", input.tail)))
});

sync_command!(run_delete, |input| {
    let key = if input.tail.is_empty() {
        input.chat.session_key.clone()
    } else {
        input.tail.clone()
    };
    if !owns_session_key(&input.channel_id, &key) {
        return Err(invalid("That session belongs to another channel."));
    }
    let Some(session) = input.store().get_session(&key)? else {
        return Err(missing(format!("No session `{key}`.")));
    };
    let title = if session.title.is_empty() {
        session.key.clone()
    } else {
        session.title.clone()
    };
    // A button rather than a second command, because this is the one thing here
    // that cannot be undone.
    Ok(CommandResult::menu(
        format!("Delete “{title}”? This cannot be undone."),
        confirm_keyboard(
            input.chat_id,
            input.menus,
            CallbackPayload::Delete { session_key: key },
            None,
        ),
    ))
});

sync_command!(run_branch, |input| {
    let seq = resolve_seq(input.store(), input.session_key(), input.arg(0))?;
    let fork = input.store().fork_session(
        input.session_key(),
        seq,
        ForkSession {
            origin: Some(input.channel_id.clone()),
            key: Some(new_session_key(
                &input.channel_id,
                input.chat_id,
                &(input.new_id)(),
            )),
            ..ForkSession::default()
        },
    )?;
    (input.attach)(&fork.session.key);
    Ok(CommandResult::say(format!(
        "Branched at `{}`, carrying {} messages.",
        fork.seq, fork.copied
    )))
});

sync_command!(run_edit, |input| {
    let reference = input.arg(0);
    let content = input
        .args
        .iter()
        .skip(1)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if reference.is_none() || content.is_empty() {
        return Err(invalid("Usage: /edit <ref> <text>"));
    }
    let seq = resolve_seq(input.store(), input.session_key(), reference)?;
    // Through the hub, not through the store: an edit is one frame because
    // truncating and re-running are a single intent, and splitting them leaves
    // a window for another client's queued message.
    (input.control)(ChannelControlFrame::Edit(EditMessage {
        tag: EditTag,
        session_key: input.chat.session_key.clone(),
        seq: u64::try_from(seq).unwrap_or(0),
        content,
        attachments: Vec::new(),
        agent_id: None,
        client_message_id: None,
    }));
    Ok(CommandResult::say(format!("Re-running from `{seq}`.")))
});

sync_command!(run_regenerate, |input| {
    let seq = resolve_seq(input.store(), input.session_key(), input.arg(0))?;
    (input.control)(ChannelControlFrame::Regenerate(RegenerateMessage {
        tag: RegenerateTag,
        session_key: input.chat.session_key.clone(),
        seq: Some(u64::try_from(seq).unwrap_or(0)),
        client_message_id: None,
    }));
    Ok(CommandResult::say(format!("Re-running `{seq}`.")))
});

// No terminal equivalent: Ctrl-C is the terminal's, and a chat has no
// keystrokes. The frame is the same one the browser's Stop button sends.
sync_command!(run_stop, |input| {
    (input.control)(ChannelControlFrame::StopTurn(StopTurnMessage {
        tag: StopTurnTag,
        session_key: input.chat.session_key.clone(),
    }));
    Ok(CommandResult::say("Stopping."))
});

fn run_context<'a>(input: &'a CommandInput<'a>) -> BoxFuture<'a, Result<CommandResult>> {
    Box::pin(async move {
        let Some(report) = input.console.context(input.session_key()).await? else {
            return Ok(CommandResult::say("Nothing to measure yet."));
        };

        // The breakdown, not the transcript. A context report also carries the
        // whole system prompt, every tool definition and every stored message,
        // which in a chat app is thousands of lines nobody asked for.
        let rows: Vec<String> = report
            .breakdown
            .iter()
            .map(|(name, tokens)| format!("  {name}: {tokens}"))
            .collect();
        let percent = if report.context_window_tokens > 0 {
            #[allow(
                clippy::cast_precision_loss,
                reason = "a token count below 2^53, rendered as a whole percent"
            )]
            {
                ((report.estimated_tokens as f64 / report.context_window_tokens as f64) * 100.0)
                    .round()
            }
        } else {
            0.0
        };
        Ok(CommandResult::say(format!(
            "{} of {} tokens ({percent}%), on agent `{}`\n{}",
            report.estimated_tokens,
            report.context_window_tokens,
            report.agent_id.as_deref().unwrap_or("default"),
            rows.join("\n")
        )))
    })
}

fn run_memory<'a>(input: &'a CommandInput<'a>) -> BoxFuture<'a, Result<CommandResult>> {
    Box::pin(async move {
        let state = input.console.memory(input.session_key()).await?;
        if !state.granted {
            return Ok(CommandResult::say(
                "This agent does not have the `memory` tool, so nothing is \
                 remembered and no memory reaches its prompt. Grant it in \
                 Settings → Agents.",
            ));
        }
        Ok(CommandResult::say(if state.count == 0 {
            "Nothing remembered yet. The first one is written when the agent \
             uses its `memory` tool."
                .to_owned()
        } else {
            format!(
                "{} memories in `memory/`, indexed in every prompt for about {} tokens.",
                state.count, state.tokens
            )
        }))
    })
}

// Discovery, not capability. The catalogue is already in the agent's prompt;
// what a person cannot see from a phone is which sheets the workspace holds.
fn run_skills<'a>(input: &'a CommandInput<'a>) -> BoxFuture<'a, Result<CommandResult>> {
    Box::pin(async move {
        let state = input.console.skills(input.session_key()).await?;
        if !state.granted {
            return Ok(CommandResult::say(
                "This agent does not have the `skill` tool, so no catalogue \
                 reaches its prompt and it cannot open a sheet. Grant it in \
                 Settings → Agents.",
            ));
        }
        if state.skills.is_empty() {
            return Ok(CommandResult::say(
                "No skills here yet. A folder with a `SKILL.md` in `skills/` becomes one.",
            ));
        }
        Ok(CommandResult::say(
            state
                .skills
                .iter()
                .map(|skill| {
                    // Marked rather than hidden: this is what the workspace
                    // holds, and a sheet missing from the list would be the
                    // harder thing to explain to whoever just wrote it.
                    let scope = if skill.mine { "" } else { " _(other agents)_" };
                    format!("`{}`: {}{scope}", skill.name, skill.description)
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    })
}

sync_command!(run_stats, |input| {
    let rows = input.store().turn_stats(
        input.session_key(),
        Some(positive(input.arg(0), DEFAULT_LINES)),
    )?;
    if rows.is_empty() {
        return Ok(CommandResult::say("No turns recorded here yet."));
    }
    Ok(CommandResult::say(
        rows.iter()
            .map(|row| {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a millisecond count below 2^53, rendered to one decimal"
                )]
                let seconds = (row.ended_at_ms - row.started_at_ms) as f64 / 1000.0;
                format!(
                    "`{}` · {} steps · {} in / {} out · {seconds:.1}s · {}",
                    row.model,
                    row.iterations,
                    row.usage.prompt_tokens,
                    row.usage.completion_tokens,
                    stop_reason_of(row.stop_reason)
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
    ))
});

// The terminal's two fields are `reasoning` and `stats`, and neither is
// expressible here: the projection never emits a reasoning delta to any
// channel, and nothing projects turn stats. `sendProgress` and `sendToolHints`
// are the manager's, read once from the global config. So the command keeps its
// shape and names the two things a chat owns.
sync_command!(run_output, |input| {
    let prefs = input.chat.prefs;
    let Some(field) = input.arg(0) else {
        return Ok(CommandResult::say(format!(
            "progress: {}. A turn fills in one message\n\
             markdown: {}. Formatted, or plain text",
            on_off(prefs.progress),
            on_off(prefs.markdown)
        )));
    };
    if field != "progress" && field != "markdown" {
        return Err(invalid("Usage: /output [progress|markdown] [on|off]"));
    }
    let current = if field == "progress" {
        prefs.progress
    } else {
        prefs.markdown
    };
    let next = match input.arg(1) {
        None => !current,
        Some(value) => value == "on",
    };
    (input.set_pref)(field, next);
    Ok(CommandResult::say(format!("{field}: {}", on_off(next))))
});

sync_command!(run_agent, |input| {
    let agents = input.console.agents();
    let session = input.store().get_session(input.session_key())?;
    if input.tail.is_empty() {
        let bound = session.and_then(|row| row.agent_id);
        let rows: Vec<PickerRow> = agents
            .iter()
            .map(|agent| PickerRow {
                label: format!("{} · {}", agent.label, agent.model),
                current: Some(&agent.id) == bound.as_ref(),
                payload: CallbackPayload::Agent {
                    agent_id: agent.id.clone(),
                },
            })
            .collect();
        return Ok(CommandResult::menu(
            "Which agent?",
            picker(&rows, MenuKind::Agents, input.chat_id, input.menus),
        ));
    }
    if !agents.iter().any(|agent| agent.id == input.tail) {
        return Err(missing(format!("No agent `{}`.", input.tail)));
    }
    ensure_then(
        input,
        UpdateSession {
            agent_id: Some(Some(input.tail.clone())),
            ..UpdateSession::default()
        },
    )?;
    Ok(CommandResult::say(format!(
        "This session now runs on `{}`.",
        input.tail
    )))
});

// Admin, because it is not scoped to this chat: it moves the process, so the
// browser and every other conversation move with it.
fn run_model<'a>(input: &'a CommandInput<'a>) -> BoxFuture<'a, Result<CommandResult>> {
    Box::pin(async move {
        let catalogue = input.console.models().await?;
        if input.tail.is_empty() {
            let rows: Vec<PickerRow> = catalogue
                .models
                .iter()
                .map(|model| PickerRow {
                    label: model.id.clone(),
                    current: false,
                    payload: CallbackPayload::Model {
                        model_id: model.id.clone(),
                    },
                })
                .collect();
            return Ok(CommandResult::menu(
                "Which model?",
                picker(&rows, MenuKind::Models, input.chat_id, input.menus),
            ));
        }
        input.console.set_model(&input.tail);
        Ok(CommandResult::say(format!("Now running `{}`.", input.tail)))
    })
}

sync_command!(run_workspaces, |input| {
    let current = workspace_of(input)?;
    Ok(CommandResult::say(
        input
            .console
            .workspaces()
            .list()?
            .iter()
            .map(|workspace| {
                let marker = if workspace.id == current {
                    "• "
                } else {
                    "  "
                };
                format!("{marker}`{}`: {}", workspace.id, workspace.name)
            })
            .collect::<Vec<_>>()
            .join("\n"),
    ))
});

sync_command!(run_workspace, run_workspace_verbs);

/// Everything dispatchable, in the order `/help` lists it.
static COMMANDS: &[TelegramCommand] = &[
    TelegramCommand {
        name: "help",
        usage: "",
        description: "Everything this bot understands",
        admin: false,
        aliases: &[],
        run: run_help,
    },
    TelegramCommand {
        name: "start",
        usage: "",
        description: "What this bot is, and what it understands",
        admin: false,
        aliases: &[],
        run: run_start,
    },
    TelegramCommand {
        name: "messages",
        usage: "[n]",
        description: "The last few messages, with the seq numbers /edit takes",
        admin: false,
        aliases: &[],
        run: run_messages,
    },
    TelegramCommand {
        name: "clear",
        usage: "",
        description: "Forget this session’s history, keeping the session",
        admin: false,
        aliases: &[],
        run: run_clear,
    },
    TelegramCommand {
        name: "exit",
        usage: "",
        description: "Detach: the next message starts a fresh session",
        admin: false,
        aliases: &["quit"],
        run: run_exit,
    },
    TelegramCommand {
        name: "sessions",
        usage: "[n]",
        description: "Pick a session",
        admin: false,
        aliases: &[],
        run: run_sessions,
    },
    TelegramCommand {
        name: "new",
        usage: "[title]",
        description: "Start a fresh session",
        admin: false,
        aliases: &[],
        run: run_new,
    },
    TelegramCommand {
        name: "session",
        usage: "[key]",
        description: "Show this session, or attach to another by key",
        admin: false,
        aliases: &[],
        run: run_session,
    },
    TelegramCommand {
        name: "rename",
        usage: "<title>",
        description: "Retitle this session",
        admin: false,
        aliases: &[],
        run: run_rename,
    },
    TelegramCommand {
        name: "delete",
        usage: "[key]",
        description: "Delete a session, after confirming",
        admin: false,
        aliases: &[],
        run: run_delete,
    },
    TelegramCommand {
        name: "branch",
        usage: "[ref]",
        description: "Fork this session at a message and continue there",
        admin: false,
        aliases: &[],
        run: run_branch,
    },
    TelegramCommand {
        name: "edit",
        usage: "<ref> <text>",
        description: "Replace one of your messages and re-run from it",
        admin: false,
        aliases: &[],
        run: run_edit,
    },
    TelegramCommand {
        name: "regenerate",
        usage: "[ref]",
        description: "Run a turn again, discarding the answer it gave",
        admin: false,
        aliases: &[],
        run: run_regenerate,
    },
    TelegramCommand {
        name: "stop",
        usage: "",
        description: "Abort the turn that is running",
        admin: false,
        aliases: &[],
        run: run_stop,
    },
    TelegramCommand {
        name: "context",
        usage: "",
        description: "How much of the model’s window this session fills",
        admin: false,
        aliases: &[],
        run: run_context,
    },
    TelegramCommand {
        name: "memory",
        usage: "",
        description: "What this agent remembers about this workspace",
        admin: false,
        aliases: &[],
        run: run_memory,
    },
    TelegramCommand {
        name: "skills",
        usage: "",
        description: "The sheets this workspace holds",
        admin: false,
        aliases: &[],
        run: run_skills,
    },
    TelegramCommand {
        name: "stats",
        usage: "[n]",
        description: "What the last few turns cost",
        admin: false,
        aliases: &[],
        run: run_stats,
    },
    TelegramCommand {
        name: "output",
        usage: "[progress|markdown] [on|off]",
        description: "How answers are rendered in this chat",
        admin: false,
        aliases: &[],
        run: run_output,
    },
    TelegramCommand {
        name: "agent",
        usage: "[id]",
        description: "Which agent this session runs on",
        admin: false,
        aliases: &[],
        run: run_agent,
    },
    TelegramCommand {
        name: "model",
        usage: "[id]",
        description: "Which model this install runs on (admin)",
        admin: true,
        aliases: &[],
        run: run_model,
    },
    TelegramCommand {
        name: "workspaces",
        usage: "",
        description: "The workspaces on this install",
        admin: false,
        aliases: &[],
        run: run_workspaces,
    },
    TelegramCommand {
        name: "workspace",
        usage: "[id] | new <name> | rename <id> <name> | rm <id> | move <from> <to>",
        description: "Move this session, or manage workspaces (verbs: admin)",
        admin: false,
        aliases: &[],
        run: run_workspace,
    },
];

fn find(name: &str) -> Option<&'static TelegramCommand> {
    COMMANDS
        .iter()
        .find(|command| command.name == name || command.aliases.contains(&name))
}

// /workspace, which is four commands wearing one name

fn require_admin(input: &CommandInput<'_>, verb: &str) -> Result<()> {
    if input.is_admin {
        return Ok(());
    }
    Err(WireError::new(
        ErrorKind::PermissionDenied,
        format!("`/workspace {verb}` is for an administrator of this install."),
    ))
}

fn run_workspace_verbs(input: &CommandInput<'_>) -> Result<CommandResult> {
    let workspaces = input.console.workspaces();
    let Some(verb) = input.arg(0) else {
        let current = workspace_of(input)?;
        let rows: Vec<PickerRow> = workspaces
            .list()?
            .iter()
            .map(|workspace| PickerRow {
                label: workspace.name.clone(),
                current: workspace.id == current,
                payload: CallbackPayload::Workspace {
                    workspace_id: workspace.id.clone(),
                },
            })
            .collect();
        return Ok(CommandResult::menu(
            "Which workspace should this session live in?",
            picker(&rows, MenuKind::Workspaces, input.chat_id, input.menus),
        ));
    };
    let rest: Vec<&str> = input.args.iter().skip(1).map(String::as_str).collect();

    match verb {
        "new" => {
            require_admin(input, "new")?;
            let name = rest.join(" ");
            if name.is_empty() {
                return Err(invalid("Usage: /workspace new <name>"));
            }
            let created = workspaces.create(CreateWorkspace {
                name,
                ..CreateWorkspace::default()
            })?;
            Ok(CommandResult::say(format!("Created `{}`.", created.id)))
        }
        "rename" => {
            require_admin(input, "rename")?;
            let (Some(id), true) = (rest.first(), rest.len() > 1) else {
                return Err(invalid("Usage: /workspace rename <id> <name>"));
            };
            workspaces.rename(id, &rest[1..].join(" "))?;
            Ok(CommandResult::say(format!("Renamed `{id}`.")))
        }
        "rm" => {
            require_admin(input, "rm")?;
            let Some(id) = rest.first() else {
                return Err(invalid("Usage: /workspace rm <id>"));
            };
            let held = input.store().count_by_workspace(id)?;
            if held > 0 {
                return Err(invalid(format!(
                    "`{id}` still holds {held} sessions. Move them first with /workspace move."
                )));
            }
            workspaces.delete(id)?;
            Ok(CommandResult::say(format!("Removed `{id}`.")))
        }
        "move" => {
            require_admin(input, "move")?;
            let (Some(from), Some(to)) = (rest.first(), rest.get(1)) else {
                return Err(invalid("Usage: /workspace move <from> <to>"));
            };
            let moved = input.store().reassign_workspace(from, to)?;
            Ok(CommandResult::say(format!(
                "Moved {moved} sessions to `{to}`."
            )))
        }
        // Not a verb, so it is an id — the same reading the terminal gives it.
        id => switch_workspace(input, id),
    }
}

fn switch_workspace(input: &CommandInput<'_>, id: &str) -> Result<CommandResult> {
    if input.console.workspaces().get(id)?.is_none() {
        return Err(missing(format!("No workspace `{id}`.")));
    }
    ensure_then(
        input,
        UpdateSession {
            workspace_id: Some(id.to_owned()),
            ..UpdateSession::default()
        },
    )?;
    Ok(CommandResult::say(format!(
        "This session now lives in `{id}`."
    )))
}

// Parsing, dispatch and the two listings

/// A command read out of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// Lowercase, without the slash or the `@bot` suffix.
    pub name: String,
    /// Whitespace-split arguments.
    pub args: Vec<String>,
    /// Everything after the command word, untouched.
    pub tail: String,
}

/// Reads a command out of a message, or decides it is not one.
///
/// Telegram's own entities rather than a leading slash, because Telegram
/// already did the parse: a message that merely *mentions* `/clear` in prose
/// carries no `bot_command` entity at offset 0, and matching on the character
/// would run it.
///
/// In a group Telegram delivers `/sessions@ghost_bot`, so the bot's own
/// username is stripped — and a command addressed to a *different* bot in the
/// same group is not ours to answer.
pub fn parse_command(
    text: &str,
    entities: &[crate::telegram::api::TelegramMessageEntity],
    bot_username: Option<&str>,
) -> Option<ParsedCommand> {
    let is_command = entities
        .iter()
        .any(|entity| entity.kind == "bot_command" && entity.offset == 0);
    if !is_command || !text.starts_with('/') {
        return None;
    }

    let trimmed = text.trim();
    let word = trimmed.split_whitespace().next().unwrap_or("");
    let args: Vec<String> = trimmed
        .split_whitespace()
        .skip(1)
        .map(str::to_owned)
        .collect();

    let bare_and_addressed = word.get(1..).unwrap_or("");
    let (bare, addressed) = match bare_and_addressed.split_once('@') {
        Some((bare, addressed)) => (bare, Some(addressed)),
        None => (bare_and_addressed, None),
    };
    if let (Some(addressed), Some(username)) = (addressed, bot_username)
        && !addressed.eq_ignore_ascii_case(username)
    {
        return None;
    }

    Some(ParsedCommand {
        name: bare.to_lowercase(),
        args,
        tail: trimmed.get(word.len()..).unwrap_or("").trim().to_owned(),
    })
}

/// Runs one command.
///
/// Never fails for anything a person typed: an error becomes the reply, the way
/// the terminal's dispatcher renders one as a warning and brings the prompt
/// back. An unknown command says so rather than being ignored, because a bot
/// that silently drops a typo looks broken.
pub async fn run_command(name: &str, input: &CommandInput<'_>) -> CommandResult {
    let Some(command) = find(name) else {
        return CommandResult::say(format!("No command `/{name}`. Try /help."));
    };
    if command.admin && !input.is_admin {
        return CommandResult::say(format!(
            "`/{}` is for an administrator of this install.",
            command.name
        ));
    }
    match (command.run)(input).await {
        Ok(result) => result,
        Err(error) => {
            // A failure a person caused reads as an answer; anything else is
            // also worth a line in the log, because the reply is the only other
            // trace it leaves.
            if !matches!(
                error.kind,
                ErrorKind::InvalidInput | ErrorKind::NotFound | ErrorKind::PermissionDenied
            ) {
                tracing::warn!(
                    command = command.name,
                    kind = error.kind.as_str(),
                    error = %error.message,
                    "telegram command failed"
                );
            }
            CommandResult::say(error.message)
        }
    }
}

/// The list `setMyCommands` registers, so Telegram's own `/` menu has it.
pub fn bot_commands() -> Vec<BotCommand> {
    COMMANDS
        .iter()
        .map(|command| BotCommand {
            command: command.name.to_owned(),
            description: command.description.to_owned(),
        })
        .collect()
}

/// `/help`, measured rather than typed out.
pub fn help_text(is_admin: bool) -> String {
    helped(is_admin)
}

fn helped(is_admin: bool) -> String {
    let mut lines = vec![
        "Send a message to talk to the agent. These are the commands:".to_owned(),
        String::new(),
    ];
    for command in COMMANDS.iter().filter(|c| !c.admin || is_admin) {
        let syntax = if command.usage.is_empty() {
            format!("/{}", command.name)
        } else {
            format!("/{} {}", command.name, command.usage)
        };
        lines.push(format!("`{syntax}`\n   {}", command.description));
    }
    lines.join("\n")
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn role_of(message: &darkwire_protocol::ChatMessage) -> &'static str {
    match message {
        darkwire_protocol::ChatMessage::System(_) => "system",
        darkwire_protocol::ChatMessage::User(_) => "user",
        darkwire_protocol::ChatMessage::Assistant(_) => "assistant",
        darkwire_protocol::ChatMessage::Tool(_) => "tool",
    }
}

fn stop_reason_of(reason: darkwire_protocol::StopReason) -> &'static str {
    match reason {
        darkwire_protocol::StopReason::Complete => "complete",
        darkwire_protocol::StopReason::Aborted => "aborted",
        darkwire_protocol::StopReason::MaxIterations => "max_iterations",
        darkwire_protocol::StopReason::WallTimeout => "wall_timeout",
        darkwire_protocol::StopReason::Error => "error",
    }
}
