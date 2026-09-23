//! `darkwire chat` — a turn, or a conversation of them.
//!
//! Three shapes over one implementation: a message on the command line runs a
//! single turn and exits, a piped stdin is read as that message, and an
//! interactive terminal opens a REPL. All three drive the same [`run_turn`], so
//! an interruption, an error and a stop reason behave identically whether they
//! happen in a pipeline or at a prompt.
//!
//! Cancellation is the part worth reading. There is exactly one cancellation
//! token per turn, and it runs from the interrupt handler here through the
//! loop, the provider request, the tool registry and into the child process —
//! so interrupting a build under `exec` stops the build rather than orphaning
//! it and returning to a prompt that lies about being idle. What differs
//! between the modes is only what happens *after* the abort:
//!
//!  - **One-shot:** the turn stops and the process exits [`SIGINT_EXIT_CODE`],
//!    the conventional "terminated by SIGINT" code, so a script can tell an
//!    interruption from a failure.
//!  - **REPL:** the turn stops and the prompt comes back. A second interrupt,
//!    with no turn running, leaves. Making the first one leave would throw away
//!    the session for a mistyped question.
//!
//! The turn's final text is not re-printed at the end: the assistant deltas
//! already streamed it, and a driver that also printed the result would show
//! every answer twice.
//!
//! **The frame owns the screen.** On a terminal the prompt takes the alternate
//! screen and Ratatui draws all of it — the conversation, the composer, the
//! status bar and any open menu — from the frame's state, at whatever size the
//! window is that frame. Nothing is patched and no coordinate outlives a draw,
//! which is what makes a resize and a closing overlay uneventful. On a pipe
//! there is no frame at all: a prompt and a newline, since escape sequences
//! written into a file are not a status bar, they are noise in somebody's log.

use std::future::Future;
use std::io::{IsTerminal, Write};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use darkwire_agent::{AgentLoop, PromptPreviewInput, describe_context};
use darkwire_core::messages::Content;
use darkwire_core::{Result, WireError};
use darkwire_i18n::{args, keys};
use std::collections::HashMap;

use darkwire_protocol::config::ReasoningDisplay;
use darkwire_protocol::{DEFAULT_AGENT_ID, DEFAULT_WORKSPACE_ID, StopReason, ToolRisk};
use darkwire_runtime::{RuntimeOptions, WireRuntime};
use darkwire_server::agent_for_turn;
use darkwire_tui::{Theme, theme_for};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::Streams;
use crate::approval::{TerminalGate, rule_saver};
use crate::commands::{SlashContext, SlashOutcome, run_slash_command};
use crate::header::{ContextUsage, HeaderView, startup_header};
use crate::i18n::{Env, Translations, describe_error};
use crate::menu::{MenuAvailable, menu_available};
use crate::models::{ModelCatalogue, ModelCatalogueOptions, create_model_catalogue};
use crate::pickers::{NoMenu, PickerMenu};
use crate::program::{ChatArgs, Globals};
use crate::render::{
    PlainPrinter, TranscriptEvent, TranscriptSink, TurnRenderer, TurnRendererOptions,
};
use crate::runtime::{env_map, install_logger, settings_of};

/// Conventional exit code for "terminated by SIGINT".
pub const SIGINT_EXIT_CODE: u8 = 130;

/// How a turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutcome {
    /// The loop's own reason, not one re-derived from the token.
    pub stop_reason: StopReason,
    /// Whether the turn was interrupted.
    pub aborted: bool,
    /// Whether anything in it failed.
    pub failed: bool,
}

/// Where one turn's events go.
///
/// `--json` writes the event stream verbatim for a script to read; everything
/// else renders prose. One enum rather than an optional writer, so the two
/// cases cannot both be half-applied.
pub enum TurnSink<'a> {
    /// Prose, through the renderer.
    Rendered(&'a mut TurnRenderer),
    /// One JSON object per line.
    Json(&'a mut dyn Write),
}

/// Everything [`run_turn`] needs, stated so a test can supply all of it.
pub struct RunTurnDeps<'a> {
    /// The loop this turn runs on.
    pub agent_loop: &'a AgentLoop,
    /// Where the events go.
    pub sink: TurnSink<'a>,
    /// The conversation.
    ///
    /// A plain string, read once per call: the REPL's attachment moves — `/new`,
    /// `/session` and `/branch` all change it — but that is the caller's
    /// problem, which is what keeps this the tested unit it is.
    pub session_key: String,
    /// Where a session *created* by this turn lands. Never moves an existing
    /// one.
    pub workspace_id: Option<String>,
    /// Which agent a session *created* by this turn is bound to.
    ///
    /// The loop applies the same stored-wins rule one layer down, so passing it
    /// here cannot move an existing conversation — but the caller has to pick
    /// the matching *loop*, or the turn would run on one agent's settings and
    /// be prompted with another's.
    pub agent_id: Option<String>,
    /// Cancels the turn, and everything under it.
    pub token: CancellationToken,
}

