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
//! **The frame owns the screen.** On a terminal one renderer draws the
//! conversation, the editor, the status bar and any open menu, because a
//! resize invalidates every row at once and only something holding all of them
//! can redraw them consistently. On a pipe there is no frame at all: a prompt
//! and a newline, since escape sequences written into a file are not a status
//! bar, they are noise in somebody's log.

use std::future::Future;
use std::io::Write;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use darkwire_agent::{AgentLoop, PromptPreviewInput, describe_context};
use darkwire_core::messages::Content;
use darkwire_core::session_store::CreateSession;
use darkwire_core::{Result, WireError};
use darkwire_i18n::{args, keys};
use darkwire_protocol::config::ReasoningDisplay;
use darkwire_protocol::tasks::TaskStatus;
use darkwire_protocol::{DEFAULT_AGENT_ID, DEFAULT_WORKSPACE_ID, StopReason};
use darkwire_runtime::{RuntimeOptions, WireRuntime};
use darkwire_server::agent_for_turn;
use darkwire_tui::{
    CHROME_ROWS, Component, DEFAULT_MAX_ROWS, Editor, EditorOutcome, FRAME_INTERVAL_MS, Key,
    KeyName, Renderer, RendererOptions, SPINNER_INTERVAL_MS, Select, SelectItem, SelectList,
    SelectOptions, SelectOutcome, StandardInput, StandardOutput, TerminalInput, Theme, Transcript,
    columns_of, is_ctrl, open_keyboard, spinner_frame, theme_for, truncate_to_width,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::Streams;
use crate::commands::{SlashContext, SlashOutcome, run_slash_command};
use crate::header::{ContextUsage, HeaderView, input_rule, startup_header, status_bar};
use crate::i18n::{Env, Translations, describe_error};
use crate::menu::{MenuAvailable, menu_available};
use crate::models::{ModelCatalogue, ModelCatalogueOptions, create_model_catalogue};
use crate::pickers::palette::{CommandChoice, command_items, complete_command, pick_command};
use crate::pickers::{MenuRequest as PickerRequest, NoMenu, PickerMenu};
use crate::program::{ChatArgs, Globals};
use crate::render::{
    LineKind, PlainPrinter, TranscriptEvent, TranscriptSink, TurnRenderer, TurnRendererOptions,
    format_duration,
};
use crate::runtime::{env_map, install_logger, settings_of};

/// How many streamed chunks the pump takes before it must draw.
///
/// Absorbing a chunk is cheap and drawing is not, so the loop prefers to drain;
/// but a provider that never stops arriving would hold the first arm of a
/// `biased` select forever and nothing else would run. This is the ceiling that
/// makes the frame tick, the keyboard and the interrupt reachable under a
/// flood.
const CHUNKS_PER_FRAME: usize = 64;

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
}

/// Everything a chat run was asked for, after the flags were read.
pub struct ChatSession {
    runtime: Arc<WireRuntime>,
    t: Translations,
    theme: Theme,
    colors: Option<bool>,
    attachment: Attachment,
    /// Set while `--model` pinned the model for this process.
    model_pinned: bool,
    /// What the last turn left in the window.
    context: Option<ContextUsage>,
    /// What `/model` lists, cached for the life of the prompt.
    models: ModelCatalogue,
    /// The process environment, for the one question the prompt asks of it:
    /// whether this terminal can draw a menu at all.
    env: Env,
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
    fn view(&self) -> HeaderView {
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
            session: match opened {
                Some(session) if !session.title.is_empty() => session.title,
                _ => self.attachment.session_key.clone(),
            },
            context: self.context,
        }
    }
}

/// Builds the runtime and the session state one chat run works over.
///
/// Separate from [`run`] so a test can drive a whole prompt without a terminal
/// or a signal handler.
pub fn open(globals: &Globals, args: &ChatArgs, env: &Env) -> Result<ChatSession> {
    let runtime = darkwire_runtime::create_runtime(RuntimeOptions {
        home: globals.home.clone(),
        workspaces: args.workspaces.clone(),
        model: args.model.clone(),
        provider: args.provider.clone(),
        tools: args.tools,
        env: Some(env_map(env)),
        ..RuntimeOptions::default()
    })?;

    if args.fresh {
        runtime.store().clear_messages(&args.session_key)?;
    }

    // After the runtime, because this is the first point the install's own
    // answer exists — `config.ui.locale` sits under `DARKWIRE_LANG` and above the
    // shell's `LANG` in the order the resolution applies.
    let t = Translations::for_env(env, Some(&runtime.config().ui.locale));

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
            session_key: args.session_key.clone(),
            workspace_id: args.workspace_id.clone(),
            agent_id: args.agent_id.clone(),
        },
        model_pinned: args.model.is_some(),
        context: None,
        runtime,
        t,
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
    session.runtime.close().await;
    code
}

/// Whichever of the three shapes this invocation is.
async fn drive(session: &mut ChatSession, args: &ChatArgs, streams: &mut Streams) -> Result<u8> {
    let input = StandardInput;
    // A message argument, then anything piped in. A prompt on a stdin that is
    // not a terminal would read its first line as a question and then see EOF.
    let one_shot = match args.message.clone() {
        Some(message) => Some(message),
        None if input.is_tty() => None,
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
    let framed = menu_available(&MenuAvailable {
        input: &StandardInput,
        output: &StandardOutput,
        json: args.json,
        env: &session.env,
    });
    if framed {
        let mut surface = FramedSurface::open(session);
        return drive_prompt(session, args, &mut surface).await;
    }
    let width = columns_of(&StandardOutput, None);
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
        // `todo` call announces would be the same list a second time, in the
        // scrollback, one copy per revision.
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
type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

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
    fn menu(&self) -> &dyn PickerMenu;

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

    /// Something the surface draws has changed.
    ///
    /// Also where anything a slash command rendered reaches the screen: it
    /// wrote through the same sink a turn does, and this is the point the
    /// prompt is next redrawn.
    fn refresh<'a>(&'a mut self, view: &'a HeaderView) -> BoxFut<'a, ()>;

    /// What a key did to `/output stats` since this was last asked.
    ///
    /// The frame owns that switch while a key is pressed, because only it can
    /// fold rows already drawn, and the renderer owns it the rest of the time,
    /// because only it decides whether a pipe sees the row. A surface with no
    /// keyboard answers `None` and nothing changes.
    fn take_stats_shown(&mut self) -> BoxFut<'_, Option<bool>> {
        Box::pin(std::future::ready(None))
    }

    /// Puts the terminal back.
    fn close(&mut self) -> BoxFut<'_, ()>;
}