/// One turn, rendered.
///
/// Separate from all three drivers because it is the whole of what the terminal
/// does with the event stream, and a test can hand it a loop and a buffer
/// without a terminal, a database or a provider.
pub async fn run_turn(deps: RunTurnDeps<'_>, content: Content) -> Result<TurnOutcome> {
    let RunTurnDeps {
        agent_loop,
        mut sink,
        session_key,
        workspace_id,
        agent_id,
        token,
    } = deps;

    let mut failed = false;
    // Read off `turn.end` rather than off the completion, which is deliberately
    // the same information every other transport has rather than something only
    // the terminal can see.
    let mut stop_reason: Option<StopReason> = None;

    let mut turn = agent_loop.run(
        darkwire_agent::TurnInput {
            session_key,
            content,
            channel: Some("cli".to_owned()),
            agent_id,
            workspace_id,
            turn_id: None,
            chain: Vec::new(),
            root_session_key: None,
            inherited_environment: None,
        },
        &token,
    );

    while let Some(event) = turn.next_event().await {
        if let darkwire_agent::AgentEvent::Nested(nested) = &event {
            match nested {
                darkwire_protocol::NestedAgentEvent::Error(_) => failed = true,
                darkwire_protocol::NestedAgentEvent::TurnEnd(end) => {
                    stop_reason = Some(end.stop_reason);
                }
                _ => {}
            }
        }
        match &mut sink {
            TurnSink::Rendered(renderer) => renderer.handle(&event),
            TurnSink::Json(out) => {
                if let Ok(line) = serde_json::to_string(&event) {
                    let _ = writeln!(out, "{line}");
                }
            }
        }
    }

    if let TurnSink::Rendered(renderer) = &mut sink {
        renderer.finish();
    }

    match turn.finish().await {
        Ok(_) => {}
        Err(error) if error.is_aborted() => {
            return Ok(TurnOutcome {
                stop_reason: StopReason::Aborted,
                aborted: true,
                failed: false,
            });
        }
        Err(error) => return Err(error),
    }

    // A stream that ended without `turn.end` is a broken loop, not a clean turn.
    let reason = stop_reason.unwrap_or(StopReason::Error);
    Ok(TurnOutcome {
        stop_reason: reason,
        aborted: reason == StopReason::Aborted,
        failed: failed || reason == StopReason::Error,
    })
}

/// The prompt's mutable attachment: which conversation, workspace and agent.
///
/// Holders rather than constants, because `/new`, `/session` and `/branch` all
/// move the prompt to another conversation and `/workspace` moves where the
/// next new one lands. Everything that draws reads them at the moment it draws.
#[derive(Debug, Default)]
pub struct Attachment {
    /// The conversation the prompt is on.
    pub session_key: String,
    /// Where a session created next lands.
    pub workspace_id: Option<String>,
    /// Which agent a session created next is bound to.
    pub agent_id: Option<String>,
    /// A name waiting for the conversation it belongs to.
    ///
    /// `/new <title>` names a session that does not exist yet, and the row is
    /// written by the first turn. The name is applied once it does.
    pub pending_title: Option<String>,
}

/// Everything a chat run was asked for, after the flags were read.
pub struct ChatSession {
    pub(crate) runtime: Arc<WireRuntime>,
    pub(crate) t: Translations,
    pub(crate) theme: Theme,
    colors: Option<bool>,
    pub(crate) attachment: Attachment,
    /// Set while `--model` pinned the model for this process.
    model_pinned: bool,
    /// What the last turn left in the window.
    context: Option<ContextUsage>,
    /// What `/model` lists, cached for the life of the prompt.
    models: ModelCatalogue,
    /// The process environment, for the one question the prompt asks of it:
    /// whether this terminal can draw a menu at all.
    env: Env,
    /// Who is asked before a tool set to `ask` runs. `None` under `--yes`.
    pub(crate) approvals: Option<Arc<TerminalGate>>,
}

impl ChatSession {
    /// Whether reasoning is worth sending to a surface at all.
    ///
    /// `hidden` is the strong reading of the setting, and it is enforced at the
    /// source rather than at the fold: text nobody can ever unfold is text
    /// nobody should have paid to format, carry over a channel and hold in a
    /// transcript.
    fn reasoning_reaches_a_reader(&self) -> bool {
        self.runtime.config().ui.reasoning != ReasoningDisplay::Hidden
    }
}

impl std::fmt::Debug for ChatSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatSession")
            .field("attachment", &self.attachment)
            .finish_non_exhaustive()
    }
}

impl ChatSession {
    /// The runtime this prompt is talking to.
    pub fn runtime(&self) -> &Arc<WireRuntime> {
        &self.runtime
    }

    /// The conversation, the workspace and the agent the prompt is on.
    pub fn attachment(&self) -> &Attachment {
        &self.attachment
    }

    /// Whether `--model` pinned the model for this process.
    pub fn model_pinned(&self) -> bool {
        self.model_pinned
    }

    /// Whether an agent id names something runnable right now.
    fn resolves(&self, id: &str) -> bool {
        self.runtime.agents().iter().any(|agent| agent.id == id)
    }

    /// The same precedence a turn applies, without the warning.
    ///
    /// The warning is right once per turn and wrong every time a prompt is
    /// redrawn — and the prompt is redrawn on every keystroke.
    fn agent_quietly(&self) -> String {
        let stored = self
            .runtime
            .store()
            .get_session(&self.attachment.session_key)
            .ok()
            .flatten()
            .and_then(|session| session.agent_id);
        agent_for_turn(
            stored.as_deref(),
            self.attachment.agent_id.as_deref(),
            &|id| self.resolves(id),
        )
        .unwrap_or_else(|| DEFAULT_AGENT_ID.to_owned())
    }

    /// Which agent this turn runs on.
    ///
    /// The stored session wins over the flag, because a history built under one
    /// agent's prompt, tools and permissions must not silently continue under
    /// another's.
    ///
    /// The fallback below is not optional. The rule answers the *stored* id
    /// when neither it nor the request resolves, and asking for a loop by an id
    /// that names nothing runnable is a refusal — so without this, deleting an
    /// agent from `config.yaml` would turn a working conversation into a hard
    /// failure on its next turn. The default runs it instead, and says so every
    /// time, because nothing is written down to make the substitution stick.
    fn agent_for_this_turn(&self, renderer: &mut TurnRenderer) -> Option<String> {
        let stored = self
            .runtime
            .store()
            .get_session(&self.attachment.session_key)
            .ok()
            .flatten()
            .and_then(|session| session.agent_id);
        let chosen = agent_for_turn(
            stored.as_deref(),
            self.attachment.agent_id.as_deref(),
            &|id| self.resolves(id),
        )?;
        if self.resolves(&chosen) {
            return Some(chosen);
        }
        renderer.warn(
            &self
                .t
                .tr(keys::chat::AGENT_GONE, args!["agent" => chosen.as_str()]),
        );
        None
    }

    /// Names the conversation, if a name has been waiting for it.
    ///
    /// `/new <title>` names a session before there is a row to put the name
    /// on. This is the other half: once the first turn has written the row,
    /// the name lands on it and the wait is over.
    ///
    /// A failure is dropped. The conversation is running and the name is a
    /// label on it; interrupting an answer to report that a rename did not
    /// take is worse than the prompt saying the key for one more turn.
    fn apply_pending_title(&mut self) {
        let Some(title) = self.attachment.pending_title.take() else {
            return;
        };
        let _ = self.runtime.store().update_session(
            &self.attachment.session_key,
            darkwire_core::session_store::UpdateSession {
                title: Some(title),
                ..darkwire_core::session_store::UpdateSession::default()
            },
        );
    }

    /// Re-measures what the conversation would cost the next request.
    ///
    /// Measured after a turn rather than on every repaint, and that is exact
    /// rather than a saving: the context only changes when the history does,
    /// and the history only changes when a turn runs. Tokenising the whole
    /// conversation on every keystroke would buy the same number at a cost
    /// nobody would forgive.
    async fn measure(&mut self) {
        let id = self.agent_quietly();
        let Ok(Some(agent_loop)) = self.runtime.loop_for(Some(&id)) else {
            return;
        };
        let tools = self.runtime.tools().definitions().to_vec();
        let window = settings_of(&self.runtime.config(), Some(&id)).context_window_tokens;
        let report = describe_context(
            self.runtime.store(),
            &agent_loop,
            &tools,
            &PromptPreviewInput {
                session_key: self.attachment.session_key.clone(),
                channel: Some("cli".to_owned()),
                agent_id: Some(id),
            },
            window,
        )
        .await;
        // A measurement is a nicety. An install whose history upsets it should
        // still get a prompt, and the bar simply says nothing about the context
        // until the next turn.
        self.context = match report {
            Ok(Some(report)) => Some(ContextUsage {
                used_tokens: report.estimated_tokens as u64,
                window_tokens: report.context_window_tokens,
            }),
            Ok(None) | Err(_) => None,
        };
    }

    /// What the header and the status bar say, read fresh every time.
    ///
    /// Deliberately not the warning-raising agent resolution: this is called to
    /// redraw a prompt, so borrowing that one would print the same notice every
    /// time the operator pressed Return on an empty line.
    pub(crate) fn view(&self) -> HeaderView {
        let opened = self
            .runtime
            .store()
            .get_session(&self.attachment.session_key)
            .ok()
            .flatten();
        let id = self.agent_quietly();
        // Falls through to `default`, and the last step is the point. Nothing is
        // stored until the first message, so there is no session on a prompt
        // nobody has typed into yet — and without a third fallback the bar
        // reported a state the store cannot hold. The row is `NOT NULL DEFAULT
        // 'default'` and the registry seeds it, so the session is going to land
        // in the default workspace the moment it exists.
        let where_id = opened
            .as_ref()
            .map(|session| session.workspace_id.clone())
            .or_else(|| self.attachment.workspace_id.clone())
            .unwrap_or_else(|| DEFAULT_WORKSPACE_ID.to_owned());
        let agents = self.runtime.agents();
        let agent = agents.iter().find(|one| one.id == id);

        // This conversation's agent, not the install's: the runtime's own model
        // and provider describe the *default* agent, so on a conversation moved
        // onto another they would disagree and the bar would report one agent's
        // label over another's model.
        //
        // Off the loop rather than through a fresh resolve, because resolving a
        // provider opens the credential vault — and opening the vault can mint
        // a keychain entry, which is far too much to do to redraw a status bar
        // on every keystroke.
        let agent_loop = self.runtime.loop_for(Some(&id)).ok().flatten();
        let spec = agent_loop.as_ref().and_then(|one| {
            darkwire_providers::find_provider(one.provider(), &darkwire_providers::PROVIDERS)
        });

        HeaderView {
            agent: agent.map_or_else(|| id.clone(), |one| one.label.clone()),
            model: agent_loop.as_ref().map_or_else(
                || agent.map_or_else(String::new, |one| one.settings.model.clone()),
                |one| one.model().to_owned(),
            ),
            provider: spec.map_or_else(
                || {
                    agent_loop.as_ref().map_or_else(
                        || self.t.t(keys::chat::NO_PROVIDER),
                        |one| one.provider().to_owned(),
                    )
                },
                |spec| spec.display_name.clone(),
            ),
            workspaces: self.runtime.paths().workspaces_dir.display().to_string(),
            workspace_name: self
                .runtime
                .workspaces()
                .get(&where_id)
                .ok()
                .flatten()
                .map_or(where_id, |row| row.name),
            // Never the raw key. A title is derived from the first message, so
            // the only conversations without one are the ones nobody has
            // spoken in — and a uuid is not a name for those, it is an
            // admission that nothing named them.
            session: match opened {
                Some(session) if !session.title.is_empty() => session.title,
                _ => self.t.t(keys::chat::NEW_SESSION),
            },
            session_key: self.attachment.session_key.clone(),
            context: self.context,
        }
    }
}