/// What a slash command left for the loop to do.
enum Flow {
    /// Leave.
    Exit,
    /// Draw the prompt again.
    Again,
    /// Run this as a turn, by the path a typed message takes.
    Turn(String),
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
            match slash(session, surface, &mut renderer, &typed).await {
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
                // `/edit` and `/regenerate` truncated and handed the content
                // back rather than running it, so the re-run takes the same
                // path a typed message does — same renderer, same interrupt.
                Flow::Turn(content) => content,
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
    surface: &dyn Surface,
    renderer: &mut TurnRenderer,
    input: &str,
) -> Flow {
    let outcome = {
        let mut ctx = SlashContext {
            renderer,
            runtime: &session.runtime,
            t: &session.t,
            session_key: &session.attachment.session_key,
            workspace_id: &mut session.attachment.workspace_id,
            agent_id: &mut session.attachment.agent_id,
            menu: surface.menu(),
            models: &session.models,
            model_pinned: session.model_pinned,
        };
        run_slash_command(input, &mut ctx).await
    };
    match outcome {
        SlashOutcome::Exit => Flow::Exit,
        SlashOutcome::Continue => Flow::Again,
        SlashOutcome::Attach(key) => {
            // The row is created here rather than at the first message,
            // because the prompt now says it is attached to this conversation
            // and `/sessions` listing nothing under that name would make the
            // statement look untrue.
            let created = session.runtime.store().ensure_session(
                &key,
                CreateSession {
                    origin: Some("cli".to_owned()),
                    ..CreateSession::default()
                },
            );
            if let Err(error) = created {
                renderer.warn(&describe_error(&error));
                return Flow::Again;
            }
            renderer.note(
                &session
                    .t
                    .tr(keys::chat::ATTACHED_TO, args!["key" => key.as_str()]),
            );
            session.attachment.session_key = key;
            Flow::Again
        }
        SlashOutcome::Turn(content) => Flow::Turn(content),
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
    menu: NoMenu,
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
            menu: NoMenu,
        })
    }
}

impl Surface for PlainSurface<'_> {
    fn menu(&self) -> &dyn PickerMenu {
        &self.menu
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

// ----------------------------------------------------------- framed prompt

/// What the frame draws, as one component.
///
/// The whole screen, deliberately: a resize invalidates every row at once, and
/// only something holding the conversation, the editor and the status together
/// can print them again consistently. A frame with a separate line editor
/// inside it is a frame nobody owns, which is what left a stranded copy of the
/// footer behind on every resize.
pub struct Frame {
    transcript: Transcript,
    editor: Editor,
    overlay: Option<Select<usize>>,
    /// The command list, while a slash command is being typed.
    ///
    /// Beside the editor rather than over it, which is the whole difference
    /// from the palette above: the palette replaces everything below the
    /// conversation and owns the keyboard, and this sits between the
    /// conversation and the rule while what you typed stays visible and
    /// editable. A list that hid the line it was filtering would be a list you
    /// had to close to see what you had asked for.
    popup: Option<SelectList<CommandChoice>>,
    /// Every command this install has, for the list above.
    commands: Vec<SelectItem<CommandChoice>>,
    /// Spinner frame while a turn has said nothing yet; absent once it has.
    thinking: Option<i64>,
    theme: Theme,
    generating: String,
    /// What the bar at the bottom says, as the record rather than the rows.
    ///
    /// Drawn at render time, not kept as strings. The rows are justified to the
    /// window, so a copy built at one width is wrong at every other: a narrower
    /// window has the renderer cut the row, and what it cuts is the right-hand
    /// side, which is the half naming the model. A resize used to lose it.
    view: HeaderView,
    /// How a *new* run of each kind arrives, and what Ctrl-T and Ctrl-O set.
    ///
    /// Held apart from each block's own state, and that is the whole of what
    /// makes the key sensible: a run that has gone to the scrollback cannot be
    /// rewritten, so a key that only reached what is on screen would stop
    /// working the moment the screen filled. This is the half that keeps
    /// working: press it once and every run after it arrives the way you asked.
    folds: FoldDefaults,
    /// Ticks since the open reasoning run started, for a summary that moves.
    reasoning_since: Option<i64>,
    /// Whether the transcript's open line has been ended.
    ///
    /// The line discipline the renderer used to keep. It belongs here because
    /// only the thing holding the rows knows whether one is half written, and
    /// a pipe drawing the same events answers the question differently.
    at_line_start: bool,
    /// What a key last did to the row saying what a turn cost, until taken.
    ///
    /// The switch has two owners and this is the wire between them. See
    /// [`Frame::take_stats_toggle`].
    stats_toggled: Option<bool>,
    /// The plan the agent is running on, as rows above the input.
    ///
    /// Frame state, not transcript: it is rewritten rather than appended, so a
    /// turn that revises its plan six times leaves one list on screen and
    /// nothing at all in the scrollback. Putting it in the conversation would
    /// leave six stale copies of it there.
    tasks: Vec<(TaskStatus, String)>,
    /// The words a fold's summary is built from, translated once.
    ///
    /// Strings rather than a `Translations`, and not by preference: the frame
    /// lives behind a mutex shared with a `Send` future, and the translator
    /// holds an `Rc`. `generating` above is the same arrangement for the same
    /// reason.
    labels: FoldLabels,
}

/// The two words a run of reasoning is labelled with.
#[derive(Debug, Clone, Default)]
pub struct FoldLabels {
    /// While it is still going.
    pub thinking: String,
    /// Once it has finished.
    pub thought: String,
    /// What Ctrl-T says when there is no reasoning to unfold.
    pub reasoning_off: String,
}

impl FoldLabels {
    /// The two words, from a translator the frame cannot hold on to.
    #[must_use]
    pub fn from(t: &Translations) -> FoldLabels {
        FoldLabels {
            thinking: t.t(keys::chat::folds::THINKING),
            thought: t.t(keys::chat::folds::THOUGHT),
            reasoning_off: t.t(keys::chat::folds::REASONING_OFF),
        }
    }
}

/// Which kinds of run arrive folded away.
///
/// The defaults are both folded, and that is the argument the whole feature
/// rests on: a turn is read for its answer, and a terminal that prints every
/// line of the reasoning and every line of every tool result buries the one
/// thing the reader came for under the four things they can ask for.
#[derive(Debug, Clone, Copy)]
pub struct FoldDefaults {
    /// How much of the model's reasoning arrives at all.
    pub reasoning: ReasoningDisplay,
    /// Whether a tool's output arrives folded.
    pub tools: bool,
    /// Whether what the turn cost arrives folded.
    pub stats: bool,
}

impl Default for FoldDefaults {
    /// All three folded. Spelled out rather than derived, because `false` is
    /// the derived answer for the two switches and it is the wrong one.
    fn default() -> Self {
        Self {
            reasoning: ReasoningDisplay::Collapsed,
            tools: true,
            stats: true,
        }
    }
}

impl FoldDefaults {
    /// What the install asked for.
    #[must_use]
    pub fn from(ui: &darkwire_protocol::config::UiConfig) -> FoldDefaults {
        FoldDefaults {
            reasoning: ui.reasoning,
            tools: !ui.expand_tool_output,
            stats: !ui.expand_turn_stats,
        }
    }
}

/// How many tasks the frame shows at once. See [`Frame::task_rows`].
const TASK_ROWS: usize = 3;

/// One task, marked by where it has got to.
///
/// The same three marks the tool card prints, because they are the same three
/// states and a reader should not have to learn them twice. The mark carries
/// the distinction and the colour carries only one thing: which row is in hand.
/// The frame's palette is deliberately small, and a plan painted in three
/// colours above the box you type into would be the loudest thing on screen.
fn task_row(theme: &Theme, status: TaskStatus, text: &str) -> String {
    match status {
        TaskStatus::Done => format!("  {} {}", theme.dim.apply("✓"), theme.dim.apply(text)),
        TaskStatus::Doing => format!("  {} {}", theme.accent.apply("▸"), theme.text.apply(text)),
        TaskStatus::Todo => format!("  {} {}", theme.dim.apply("☐"), theme.dim.apply(text)),
    }
}

/// The tag a run of reasoning carries in the transcript.
const REASONING_FOLD: &str = "reasoning";
/// The tag a tool's output carries.
const TOOL_FOLD: &str = "tool";
/// The tag the row saying what a turn cost carries.
const STATS_FOLD: &str = "stats";

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Frame")
    }
}