/// Builds the runtime and the session state one chat run works over.
///
/// Separate from [`run`] so a test can drive a whole prompt without a terminal
/// or a signal handler.
pub fn open(globals: &Globals, args: &ChatArgs, env: &Env) -> Result<ChatSession> {
    // Built before the runtime, which carries it into every agent's loop, and
    // told where the runtime is once there is one. `--yes` installs none, and
    // then every tool set to `ask` runs unasked.
    let built = Arc::new(OnceLock::new());
    let approvals = (!args.yes).then(|| {
        Arc::new(TerminalGate::new(
            rule_saver(Arc::clone(&built)),
            Translations::for_env(env, None).locale(),
        ))
    });
    let runtime = darkwire_runtime::create_runtime(RuntimeOptions {
        home: globals.home.clone(),
        workspaces: args.workspaces.clone(),
        model: args.model.clone(),
        provider: args.provider.clone(),
        tools: args.tools,
        env: Some(env_map(env)),
        approvals: approvals
            .clone()
            .map(|gate| gate as Arc<dyn darkwire_agent::ApprovalGate>),
        ..RuntimeOptions::default()
    })?;
    let _ = built.set(Arc::downgrade(&runtime));

    // A prompt with no `-s` starts a conversation of its own rather than
    // continuing whichever one ran last. Opening the prompt and being handed
    // somebody's previous questions is the wrong default: a session is worth
    // resuming on purpose, by name, and `/sessions` is how you pick one.
    let session_key = match args.session_key.clone() {
        Some(key) => key,
        None => runtime.new_session_key(crate::program::CLI_SESSION_PREFIX),
    };

    if args.fresh {
        runtime.store().clear_messages(&session_key)?;
    }

    // After the runtime, because this is the first point the install's own
    // answer exists — `config.ui.locale` sits under `DARKWIRE_LANG` and above the
    // shell's `LANG` in the order the resolution applies.
    let t = Translations::for_env(env, Some(&runtime.config().ui.locale));
    if let Some(gate) = &approvals {
        gate.set_locale(t.locale());
    }

    let models = create_model_catalogue(
        Arc::clone(&runtime),
        ModelCatalogueOptions {
            // `VaultChoice::Default` opens `vault.json` only when it already
            // exists, so listing models on an install that has never stored a
            // key does not mint a keychain entry to find out.
            credential_for: {
                let runtime = Arc::clone(&runtime);
                let env = env_map(env);
                Arc::new(move |instance| {
                    darkwire_runtime::find_credential(
                        instance,
                        &runtime.paths(),
                        &env,
                        &darkwire_runtime::VaultChoice::Default,
                    )
                    .ok()
                    .flatten()
                })
            },
            timeout_ms: None,
            clock: None,
        },
    );

    Ok(ChatSession {
        theme: theme_for(globals.color),
        models,
        env: env.clone(),
        colors: globals.color,
        attachment: Attachment {
            session_key,
            workspace_id: args.workspace_id.clone(),
            agent_id: args.agent_id.clone(),
            pending_title: None,
        },
        model_pinned: args.model.is_some(),
        context: None,
        runtime,
        t,
        approvals,
    })
}

/// Runs one `darkwire chat` invocation and answers with its exit code.
pub async fn run(
    globals: &Globals,
    args: ChatArgs,
    env: &Env,
    streams: &mut Streams,
) -> Result<u8> {
    // `error`, where the server uses `info`. A warning here is worth reading and
    // worth acting on, but this is the one surface where it interrupts a
    // conversation to say something about the install rather than about the
    // answer — and the ones that recur do so on *every turn*. `--verbose` brings
    // them back.
    install_logger(
        globals
            .chat_log_level()
            .unwrap_or(darkwire_core::LogLevel::Error),
        env,
    );

    let mut session = open(globals, &args, env)?;
    let code = drive(&mut session, &args, streams).await;
    if let Some(hint) = session
        .approvals
        .as_ref()
        .and_then(|gate| gate.refused_hint())
    {
        let _ = writeln!(streams.err, "{hint}");
    }
    session.runtime.close().await;
    code
}

/// Whichever of the three shapes this invocation is.
async fn drive(session: &mut ChatSession, args: &ChatArgs, streams: &mut Streams) -> Result<u8> {
    // A message argument, then anything piped in. A prompt on a stdin that is
    // not a terminal would read its first line as a question and then see EOF.
    let one_shot = match args.message.clone() {
        Some(message) => Some(message),
        None if std::io::stdin().is_terminal() => None,
        None => Some(read_all_stdin()?),
    };

    if let Some(message) = one_shot {
        return one_shot_turn(session, args, &message, streams).await;
    }
    repl(session, args, streams).await
}

/// Whichever prompt this terminal can carry.
///
/// [`menu_available`] is the single predicate, so the answer cannot differ
/// between the code that decides to offer a frame and the picker that asks
/// whether it may draw one.
async fn repl(session: &mut ChatSession, args: &ChatArgs, streams: &mut Streams) -> Result<u8> {
    let columns = crate::menu::terminal_columns();
    let framed = menu_available(&MenuAvailable {
        stdin_tty: std::io::stdin().is_terminal(),
        stdout_tty: std::io::stdout().is_terminal(),
        columns,
        json: args.json,
        env: &session.env,
    });
    // A terminal that passes the predicate and still will not give up raw mode
    // or the alternate screen gets the plain prompt. A prompt with no frame is
    // still a prompt, and refusing outright would be a session lost to a
    // capability nobody asked for.
    if let Some(mut surface) = framed
        .then(|| crate::app::TuiSurface::open(session).ok())
        .flatten()
    {
        return drive_prompt(session, args, &mut surface).await;
    }
    let width = crate::menu::columns_or_default(columns);
    let header = startup_header(&session.view(), width, &session.theme, &session.t, false);
    let mut surface = PlainSurface::open(&mut streams.out, &header)?;
    drive_prompt(session, args, &mut surface).await
}

/// Everything piped in, for `darkwire chat < prompt.txt`.
fn read_all_stdin() -> Result<String> {
    use std::io::Read as _;
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(WireError::from)?;
    Ok(text)
}

/// One turn, then out.
async fn one_shot_turn(
    session: &mut ChatSession,
    args: &ChatArgs,
    message: &str,
    streams: &mut Streams,
) -> Result<u8> {
    let content = message.trim();
    if content.is_empty() {
        return Ok(0);
    }

    let token = CancellationToken::new();
    let interrupted = Arc::new(AtomicBool::new(false));
    let watcher = {
        let token = token.clone();
        let interrupted = Arc::clone(&interrupted);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                interrupted.store(true, Ordering::SeqCst);
                token.cancel();
            }
        })
    };

    let outcome = turn_once(
        session,
        args,
        Content::Text(content.to_owned()),
        streams,
        &token,
    )
    .await;
    watcher.abort();

    match outcome {
        Ok(outcome) if outcome.aborted => Ok(SIGINT_EXIT_CODE),
        Ok(outcome) => Ok(u8::from(outcome.failed)),
        Err(error) if error.is_aborted() => Ok(SIGINT_EXIT_CODE),
        Err(error) => Err(error),
    }
}

/// One turn on whichever loop the session's agent resolves to.
async fn turn_once(
    session: &mut ChatSession,
    args: &ChatArgs,
    content: Content,
    streams: &mut Streams,
    token: &CancellationToken,
) -> Result<TurnOutcome> {
    // `--json` writes machine-readable output to the same stream; colouring it
    // would corrupt the JSON for the script reading it.
    let (sink, mut pending) = chunks();
    let mut renderer = TurnRenderer::new(TurnRendererOptions {
        out: Box::new(sink),
        colors: if args.json {
            Some(false)
        } else {
            session.colors
        },
        show_reasoning: args.show_reasoning && session.reasoning_reaches_a_reader(),
        show_stats: session.runtime.config().ui.expand_turn_stats,
        t: Translations::new(session.t.locale()),
        ..TurnRendererOptions::new(Box::new(crate::render::NullSink))
    });

    let chosen = session.agent_for_this_turn(&mut renderer);
    // Requiring a loop rather than taking whatever there is: an unconfigured
    // install builds a runtime with no loop so that `darkwire serve` can come
    // up, and this is the one caller that genuinely cannot proceed without one.
    // The refusal names what to set.
    let agent_loop = session.runtime.require_loop_for(chosen.as_deref())?;

    if args.json {
        // Nothing reaches the renderer on this path: the events are the output.
        return run_turn(
            RunTurnDeps {
                agent_loop: &agent_loop,
                sink: TurnSink::Json(&mut streams.out),
                session_key: session.attachment.session_key.clone(),
                workspace_id: session.attachment.workspace_id.clone(),
                agent_id: chosen,
                token: token.clone(),
            },
            content,
        )
        .await;
    }

    let turn = run_turn(
        RunTurnDeps {
            agent_loop: &agent_loop,
            sink: TurnSink::Rendered(&mut renderer),
            session_key: session.attachment.session_key.clone(),
            workspace_id: session.attachment.workspace_id.clone(),
            agent_id: chosen,
            token: token.clone(),
        },
        content,
    );
    streamed(turn, &mut pending, &mut streams.out).await
}

// ------------------------------------------------------------- streaming

/// A sink that hands every event to whoever is draining it.
///
/// The alternative, a buffer the driver reads once the turn is over, is what
/// made an answer arrive all at once at the end. A channel is what lets the
/// same events reach a pipe, or a frame's transcript, as the model produces
/// them, without the renderer having to know which one it got.
#[derive(Clone, Debug)]
pub struct ChunkSink(mpsc::UnboundedSender<TranscriptEvent>);

impl ChunkSink {
    /// A closed receiver means the surface has already gone; the turn is on its
    /// way out behind it and has nothing useful to do about the loss.
    fn send(&self, event: TranscriptEvent) {
        let _ = self.0.send(event);
    }
}

impl TranscriptSink for ChunkSink {
    fn emit(&mut self, event: TranscriptEvent) {
        // The frame keeps the plan above the box you type into, so the card a
        // `todo` call announces would be the same list a second time in the
        // conversation, one copy per revision.
        if matches!(event, TranscriptEvent::TasksCard { .. }) {
            return;
        }
        self.send(event);
    }
}

/// A sink and the receiver that drains it.
#[must_use]
pub fn chunks() -> (ChunkSink, mpsc::UnboundedReceiver<TranscriptEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (ChunkSink(tx), rx)
}

/// A session's stored messages, as the events a live turn would have emitted.
///
/// Built here because a renderer needs the settings a turn's renderer is built
/// with, and those are assembled in this file. A surface is handed the events
/// and never the store: drawing a conversation is its job, and reading one is
/// not.
///
/// Empty for a session with nothing in it, and for one whose messages cannot be
/// read. A prompt that refused to open because a row was unreadable would be a
/// prompt lost to a conversation nobody can leave.
pub fn replayed(session: &ChatSession) -> Vec<TranscriptEvent> {
    let Ok(history) =
        crate::messages::session_messages(session.runtime.store(), &session.attachment.session_key)
    else {
        return Vec::new();
    };
    if history.is_empty() {
        return Vec::new();
    }

    let risks: HashMap<String, ToolRisk> = session
        .runtime
        .tools()
        .definitions()
        .iter()
        .map(|definition| (definition.name.clone(), definition.risk))
        .collect();

    let (sink, mut events) = chunks();
    let mut renderer = TurnRenderer::new(TurnRendererOptions {
        out: Box::new(sink),
        colors: session.colors,
        show_reasoning: session.reasoning_reaches_a_reader(),
        show_stats: session.runtime.config().ui.expand_turn_stats,
        t: Translations::new(session.t.locale()),
        ..TurnRendererOptions::new(Box::new(crate::render::NullSink))
    });
    renderer.replay(&session.attachment.session_key, &history, &|name| {
        risks.get(name).copied().unwrap_or(ToolRisk::Safe)
    });
    drop(renderer);

    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        out.push(event);
    }
    out
}