impl Frame {
    /// An empty frame over one theme.
    ///
    /// Public so that the input rule below can be driven without a terminal:
    /// what a keystroke means is the part worth asserting, and a real screen
    /// is not.
    #[must_use]
    pub fn new(theme: Theme, generating: &str, labels: FoldLabels) -> Frame {
        Frame {
            transcript: Transcript::new(),
            editor: Editor::new(&theme),
            overlay: None,
            popup: None,
            commands: Vec::new(),
            thinking: None,
            theme,
            generating: generating.to_owned(),
            view: HeaderView::default(),
            folds: FoldDefaults::default(),
            reasoning_since: None,
            at_line_start: true,
            stats_toggled: None,
            tasks: Vec::new(),
            labels,
        }
    }

    /// One thing the turn said.
    ///
    /// Every event carries its own kind, so a fold is a property of what
    /// arrived rather than a guess about where a write landed. A pipe reads the
    /// same events and renders them flat, which is the point of the kind being
    /// on the event rather than a signal beside it.
    pub fn absorb(&mut self, event: &TranscriptEvent) {
        match event {
            TranscriptEvent::AssistantDelta { text, depth } => {
                self.stream(text, *depth);
                // The spinner stood in for an answer that had not started. A
                // fold opening is not an answer starting, which is why this is
                // here and not on every event: a collapsed run of reasoning
                // would otherwise clear the one thing on screen that was moving.
                if self.reasoning_since.is_none() {
                    self.thinking = None;
                }
            }
            TranscriptEvent::ReasoningDelta { text, depth } => {
                self.stream(text, *depth);
            }
            TranscriptEvent::Line { kind, text } => {
                // The one row of space between one exchange and the next. A
                // layout rule read off the kind, not a newline the renderer had
                // to remember to write.
                if *kind == LineKind::Echo {
                    self.write_line("");
                }
                self.write_line(text);
                if self.reasoning_since.is_none() {
                    self.thinking = None;
                }
            }
            // A stream that ended mid-line closes it. The transcript no longer
            // draws the line a write left open, so this is what makes its last
            // row text rather than an empty row waiting for more.
            TranscriptEvent::EndLine => self.end_line(),
            TranscriptEvent::ReasoningStart => {
                self.reasoning_since = Some(0);
                let summary = self.reasoning_summary(false);
                let collapsed = self.folds.reasoning != ReasoningDisplay::Expanded;
                self.transcript
                    .open_block(REASONING_FOLD, &summary, collapsed);
            }
            TranscriptEvent::ReasoningEnd => {
                let summary = self.reasoning_summary(true);
                self.transcript.set_summary(&summary);
                self.transcript.close_block();
                self.reasoning_since = None;
            }
            TranscriptEvent::ToolBodyStart { summary } => {
                self.transcript.open_block(
                    TOOL_FOLD,
                    summary.trim_end_matches('\n'),
                    self.folds.tools,
                );
                self.at_line_start = true;
            }
            TranscriptEvent::ToolBodyEnd => self.transcript.close_block(),
            TranscriptEvent::Tasks(tasks) => self.set_tasks(tasks),
            // The frame keeps the plan above the box you type into, so the card
            // never reaches here. `ChunkSink` drops it at the source.
            TranscriptEvent::TasksCard { .. } => {}
            // `/output reasoning off` and `ui.reasoning: hidden` are the same
            // state reached two ways, so the fold has to read them the same
            // way: Ctrl-T says where the switch is rather than pretending there
            // is something folded away.
            TranscriptEvent::ReasoningShown(shown) => {
                self.folds.reasoning = if *shown {
                    ReasoningDisplay::Collapsed
                } else {
                    ReasoningDisplay::Hidden
                };
            }
            // Kept whatever the switch says, in a run that shows nothing while
            // it is folded. Opened, written and closed in one step: a run left
            // open holds the live region, and one that said nothing is dropped
            // rather than kept as a fold onto an empty body.
            TranscriptEvent::TurnStats { line, .. } => {
                self.transcript.hide_block(STATS_FOLD, self.folds.stats);
                self.transcript.write(line);
                self.transcript.close_block();
                self.at_line_start = true;
            }
            // Set rather than flipped: this is the command's half of one switch
            // arriving, and a flip here would undo what was asked for.
            TranscriptEvent::StatsShown(shown) => {
                self.folds.stats = !*shown;
                self.transcript.set_collapsed(STATS_FOLD, self.folds.stats);
            }
        }
    }

    /// A chunk of streamed text, indented for the turn it belongs to.
    ///
    /// The indent is applied here rather than at the source because a chunk may
    /// start mid-line: a subagent's answer indented on whichever line a chunk
    /// happened to begin, and flush left everywhere else, is what doing it any
    /// earlier produces.
    fn stream(&mut self, text: &str, depth: usize) {
        if text.is_empty() {
            return;
        }
        let indent = "  ".repeat(depth);
        if indent.is_empty() {
            self.transcript.write(text);
        } else {
            let body = crate::render::indented(text, &indent, self.at_line_start);
            self.transcript.write(&body);
        }
        // Measured on what a reader sees. Dimmed reasoning ends in a closing
        // sequence however its prose ended, so testing the raw string reports
        // "mid-line" for a chunk that plainly finished one.
        self.at_line_start = darkwire_tui::strip_ansi(text).ends_with('\n');
    }

    /// Closes the open line, if one is open.
    fn end_line(&mut self) {
        if !self.at_line_start {
            self.transcript.write("\n");
            self.at_line_start = true;
        }
    }

    /// One complete line, with a break in front when the stream owes one.
    fn write_line(&mut self, text: &str) {
        self.end_line();
        self.transcript.write(&format!("{text}\n"));
        self.at_line_start = true;
    }