/// Drives a turn, putting every byte it renders on `out` as it arrives.
///
/// `biased` so a chunk that is already waiting is written before the turn's
/// own completion is noticed: without it the last delta and the end of the
/// turn race, and the tail of an answer lands after the summary line.
async fn streamed(
    turn: impl Future<Output = Result<TurnOutcome>>,
    chunks: &mut mpsc::UnboundedReceiver<TranscriptEvent>,
    out: &mut (dyn Write + Send),
) -> Result<TurnOutcome> {
    tokio::pin!(turn);
    let mut printer = PlainPrinter::new();
    let outcome = loop {
        tokio::select! {
            biased;
            Some(event) = chunks.recv() => {
                out.write_all(printer.bytes(&event).as_bytes()).map_err(WireError::from)?;
                out.flush().map_err(WireError::from)?;
            }
            done = &mut turn => break done,
        }
    };
    while let Ok(event) = chunks.try_recv() {
        out.write_all(printer.bytes(&event).as_bytes())
            .map_err(WireError::from)?;
    }
    out.flush().map_err(WireError::from)?;
    outcome
}

// ---------------------------------------------------------------- surfaces

/// A boxed future, because the surface below is used as a trait object.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// What the prompt loop talks to, so that it does not have to know which one
/// it got.
///
/// There are two, and the difference is whether the output is a terminal. On
/// one it is the frame; on a pipe it is a prompt and a newline, because escape
/// sequences written into a file are not a status bar, they are noise in
/// somebody's log.
///
/// Public because it is the seam a test drives the whole prompt through: a
/// scripted surface answers lines from a list and records what was drawn, and
/// the loop below cannot tell the difference.
pub trait Surface {
    /// Where a slash command opens a picker.
    ///
    /// Shared rather than borrowed, so a command holding it can run while the
    /// surface it came from is being driven: a picker only answers because
    /// something is still reading the keyboard, and that something needs the
    /// surface. A borrow here would make the two mutually exclusive.
    fn menu(&self) -> Arc<dyn PickerMenu>;

    /// The sink every renderer in this session writes through.
    fn sink(&self) -> ChunkSink;

    /// Blocks until a line is submitted, or `None` to leave.
    fn next_line(&mut self) -> BoxFut<'_, Option<String>>;

    /// Runs the turn, drawing whatever this surface shows while one runs.
    ///
    /// The token is the turn's, and the surface is what holds the key that
    /// cancels it: while a turn runs an interrupt belongs to the turn, and at
    /// an idle prompt the same key means "leave".
    fn run<'a>(
        &'a mut self,
        token: &'a CancellationToken,
        body: BoxFut<'a, Result<TurnOutcome>>,
    ) -> BoxFut<'a, Result<TurnOutcome>>;

    /// Shows the submitted line, since the block it was typed into is gone.
    fn echo<'a>(&'a mut self, content: &'a str) -> BoxFut<'a, ()>;

    /// The prompt has moved to another conversation: draw that one instead.
    ///
    /// Only a surface that *holds* the conversation has anything to do here. A
    /// pipe has already written the old one to the stream and cannot take it
    /// back, so the default is to do nothing.
    ///
    /// The view comes with the history because the header names the session,
    /// and the one this surface is holding still names the session that was
    /// left. Reading it from a field would put the old name on the new
    /// conversation.
    fn reopen<'a>(
        &'a mut self,
        view: &'a HeaderView,
        history: &'a [TranscriptEvent],
    ) -> BoxFut<'a, ()> {
        let _ = (view, history);
        Box::pin(std::future::ready(()))
    }

    /// Something the surface draws has changed.
    ///
    /// Also where anything a slash command rendered reaches the screen: it
    /// wrote through the same sink a turn does, and this is the point the
    /// prompt is next redrawn.
    fn refresh<'a>(&'a mut self, view: &'a HeaderView) -> BoxFut<'a, ()>;

    /// Puts a command on the composer line, for the operator to finish.
    ///
    /// What a modal does instead of growing a text field. A workspace being
    /// renamed needs a name typed, and a workspace being removed deserves a
    /// look before it goes; both are a line in the prompt that is already
    /// there. Nothing to do where there is no composer, so a pipe ignores it.
    fn compose<'a>(&'a mut self, text: &'a str) -> BoxFut<'a, ()> {
        let _ = text;
        Box::pin(std::future::ready(()))
    }

    /// Lays the whole transcript over the prompt, as `ctrl-t` does.
    ///
    /// `false` where there is nowhere to lay one, which is what tells the
    /// caller to say so rather than appearing to have done nothing. A pipe
    /// has already written every one of those rows to the stream.
    fn transcript(&mut self) -> BoxFut<'_, bool> {
        Box::pin(std::future::ready(false))
    }

    /// What a key did to `/output stats` since this was last asked.
    ///
    /// The frame owns that switch while a key is pressed, because only it can
    /// fold rows already drawn, and the renderer owns it the rest of the time,
    /// because only it decides whether a pipe sees the row. A surface with no
    /// keyboard answers `None` and nothing changes.
    fn take_stats_shown(&mut self) -> BoxFut<'_, Option<bool>> {
        Box::pin(std::future::ready(None))
    }

    /// Runs something that is not a turn, drawing while it runs.
    ///
    /// A slash command can open a picker, and a picker only answers when
    /// somebody is reading the keyboard. On a pipe nothing is, so the default
    /// is to await the command and nothing else — which is also why the
    /// default is correct rather than merely harmless.
    fn attend<'a>(&'a mut self, body: BoxFut<'a, Flow>) -> BoxFut<'a, Flow> {
        body
    }

    /// Puts the terminal back.
    fn close(&mut self) -> BoxFut<'_, ()>;
}