    /// Replaces the plan shown above the input.
    ///
    /// Whole, because that is what the `todo` tool does: there is no add and no
    /// complete, so the list that arrived is the list.
    pub fn set_tasks(&mut self, tasks: &[(TaskStatus, String)]) {
        self.tasks = tasks.to_vec();
    }

    /// How long the open run of reasoning has been going, in milliseconds.
    ///
    /// Counted from spinner ticks rather than a clock, which is not a style
    /// choice: `SystemTime::now` is denied in `clippy.toml`, because a renderer
    /// reading the wall clock is one a test cannot hold still.
    fn reasoning_elapsed(&self) -> f64 {
        let ticks = self.reasoning_since.unwrap_or(0).max(0);
        #[allow(
            clippy::cast_precision_loss,
            reason = "a tick count large enough to lose precision is a turn lasting weeks"
        )]
        let elapsed = (ticks as f64) * (SPINNER_INTERVAL_MS as f64);
        elapsed
    }

    /// The row a folded run of reasoning shows.
    ///
    /// The figure is appended rather than interpolated into a sentence, so the
    /// translation is a word and the duration is a duration. A template with a
    /// count in it would need plural rules to say "1 second" in the languages
    /// that have them, for a row that is read at a glance.
    fn reasoning_summary(&self, done: bool) -> String {
        let elapsed = format_duration(self.reasoning_elapsed());
        let (mark, word) = if done {
            ("┄", &self.labels.thought)
        } else {
            (
                spinner_frame(self.thinking.unwrap_or(0)),
                &self.labels.thinking,
            )
        };
        self.theme.dim.apply(&format!("{mark} {word} {elapsed}"))
    }

    /// The commands the list offers.
    pub fn set_commands(&mut self, commands: Vec<SelectItem<CommandChoice>>) {
        self.commands = commands;
    }

    /// Moves the cursor in the open command list.
    fn move_popup(&mut self, delta: i64) {
        if let Some(popup) = self.popup.as_mut() {
            popup.move_by(delta);
        }
    }

    /// Opens, filters or closes the command list for what is on the line.
    ///
    /// A slash command is a single token, so the list is open exactly while the
    /// line is one: from the `/` until the space that ends it. After that the
    /// operator is typing arguments, and a list of commands is no longer an
    /// answer to anything.
    fn sync_popup(&mut self, rows: usize) {
        let text = self.editor.text();
        if !text.starts_with('/') || text.contains(' ') {
            self.popup = None;
            return;
        }
        if let Some(popup) = self.popup.as_mut() {
            popup.set_rows(rows);
            popup.set_filter(text);
            return;
        }
        let mut popup = SelectList::new(self.commands.clone(), Some(rows), None);
        popup.set_filter(text);
        self.popup = Some(popup);
    }

    /// Takes the highlighted row, if the list is open and has one.
    ///
    /// A row that needs an argument lands on the line with the cursor after it.
    /// A row that needs nothing is submitted when `run` says so, because there
    /// is nothing left for the operator to say. Only then, though: Tab completes
    /// and Return runs, and that distinction is older than this program.
    fn accept_popup(&mut self, run: bool) -> Typed {
        let Some(choice) = self
            .popup
            .as_ref()
            .and_then(SelectList::selected)
            .map(|item| item.value.clone())
        else {
            return Typed::Redraw;
        };
        self.popup = None;
        if run && choice.submit {
            self.editor.set_text("");
            self.editor.remember(&choice.command);
            return Typed::Line(choice.command);
        }
        self.editor.set_text(&format!("{} ", choice.command));
        Typed::Redraw
    }

    /// How runs of each kind arrive from here on.
    pub fn set_folds(&mut self, folds: FoldDefaults) {
        self.folds = folds;
    }

    /// A turn has started: nothing has been said yet, so the spinner stands in.
    pub fn start_turn(&mut self) {
        self.thinking = Some(0);
    }

    /// The turn is over, or has started answering.
    pub fn end_turn(&mut self) {
        self.thinking = None;
        self.reasoning_since = None;
    }

    /// Whether the frame is still standing in for an answer that has not begun.
    #[must_use]
    pub fn is_waiting(&self) -> bool {
        self.thinking.is_some()
    }

    /// Advances everything that moves on its own. Says whether anything did.
    ///
    /// The spinner, and the figure on an open run of reasoning. The second is
    /// what keeps a folded run from reading as a terminal that has stopped:
    /// there is nothing else on screen while the model thinks, and a row that
    /// never changes is indistinguishable from one nobody is writing.
    pub fn tick(&mut self) -> bool {
        let mut moved = false;
        if let Some(tick) = self.thinking {
            self.thinking = Some(tick + 1);
            moved = true;
        }
        if let Some(ticks) = self.reasoning_since {
            self.reasoning_since = Some(ticks + 1);
            let summary = self.reasoning_summary(false);
            self.transcript.set_summary(&summary);
            moved = true;
        }
        moved
    }

    /// Folds or unfolds one kind of run, now and from now on.
    ///
    /// Returns what the new default is, so the caller can say which way it
    /// went. Both halves matter: the runs on screen follow at once, and the
    /// runs that have already gone to the scrollback cannot, so the default is
    /// what makes the key mean anything on the next turn.
    pub fn toggle_fold(&mut self, tag: &'static str) -> Option<bool> {
        let collapsed = if tag == REASONING_FOLD {
            // Nothing to fold. The reasoning never reached this frame, so the
            // honest answer is to say where the switch is rather than to do
            // nothing and let the key look broken.
            if self.folds.reasoning == ReasoningDisplay::Hidden {
                let note = self.theme.dim.apply(&self.labels.reasoning_off);
                self.transcript.write(&format!("\n{note}\n"));
                return None;
            }
            self.folds.reasoning = if self.folds.reasoning == ReasoningDisplay::Collapsed {
                ReasoningDisplay::Expanded
            } else {
                ReasoningDisplay::Collapsed
            };
            self.folds.reasoning == ReasoningDisplay::Collapsed
        } else if tag == STATS_FOLD {
            self.folds.stats = !self.folds.stats;
            // The other owner of this switch is the renderer, which decides
            // whether a pipe ever sees the row. `/output stats` reaches the
            // frame the other way, through `StatsShown`.
            self.stats_toggled = Some(!self.folds.stats);
            self.folds.stats
        } else {
            self.folds.tools = !self.folds.tools;
            self.folds.tools
        };
        self.transcript.set_collapsed(tag, collapsed);
        Some(collapsed)
    }

    /// Whether the row saying what a turn cost is showing.
    #[must_use]
    pub fn stats_shown(&self) -> bool {
        !self.folds.stats
    }

    /// What a key last did to that row, answered once.
    ///
    /// One switch with two owners. The frame owns it while a key is pressed,
    /// because only the frame can fold what is already drawn; the renderer owns
    /// it while `/output` is typed, because only the renderer decides whether a
    /// pipe sees the row at all. This is how the key's half reaches the other.
    #[must_use]
    pub fn take_stats_toggle(&mut self) -> Option<bool> {
        self.stats_toggled.take()
    }

    /// What is on the editor line right now.
    #[must_use]
    pub fn typing(&self) -> &str {
        self.editor.text()
    }

    /// The plan, as a window around whatever is in hand.
    ///
    /// Capped, because the question this answers is "where has this got to" and
    /// a ten-row list above the box you type into answers it worse than three
    /// rows do. The window is centred on the task in progress: what was just
    /// finished and what is next are the two things worth seeing beside it, and
    /// the rest is a count.
    fn task_rows(&self, width: usize) -> Vec<String> {
        if self.tasks.is_empty() {
            return Vec::new();
        }
        let doing = self
            .tasks
            .iter()
            .position(|(status, _)| *status == TaskStatus::Doing)
            .unwrap_or(0);
        let first = doing
            .saturating_sub(1)
            .min(self.tasks.len().saturating_sub(TASK_ROWS));
        let shown = self.tasks.iter().skip(first).take(TASK_ROWS);

        let mut rows: Vec<String> = shown
            .map(|(status, text)| {
                truncate_to_width(&task_row(&self.theme, *status, text), width, "…")
            })
            .collect();
        // Everything off the window, above it as well as below. Counting only
        // what follows would report "+1 more" for a plan with two finished
        // tasks scrolled off the top, which is a count of the wrong thing.
        let hidden = self.tasks.len().saturating_sub(rows.len());
        if hidden > 0 {
            rows.push(self.theme.dim.apply(&format!("  +{hidden} more")));
        }
        rows
    }

    /// Replaces what the bar at the bottom says.
    pub fn set_view(&mut self, view: HeaderView) {
        self.view = view;
    }

    /// How many rows the conversation itself takes.
    ///
    /// The other half of the frame's height, so a test can add the two up and
    /// compare the total against what was drawn.
    #[must_use]
    pub fn conversation_rows(&mut self, width: usize) -> usize {
        self.transcript.height(width)
    }

    /// How many rows everything below the conversation takes.
    ///
    /// Measured rather than counted, because the editor grows with what is
    /// typed into it and an overlay replaces the lot. Whatever is left of the
    /// window after this is what the conversation may keep on screen; the rest
    /// goes to the terminal's scrollback.
    ///
    /// It renders the chrome to measure it, and the frame renders it again a
    /// moment later. That is a real cost and a deliberately bounded one: the
    /// chrome is a couple of dozen rows whatever the session has said, which is
    /// the whole property this arrangement exists to buy. Counting instead would
    /// mean a second description of the layout, kept in step by hand.
    ///
    /// Public so a test can assert it against what [`Component::render`] drew.
    /// The two are one description of the layout written twice, and the bug
    /// they produce when they disagree is silent: a row of conversation goes to
    /// the scrollback that would have fitted on screen.
    pub fn chrome_rows(&mut self, width: usize) -> usize {
        // The gap above the frame, counted the same way `render` writes it: a
        // chrome measured one row too tall commits a row of conversation that
        // would have fitted. One row and no condition, which is the point of
        // the transcript not drawing the line a write left open.
        let mut rows = 1;
        if let Some(overlay) = self.overlay.as_mut() {
            return rows + overlay.render(width).len();
        }
        if self.thinking.is_some() {
            rows += 1;
        }
        if let Some(popup) = self.popup.as_ref() {
            rows += popup.render(width, &self.theme).len();
        }
        rows += self.task_rows(width).len();
        rows + 1
            + self.editor.render(width).len()
            + status_bar(&self.view, width, &self.theme).len()
    }
}

impl Component for Frame {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut rows = self.transcript.render(width);
        // One blank row between the conversation and the frame. Unconditional,
        // because the transcript no longer draws the line a write left open,
        // so its last row is text whether or not that write ended a line.
        rows.push(String::new());

        if let Some(overlay) = self.overlay.as_mut() {
            rows.extend(overlay.render(width));
            return rows;
        }

        if let Some(tick) = self.thinking {
            rows.push(self.theme.dim.apply(&format!(
                "{} {}",
                spinner_frame(tick),
                self.generating
            )));
        }
        if let Some(popup) = self.popup.as_ref() {
            rows.extend(popup.render(width, &self.theme));
        }
        rows.extend(self.task_rows(width));
        rows.push(input_rule(width, &self.theme));
        rows.extend(self.editor.render(width));
        rows.extend(status_bar(&self.view, width, &self.theme));
        rows
    }
}

/// What a keystroke asked the prompt to do.
///
/// Public because [`handle_key`] is the whole of the frame's input rule, and a
/// test that could not name its answers would be asserting the rule through a
/// real terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// A line was submitted.
    Line(String),
    /// Stop the running turn, or leave if none is.
    Interrupt,
    /// Leave.
    Leave,
    /// Open the palette.
    Palette,
    /// Complete the slash command being typed.
    Complete,
    /// Redraw and keep waiting.
    Redraw,
    /// Throw the screen away and draw it again.
    Reset,
    /// Fold or unfold the model's reasoning.
    FoldReasoning,
    /// Fold or unfold what tools printed.
    FoldTools,
    /// Fold or unfold the row saying what the turn cost.
    FoldStats,
}

/// The screen, the editor and the keyboard, shared with the menu.
///
/// Shared rather than owned, because a slash command holds the prompt's
/// renderer while it asks a question: a menu that borrowed the frame instead
/// would be a second mutable borrow of the thing already being written to.
struct FrameState {
    frame: Frame,
    renderer: Renderer<StandardOutput>,
    /// Keystrokes and window changes, in the order they happened.
    woke: mpsc::UnboundedReceiver<Wake>,
}

impl FrameState {
    /// How many rows an open menu may take.
    ///
    /// The window minus the frame's own chrome, so a menu never pushes the
    /// editor off the screen it is being typed into.
    fn menu_rows(&self) -> usize {
        DEFAULT_MAX_ROWS
            .min(self.renderer.rows().saturating_sub(CHROME_ROWS))
            .max(1)
    }

    /// Prints finished conversation, then draws what is left.
    ///
    /// Everything that can go, goes, which is what makes the frame a strip
    /// rather than a screen. What is left is the run still open, the line still
    /// being written, and the chrome. The conversation above belongs to the
    /// terminal from here on: it reflows on a resize for free, it can be
    /// selected and searched, and nothing redraws it.
    ///
    /// A run that is still open is held, because its fold is still the reader's
    /// to change and a block half in the history is a fold nobody can open. One
    /// too tall to sit above the composer loses the fold instead of the screen.
    fn draw(&mut self) {
        let width = self.renderer.columns();
        let mut committed = self.frame.transcript.take_committable(0, width);
        let room = self
            .renderer
            .rows()
            .saturating_sub(self.frame.chrome_rows(width));
        if self.frame.transcript.height(width) > room {
            committed.extend(self.frame.transcript.give_up_the_fold(room, width));
        }
        if committed.is_empty() {
            self.renderer.render(&mut self.frame);
        } else {
            self.renderer.print_above(&committed, &mut self.frame);
        }
    }