/// What a slash command left for the loop to do.
///
/// Public because [`Surface::attend`] names it.
pub enum Flow {
    /// Leave.
    Exit,
    /// Draw the prompt again.
    Again,
    /// The prompt is on another conversation now, so the screen is the wrong
    /// one: what is on it belongs to the session that was just left.
    Attached,
    /// Run this as a turn, by the path a typed message takes.
    Turn(String),
    /// Put this on the composer and wait: the operator finishes it.
    Compose(String),
    /// Lay the whole transcript over the prompt.
    Transcript,
}

/// The prompt loop.
///
/// Everything about *what* a line means lives here — a slash command, a
/// message, a re-run — and everything about how it is drawn lives in the
/// surface. That split is what lets a piped stdout keep working unchanged
/// while a terminal gets a frame.
pub async fn drive_prompt(
    session: &mut ChatSession,
    args: &ChatArgs,
    surface: &mut dyn Surface,
) -> Result<u8> {
    let mut renderer = TurnRenderer::new(TurnRendererOptions {
        out: Box::new(surface.sink()),
        colors: session.colors,
        show_reasoning: args.show_reasoning && session.reasoning_reaches_a_reader(),
        show_stats: session.runtime.config().ui.expand_turn_stats,
        t: Translations::new(session.t.locale()),
        ..TurnRendererOptions::new(Box::new(crate::render::NullSink))
    });

    loop {
        let Some(line) = surface.next_line().await else {
            surface.close().await;
            return Ok(0);
        };
        // Before the slash dispatch, so `/output` typed straight after Ctrl-Y
        // prints the truth. The frame has already folded what is on screen;
        // this is the half that decides whether a pipe sees the next one.
        if let Some(shown) = surface.take_stats_shown().await {
            renderer.set_stats_shown(shown);
        }

        let typed = line.trim().to_owned();
        if typed.is_empty() {
            continue;
        }
        surface.echo(&typed).await;

        let content = if typed.starts_with('/') {
            // The menu comes out first so the command can hold it while the
            // surface below is busy drawing for it.
            let menu = surface.menu();
            let command = Box::pin(slash(session, menu, &mut renderer, &typed));
            match surface.attend(command).await {
                Flow::Exit => {
                    surface.close().await;
                    return Ok(0);
                }
                // `/clear` and `/branch` change the history without running a
                // turn, and a different conversation is a different context.
                Flow::Again => {
                    session.measure().await;
                    surface.refresh(&session.view()).await;
                    continue;
                }
                // `/session`, `/new` and `/branch` move the prompt. What is on
                // screen is the conversation that was left, so it is replaced
                // by the one that was joined rather than written under it.
                Flow::Attached => {
                    let history = replayed(session);
                    // Measured first, so the view handed to both of these is
                    // the conversation being joined rather than the one left.
                    session.measure().await;
                    let view = session.view();
                    surface.reopen(&view, &history).await;
                    surface.refresh(&view).await;
                    continue;
                }
                // `/edit` and `/regenerate` truncated and handed the content
                // back rather than running it, so the re-run takes the same
                // path a typed message does — same renderer, same interrupt.
                Flow::Turn(content) => content,
                // Drawn first and filled second: `refresh` redraws the prompt,
                // and a line put on the composer before it would be drawn over.
                // The same overlay `ctrl-t` opens, for somebody who does not
                // know the key. Nothing is written either way, so there is
                // nothing to measure and nothing to refresh afterwards: the
                // conversation is exactly where it was.
                Flow::Transcript => {
                    if !surface.transcript().await {
                        renderer.note(&session.t.t(keys::slash::notes::NO_TRANSCRIPT));
                    }
                    continue;
                }
                Flow::Compose(text) => {
                    session.measure().await;
                    surface.refresh(&session.view()).await;
                    surface.compose(&text).await;
                    continue;
                }
            }
        } else {
            typed
        };

        let token = CancellationToken::new();
        let outcome = {
            let body = Box::pin(prompt_turn(session, &mut renderer, &content, &token));
            surface.run(&token, body).await
        };
        match outcome {
            Ok(outcome) if outcome.aborted => {
                renderer.note(&session.t.t(keys::chat::INTERRUPTED));
            }
            Ok(_) => {}
            // The prompt outlives a failed turn: a provider that refused one
            // question is no reason to throw away the conversation.
            Err(error) => renderer.warn(&describe_error(&error)),
        }
        // The name `/new <title>` asked for, now that there is something to
        // name. After the turn rather than before it, because the turn is what
        // writes the row — and over whatever the loop derived from the first
        // message, because a name somebody typed beats one inferred.
        session.apply_pending_title();
        // After the turn, never before a keystroke: the context only changes
        // when the history does, so measuring here is both the cheap answer
        // and the exact one.
        session.measure().await;
        surface.refresh(&session.view()).await;
    }
}