    /// Draws only if something asked for a frame since the last one.
    ///
    /// Through [`FrameState::draw`], so a frame that goes out on the tick
    /// commits what a frame drawn any other way would have.
    fn render_if_requested(&mut self) {
        if self.renderer.take_request() {
            self.draw();
        }
    }

    /// Draws the strip again, trusting nothing about where it was.
    ///
    /// What a resize needs, and what Ctrl-L is. The renderer notices a width
    /// change on its own, but a window that changed only in height, or one
    /// whose rows some other program scribbled on, looks identical to it.
    ///
    /// The conversation above is not reprinted and must not be. It was printed
    /// to the terminal, which is where it lives: the terminal rewraps it on a
    /// resize better than this could, and reprinting it is what used to put a
    /// second copy of the session in the history every time the window moved.
    fn redraw(&mut self) {
        self.renderer.invalidate();
        self.draw();
    }

    /// One thing the turn said, into the transcript.
    ///
    /// Where a signal becomes a fold. The renderer says *a reasoning run
    /// started*; this is what decides that a reasoning run is something the
    /// reader may put away, and that it starts put away. Reversing those two,
    /// with a renderer that emitted folds, would have handed a pipe and a log
    /// file a disclosure widget neither can draw.
    fn absorb(&mut self, event: &TranscriptEvent) {
        self.frame.absorb(event);
    }
}

/// A menu drawn into the frame, opened from wherever a command runs.
///
/// Takes the frame for as long as the menu is open and pumps keystrokes into
/// the overlay itself, which is what makes a picker a plain `await` at the
/// call site rather than a mode the loop has to know about.
struct FrameMenu {
    state: Arc<tokio::sync::Mutex<FrameState>>,
}

impl PickerMenu for FrameMenu {
    fn available(&self) -> bool {
        true
    }

    fn choose<'a>(
        &'a self,
        request: PickerRequest,
    ) -> Pin<Box<dyn Future<Output = Option<usize>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            let rows = state.menu_rows();
            state.frame.overlay = Some(Select::new(SelectOptions {
                items: request.items,
                labels: request.labels,
                theme: Some(state.frame.theme),
                index: request.index,
                max_rows: Some(rows),
            }));
            state.draw();
            let chosen = loop {
                let Some(woke) = state.woke.recv().await else {
                    break None;
                };
                let key = match woke {
                    Wake::Key(key) => key,
                    Wake::Resized => {
                        state.redraw();
                        continue;
                    }
                };
                let Some(overlay) = state.frame.overlay.as_mut() else {
                    break None;
                };
                match overlay.handle_key(&key) {
                    SelectOutcome::Open => state.draw(),
                    SelectOutcome::Chosen(at) => break Some(at),
                    SelectOutcome::Cancelled => break None,
                }
            };
            state.frame.overlay = None;
            state.draw();
            chosen
        })
    }
}

/// The frame, on a terminal.
///
/// ```text
///     …the conversation so far…
///     (blank)
///     ───────────────────────────────
///     › what is being typed
///     ───────────────────────────────
///     Default                 default
///     3.6%/66k          Ollama/qwen3
/// ```
///
/// Three things fall out of one renderer owning all of it, all of them
/// simplifications:
///
///  - **A menu is rows, not a mode.** It replaces the editor and the status
///    while it is open and the same renderer draws it, so there is no region
///    to open, no input to hand over and nothing to erase afterwards.
///  - **A turn changes nothing structural.** The editor stays where it is,
///    typing keeps working, and a message submitted while the answer streams
///    is queued for the moment it finishes.
///  - **An interrupt is a key.** Raw mode delivers `0x03` rather than raising
///    a signal, so one branch covers "stop this turn" and "leave" whether or
///    not a menu happens to be open.
pub struct FramedSurface {
    state: Arc<tokio::sync::Mutex<FrameState>>,
    menu: FrameMenu,
    sink: ChunkSink,
    chunks: mpsc::UnboundedReceiver<TranscriptEvent>,
    /// Lines typed while a turn was running, in the order they were submitted.
    queued: std::collections::VecDeque<String>,
    t: Translations,
    rows: Vec<crate::pickers::palette::PaletteRow>,
    /// Whether this process is the one that put the terminal into raw mode.
    ///
    /// Only what we took is given back. A prompt opened inside something that
    /// was already in raw mode must not hand that terminal's line discipline
    /// to whoever comes next.
    owns_raw: bool,
}

impl std::fmt::Debug for FramedSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FramedSurface")
    }
}

impl FramedSurface {
    /// Takes the terminal and draws the banner.
    pub fn open(session: &ChatSession) -> FramedSurface {
        let mut renderer = Renderer::new(
            StandardOutput,
            RendererOptions {
                take_screen_on_open: true,
                ..RendererOptions::default()
            },
        );
        let width = renderer.columns();
        let mut frame = Frame {
            transcript: Transcript::new(),
            editor: Editor::new(&session.theme),
            overlay: None,
            popup: None,
            commands: command_items(&crate::commands::palette_rows(&session.runtime), &session.t),
            thinking: None,
            theme: session.theme,
            generating: session.t.t(keys::chat::GENERATING),
            view: session.view(),
            folds: FoldDefaults::from(&session.runtime.config().ui),
            reasoning_since: None,
            at_line_start: true,
            stats_toggled: None,
            tasks: Vec::new(),
            labels: FoldLabels::from(&session.t),
        };
        // A resumed session has a plan already. Reading it here rather than
        // waiting for the agent to rewrite one means the first thing on screen
        // is where the work had got to, not a blank above the composer.
        if let Ok(stored) = session
            .runtime
            .store()
            .tasks(&session.attachment.session_key)
        {
            frame.set_tasks(
                &stored
                    .iter()
                    .map(|task| (task.status, task.text.clone()))
                    .collect::<Vec<_>>(),
            );
        }
        frame.transcript.write(&startup_header(
            &session.view(),
            width,
            &session.theme,
            &session.t,
            true,
        ));
        renderer.render(&mut frame);

        // Asked before the reader starts, because from then on the thread is
        // blocked inside a device read and cannot answer anything.
        let owns_raw =
            StandardInput.is_tty() && StandardInput.supports_raw_mode() && !StandardInput.is_raw();
        let state = Arc::new(tokio::sync::Mutex::new(FrameState {
            frame,
            renderer,
            woke: spawn_events(),
        }));
        let (sink, chunks) = chunks();
        FramedSurface {
            menu: FrameMenu {
                state: Arc::clone(&state),
            },
            state,
            sink,
            chunks,
            queued: std::collections::VecDeque::new(),
            owns_raw,
            t: Translations::new(session.t.locale()),
            rows: crate::commands::palette_rows(&session.runtime),
        }
    }
}

impl Surface for FramedSurface {
    fn menu(&self) -> &dyn PickerMenu {
        &self.menu
    }

    fn take_stats_shown(&mut self) -> BoxFut<'_, Option<bool>> {
        Box::pin(async move { self.state.lock().await.frame.take_stats_toggle() })
    }

    fn sink(&self) -> ChunkSink {
        self.sink.clone()
    }

    fn next_line(&mut self) -> BoxFut<'_, Option<String>> {
        Box::pin(async move {
            loop {
                // Before the keyboard, every time: a line submitted while the
                // last turn ran, and one the palette submitted on the
                // operator's behalf, are both already waiting here.
                if let Some(held) = self.queued.pop_front() {
                    return Some(held);
                }
                let woke = {
                    let mut state = self.state.lock().await;
                    state.woke.recv().await?
                };
                let key = match woke {
                    Wake::Key(key) => key,
                    Wake::Resized => {
                        let mut state = self.state.lock().await;
                        state.redraw();
                        continue;
                    }
                };
                let typed = {
                    let mut state = self.state.lock().await;
                    let rows = state.menu_rows();
                    let typed = handle_key_with(&mut state.frame, &key, rows);
                    match typed {
                        Typed::Reset => state.redraw(),
                        _ => state.draw(),
                    }
                    typed
                };
                match typed {
                    Typed::Line(line) => return Some(line),
                    // At an idle prompt an interrupt means "leave", which is
                    // what the shell's own would have meant.
                    Typed::Interrupt | Typed::Leave => return None,
                    Typed::Palette => self.open_palette().await,
                    Typed::Complete => self.complete().await,
                    Typed::Redraw
                    | Typed::Reset
                    | Typed::FoldReasoning
                    | Typed::FoldTools
                    | Typed::FoldStats => {}
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
            {
                let mut state = self.state.lock().await;
                state.frame.start_turn();
                // Nothing to point at until the answer starts: the caret would
                // otherwise sit on the blank row beside the spinner and read
                // as a stray block.
                state.renderer.set_cursor_visible(false);
                state.draw();
            }
            let outcome = self.pump(token, body).await;
            {
                let mut state = self.state.lock().await;
                state.frame.end_turn();
                state.renderer.set_cursor_visible(true);
                state.draw();
            }
            outcome
        })
    }

    fn echo<'a>(&'a mut self, content: &'a str) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            state.frame.transcript.write(&format!("\n› {content}\n"));
            state.draw();
        })
    }

    fn refresh<'a>(&'a mut self, view: &'a HeaderView) -> BoxFut<'a, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            while let Ok(event) = self.chunks.try_recv() {
                state.absorb(&event);
            }
            state.frame.set_view(view.clone());
            state.draw();
        })
    }

    fn close(&mut self) -> BoxFut<'_, ()> {
        Box::pin(async move {
            {
                let mut state = self.state.lock().await;
                state.renderer.stop();
            }
            // Not through the keyboard: that thread is parked inside a
            // blocking device read and will not come back until the terminal
            // sends something. The mode is a property of the tty rather than
            // of the reader, so it is set from here — otherwise `darkwire chat`
            // returns the operator to a shell with no echo and no line
            // editing, which looks like a hung terminal.
            if self.owns_raw {
                let _ = StandardInput.set_raw_mode(false);
            }
        })
    }
}

impl FramedSurface {
    /// Drives the turn while the frame keeps drawing.
    ///
    /// The five arms are the whole of what a running turn has to stay
    /// responsive to: the answer arriving, the frame going out, the spinner
    /// advancing, the keyboard, and the turn finishing. Anything typed meanwhile
    /// is queued rather than dropped, which is what lets somebody write the next
    /// question while the current answer is still streaming.
    ///
    /// **Text arriving does not draw.** It asks for a frame, and the frame tick
    /// draws at most one. A turn streams a token at a time and a fast provider
    /// sends hundreds a second; drawing each one is a frame per word, and the
    /// operator cannot read at 200fps anyway. What they see is identical and
    /// the machine does a fraction of the work, which on slow hardware is the
    /// difference between a prompt that types and one that stutters.
    ///
    /// The select is `biased` and the chunk arm is first, so a provider fast
    /// enough to keep it permanently ready would starve every arm below it and
    /// the screen would freeze for the length of the answer. Hence
    /// [`CHUNKS_PER_FRAME`]: once that many have been taken without a frame
    /// going out, the arm disables itself until the tick comes round, which is
    /// what lets the tick come round.
    async fn pump(
        &mut self,
        token: &CancellationToken,
        body: BoxFut<'_, Result<TurnOutcome>>,
    ) -> Result<TurnOutcome> {
        tokio::pin!(body);
        let mut frames = tokio::time::interval(std::time::Duration::from_millis(FRAME_INTERVAL_MS));
        let mut spins =
            tokio::time::interval(std::time::Duration::from_millis(SPINNER_INTERVAL_MS));
        let mut since_frame = 0usize;
        loop {
            let mut state = self.state.lock().await;
            let outcome = tokio::select! {
                biased;
                Some(event) = self.chunks.recv(), if since_frame < CHUNKS_PER_FRAME => {
                    state.absorb(&event);
                    state.renderer.request_render();
                    since_frame += 1;
                    None
                }
                _ = frames.tick() => {
                    since_frame = 0;
                    state.render_if_requested();
                    None
                }
                Some(woke) = state.woke.recv() => {
                    let Wake::Key(key) = woke else {
                        state.redraw();
                        continue;
                    };
                    let rows = state.menu_rows();
                    let typed = handle_key_with(&mut state.frame, &key, rows);
                    let reset = typed == Typed::Reset;
                    match typed {
                        // While a turn runs the interrupt belongs to the turn.
                        Typed::Interrupt => token.cancel(),
                        Typed::Line(line) => self.queued.push_back(line),
                        // Leaving is refused mid-turn: the answer is still
                        // being written into the transcript this would tear
                        // down. A second interrupt stops the turn first.
                        Typed::Leave
                        | Typed::Palette
                        | Typed::Complete
                        | Typed::Redraw
                        | Typed::Reset
                        | Typed::FoldReasoning
                        | Typed::FoldTools
                        | Typed::FoldStats => {}
                    }
                    if reset {
                        state.redraw();
                    } else {
                        state.draw();
                    }
                    None
                }
                _ = spins.tick() => {
                    if state.frame.tick() {
                        state.renderer.request_render();
                    }
                    None
                }
                done = &mut body => Some(done),
            };
            if let Some(done) = outcome {
                while let Ok(event) = self.chunks.try_recv() {
                    state.absorb(&event);
                }
                state.draw();
                return done;
            }
        }
    }

    /// Tab: the one candidate that completes what is typed, or nothing.
    ///
    /// A command is a token with a known vocabulary — the table `/help`
    /// prints — rather than a guess at a word, so an ambiguous prefix leaves
    /// the line alone instead of choosing for the operator.
    async fn complete(&mut self) {
        let mut state = self.state.lock().await;
        let (matches, _) = complete_command(state.frame.editor.text(), &self.rows);
        if let [only] = matches.as_slice() {
            state.frame.editor.set_text(&format!("{only} "));
        }
        state.draw();
    }