/// One slash command, and what it left the loop to do.
async fn slash(
    session: &mut ChatSession,
    menu: Arc<dyn PickerMenu>,
    renderer: &mut TurnRenderer,
    input: &str,
) -> Flow {
    // What was waiting before the command ran, so the branch below can tell a
    // name this command set from one it merely left alone.
    let waiting = session.attachment.pending_title.clone();
    let outcome = {
        let mut ctx = SlashContext {
            renderer,
            runtime: &session.runtime,
            t: &session.t,
            session_key: &session.attachment.session_key,
            workspace_id: &mut session.attachment.workspace_id,
            agent_id: &mut session.attachment.agent_id,
            pending_title: &mut session.attachment.pending_title,
            menu: menu.as_ref(),
            models: &session.models,
            model_pinned: session.model_pinned,
        };
        run_slash_command(input, &mut ctx).await
    };
    match outcome {
        SlashOutcome::Exit => Flow::Exit,
        SlashOutcome::Continue => Flow::Again,
        SlashOutcome::Attach(key) => {
            // A name waiting for *this* conversation does not follow the prompt
            // to another one. Unless the command that moved it is the one that
            // asked for the name, which is `/new <title>`.
            if session.attachment.pending_title == waiting {
                session.attachment.pending_title = None;
            }
            // The prompt moves; nothing is written. A conversation exists once
            // something has been said in it, and the turn is what says it —
            // with the workspace and the agent it actually ran under, which is
            // more than anything here knows. Until then this is a name.
            renderer.note(
                &session
                    .t
                    .tr(keys::chat::ATTACHED_TO, args!["key" => key.as_str()]),
            );
            session.attachment.session_key = key;
            Flow::Attached
        }
        SlashOutcome::Turn(content) => Flow::Turn(content),
        SlashOutcome::Compose(text) => Flow::Compose(text),
        SlashOutcome::Transcript => Flow::Transcript,
    }
}

/// One turn on whichever loop the session's agent resolves to, rendered
/// through the sink the surface handed out.
async fn prompt_turn(
    session: &mut ChatSession,
    renderer: &mut TurnRenderer,
    content: &str,
    token: &CancellationToken,
) -> Result<TurnOutcome> {
    let chosen = session.agent_for_this_turn(renderer);
    let agent_loop = session.runtime.require_loop_for(chosen.as_deref())?;
    run_turn(
        RunTurnDeps {
            agent_loop: &agent_loop,
            sink: TurnSink::Rendered(renderer),
            session_key: session.attachment.session_key.clone(),
            workspace_id: session.attachment.workspace_id.clone(),
            agent_id: chosen,
            token: token.clone(),
        },
        Content::Text(content.to_owned()),
    )
    .await
}

// ------------------------------------------------------------ plain prompt

/// Lines in, lines out, for a stdout that is not a terminal.
///
/// `darkwire chat > log` and `darkwire chat | tee` still open a prompt, because
/// stdin is still a keyboard — but nothing here moves a cursor. There is no
/// frame, no status bar and no menu, and [`NoMenu`] is what makes that last
/// part a property of the type rather than an `if` at every call site.
pub struct PlainSurface<'a> {
    out: &'a mut (dyn Write + Send),
    lines: mpsc::UnboundedReceiver<String>,
    sink: ChunkSink,
    chunks: mpsc::UnboundedReceiver<TranscriptEvent>,
    /// The line discipline for a stream that cannot fold anything.
    printer: PlainPrinter,
    menu: Arc<dyn PickerMenu>,
}

impl std::fmt::Debug for PlainSurface<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PlainSurface")
    }
}

impl<'a> PlainSurface<'a> {
    /// Opens the prompt, having printed the banner.
    ///
    /// Reads stdin on a thread rather than in the task, because a blocking
    /// device read in the middle of the loop is what stops an answer streaming
    /// while somebody is typing the next question.
    pub fn open(out: &'a mut (dyn Write + Send), header: &str) -> Result<PlainSurface<'a>> {
        writeln!(out, "{header}").map_err(WireError::from)?;
        let (tx, lines) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            for line in std::io::stdin().lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let (sink, chunks) = chunks();
        Ok(PlainSurface {
            out,
            lines,
            sink,
            chunks,
            printer: PlainPrinter::new(),
            menu: Arc::new(NoMenu),
        })
    }
}

impl Surface for PlainSurface<'_> {
    fn menu(&self) -> Arc<dyn PickerMenu> {
        self.menu.clone()
    }

    fn sink(&self) -> ChunkSink {
        self.sink.clone()
    }

    fn next_line(&mut self) -> BoxFut<'_, Option<String>> {
        Box::pin(async move {
            let _ = write!(self.out, "\n› ");
            let _ = self.out.flush();
            tokio::select! {
                line = self.lines.recv() => line,
                // At an idle prompt an interrupt means "leave", which is what
                // the shell's own would have meant.
                signal = tokio::signal::ctrl_c() => {
                    signal.ok()?;
                    None
                }
            }
        })
    }

    fn run<'a>(
        &'a mut self,
        token: &'a CancellationToken,
        body: BoxFut<'a, Result<TurnOutcome>>,
    ) -> BoxFut<'a, Result<TurnOutcome>> {
        Box::pin(async move {
            // Not raw mode, so the interrupt arrives as a signal rather than
            // as a keystroke. It still belongs to the turn.
            let watcher = {
                let token = token.clone();
                tokio::spawn(async move {
                    if tokio::signal::ctrl_c().await.is_ok() {
                        token.cancel();
                    }
                })
            };
            let outcome = streamed(body, &mut self.chunks, self.out).await;
            watcher.abort();
            outcome
        })
    }

    fn echo<'a>(&'a mut self, content: &'a str) -> BoxFut<'a, ()> {
        // The terminal echoed the line as it was typed; repeating it would
        // print every question twice.
        let _ = content;
        Box::pin(std::future::ready(()))
    }

    fn refresh<'a>(&'a mut self, view: &'a HeaderView) -> BoxFut<'a, ()> {
        // Nothing on this surface is drawn twice — but a slash command's note
        // went through the sink like everything else, and this is where it
        // reaches the stream.
        let _ = view;
        Box::pin(async move {
            while let Ok(event) = self.chunks.try_recv() {
                let _ = self.out.write_all(self.printer.bytes(&event).as_bytes());
            }
            let _ = self.out.flush();
        })
    }

    fn close(&mut self) -> BoxFut<'_, ()> {
        Box::pin(async move {
            while let Ok(event) = self.chunks.try_recv() {
                let _ = self.out.write_all(self.printer.bytes(&event).as_bytes());
            }
            let _ = self.out.flush();
        })
    }
}