    /// Ctrl-G: every command as one searchable list.
    ///
    /// A row that needs an argument lands in the editor with the cursor after
    /// it; a row that needs nothing is submitted outright, because there is
    /// nothing left for the operator to say.
    async fn open_palette(&mut self) {
        let chosen = pick_command(&self.menu, &self.rows, &self.t).await;
        let Some(choice) = chosen else {
            return;
        };
        if choice.submit {
            self.queued.push_back(choice.command);
            return;
        }
        let mut state = self.state.lock().await;
        state.frame.editor.set_text(&format!("{} ", choice.command));
        state.draw();
    }
}

/// Something the frame has to react to.
///
/// One channel rather than two, because both loops below wait on it inside a
/// `select!` and two receivers on the same state cannot both be borrowed there.
/// Keeping them separate *variants* is the part that matters: a resize must not
/// reach the editor, and a synthetic keystroke would.
#[derive(Debug)]
enum Wake {
    Key(Key),
    Resized,
}

/// Keystrokes and window changes, on one channel.
///
/// A resize is a signal rather than anything on the input stream: the keyboard
/// thread reads raw bytes and decodes them itself, so it never sees the
/// crossterm event that would have carried one. Without this the frame is drawn
/// at the new width only when something *else* asks for a draw, which at an idle
/// prompt is the next keystroke. The window is resized, the terminal reflows
/// rows this program will not rewrite, and the artifacts sit there until
/// somebody types.
fn spawn_events() -> mpsc::UnboundedReceiver<Wake> {
    let (tx, rx) = mpsc::unbounded_channel();
    spawn_keyboard(tx.clone());
    // Both the subscription and the task it runs on need a reactor, and this
    // function is reachable from a plain `fn`. Asking rather than assuming, so
    // the worst a caller outside a runtime gets is a prompt that does not
    // notice resizes, rather than a panic on launch.
    #[cfg(unix)]
    if tokio::runtime::Handle::try_current().is_ok() {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut winch) = signal(SignalKind::window_change()) {
            tokio::spawn(async move {
                while winch.recv().await.is_some() {
                    if tx.send(Wake::Resized).is_err() {
                        return;
                    }
                }
            });
            return rx;
        }
    }
    // No such signal, or it could not be handled: the keyboard is the only
    // producer and this sender has nothing left to do.
    drop(tx);
    rx
}

/// Reads keystrokes on a thread and delivers them to the prompt.
///
/// A thread rather than an async read, because terminal input is a blocking
/// device read with no portable poll — and the prompt has to stay responsive
/// to a turn's events while it waits. The channel is what joins the two.
fn spawn_keyboard(tx: mpsc::UnboundedSender<Wake>) {
    std::thread::spawn(move || {
        let Ok(mut keyboard) = open_keyboard(StandardInput, None) else {
            return;
        };
        loop {
            match keyboard.read_keys() {
                Ok(keys) if keys.is_empty() => break,
                Ok(keys) => {
                    for key in keys {
                        if tx.send(Wake::Key(key)).is_err() {
                            let _ = keyboard.stop();
                            return;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let _ = keyboard.stop();
    });
}

/// One keystroke against the frame.
///
/// Public because it is the whole of the frame's input rule, and a test that
/// could not reach it would be asserting the rule through a real terminal.
pub fn handle_key(frame: &mut Frame, key: &Key) -> Typed {
    handle_key_with(frame, key, DEFAULT_MAX_ROWS)
}

/// The same, told how many rows a list may take.
///
/// Split out because the window's height is the renderer's to know and the
/// input rule is the frame's, and a test asserting what a keystroke means
/// should not have to invent a terminal to ask.
pub fn handle_key_with(frame: &mut Frame, key: &Key, rows: usize) -> Typed {
    if let Some(overlay) = frame.overlay.as_mut() {
        // An open menu owns the keyboard; the pump that opened it is what
        // reads the answer.
        let _ = overlay.handle_key(key);
        return Typed::Redraw;
    }

    // The command list, while one is being typed. It takes the four keys a list
    // needs and nothing else, so every other key still reaches the editor and
    // the line stays editable underneath it.
    if frame.popup.is_some() {
        match key.name {
            KeyName::Up => {
                frame.move_popup(-1);
                return Typed::Redraw;
            }
            KeyName::Down => {
                frame.move_popup(1);
                return Typed::Redraw;
            }
            KeyName::Escape => {
                frame.popup = None;
                return Typed::Redraw;
            }
            // Both take the row under the cursor, and they differ in what
            // happens next. Tab completes: the command lands on the line and
            // the operator carries on. Return runs it. That is what the two
            // keys mean everywhere else, and a Tab that submitted would run a
            // command nobody had finished looking at.
            KeyName::Tab => return frame.accept_popup(false),
            KeyName::Enter => return frame.accept_popup(true),
            _ => {}
        }
    }

    // Ctrl-G is the palette: the list of every command at once, searchable,
    // for when you do not know the name to start typing.
    if is_ctrl(key, 'g') {
        return Typed::Palette;
    }

    // The two folds. Both flip what is on screen *and* how the next run
    // arrives, because a run that has gone to the scrollback cannot be
    // rewritten and a key that only reached the screen would stop working the
    // moment the screen filled.
    if is_ctrl(key, 't') {
        frame.toggle_fold(REASONING_FOLD);
        return Typed::FoldReasoning;
    }
    if is_ctrl(key, 'o') {
        frame.toggle_fold(TOOL_FOLD);
        return Typed::FoldTools;
    }

    // The third fold, and the one that starts hidden. Ctrl-Y is free here:
    // there is no kill ring for it to yank from, and readline's meaning would
    // have nothing to paste.
    if is_ctrl(key, 'y') {
        frame.toggle_fold(STATS_FOLD);
        return Typed::FoldStats;
    }

    // Ctrl-L is what it is in every shell: the screen is wrong, draw it again.
    // Worth having even with the resize signal wired up, because a program
    // writing to the same terminal from somewhere else leaves damage no signal
    // announces.
    if is_ctrl(key, 'l') {
        return Typed::Reset;
    }

    // Tab completes a slash command and nothing else. The rest of a prompt is
    // prose, and a completer guessing at the middle of a sentence would
    // surprise far more often than it helped.
    if key.name == KeyName::Tab {
        return Typed::Complete;
    }

    let outcome = frame.editor.handle_key(key);
    frame.sync_popup(rows);
    match outcome {
        EditorOutcome::Submit(text) => {
            let line = text.trim().to_owned();
            if line.is_empty() {
                return Typed::Redraw;
            }
            frame.editor.remember(&line);
            Typed::Line(line)
        }
        EditorOutcome::Interrupt => Typed::Interrupt,
        EditorOutcome::Eof => Typed::Leave,
        EditorOutcome::None => Typed::Redraw,
    }
}
