//! Slash commands: everything the web UI grew, at a prompt.
//!
//! The terminal and the browser share one SQLite file, so a conversation
//! started here is the same row the sidebar lists. What was missing was not
//! plumbing — it was a way to *say* any of it from a terminal, and the
//! primitives are the same ones the routes call. Nothing here reimplements
//! truncation, forking or context measurement; it names them.
//!
//! Three shapes carry the design.
//!
//! **[`SlashOutcome`] rather than a boolean.** The dispatcher this replaced
//! answered "should the prompt exit", which was enough when the only commands
//! were `/clear` and `/help`. `/new`, `/session <key>` and `/branch` all change
//! *which conversation the prompt is attached to*, and `/edit` and
//! `/regenerate` end by wanting a turn run. Both are answers a boolean cannot
//! express.
//!
//! **`/edit` and `/regenerate` hand content back rather than running it.** They
//! truncate, then answer [`SlashOutcome::Turn`], and the prompt runs it through
//! the same path a typed message takes. One turn-running code path in the
//! terminal — exactly as the hub has one — so a re-run inherits the renderer,
//! the cancellation and the exit codes rather than a second copy of them.
//!
//! **Nothing here fails its caller.** [`run_slash_command`] answers an outcome
//! and never an error: a refusal from a primitive is rendered as a warning and
//! the prompt comes back. A prompt that exited on a mistyped session key would
//! be worse than the mistyped key. The helpers below return [`Result`] and the
//! one public entry point is where that stops.
//!
//! **Copy is deliberately absent.** A terminal has no portable clipboard, and
//! shelling out to `pbcopy`/`xclip` would put platform detection and a child
//! process into a layer that has neither. A terminal's selection *is* its copy
//! mechanism.
//!
//! ## Which menu trait this consumes
//!
//! [`crate::pickers::PickerMenu`], not [`crate::menu::Menu`]. The pickers are
//! written against it, its `choose` is asynchronous — a menu opens *inside* a
//! running frame and answers keystrokes later — and it takes `&self`, so a
//! command can open one while still holding the renderer. [`SyncMenu`] adapts
//! the other direction for a frame owner that has a [`crate::menu::Menu`]
//! already.

use std::sync::Mutex;

use darkwire_agent::skills::{SKILLS_DIRNAME, read_skills};
use darkwire_agent::{PromptPreviewInput, describe_context};
use darkwire_core::memory::{MEMORY_DIRNAME, read_memories};
use darkwire_core::session_store::{
    CreateSession, ForkSession, ListSessions, ReadMessages, UpdateSession,
};
use darkwire_core::workspace_store::CreateWorkspace;
use darkwire_core::{Clock, ErrorKind, Result, SessionStore, SystemClock, WireError, text_of};
use darkwire_i18n::{args, format_number, keys};
use darkwire_protocol::config::{AgentSettingsChange, agent_settings_patch};
use darkwire_protocol::{
    DEFAULT_AGENT_ID, DEFAULT_WORKSPACE_ID, ModelsResponse, ReasoningEffort, ToolPermission,
    new_uuid,
};
use darkwire_providers::estimate_tokens;
use darkwire_security::random::{OsRandom, RandomSource};
use darkwire_server::agent_for_turn;
use darkwire_tui::{pad_to_width, visible_width};
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

use crate::i18n::Translations;
use crate::menu::Menu;
use crate::messages::{DEFAULT_MESSAGE_LINES, recent_messages, resolve_seq};
use crate::models::ModelCatalogue;
use crate::pickers::agents::{agent_listing, pick_agent};
use crate::pickers::effort::{DEFAULT_LEVEL, LEVELS, effort_listing, effort_value, pick_effort};
use crate::pickers::models::{model_errors, model_listing, pick_model};
use crate::pickers::palette::PaletteRow;
use crate::pickers::sessions::pick_session;
use crate::pickers::workspaces::pick_workspace;
use crate::pickers::{MenuRequest, PickerMenu};
use crate::render::{TurnRenderer, clip};
use crate::runtime::{ChatRuntime, save_settings, settings_of};

/// How much of a message `/messages` shows on its one line.
const MESSAGE_CLIP_CHARS: usize = 72;

/// How many sessions `/sessions` lists when no count is given.
const DEFAULT_SESSION_LINES: usize = 20;

/// How many turns `/stats` lists when no count is given.
const DEFAULT_STATS_LINES: usize = 10;

/// The width of the `seq` column in `/messages`.
const SEQ_COLUMN: usize = 4;

/// The label column in `/context`.
const CONTEXT_LABEL_COLUMN: usize = 10;

/// What a slash command asks the prompt to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashOutcome {
    /// Draw the prompt again.
    Continue,
    /// Leave.
    Exit,
    /// Attach the prompt to another conversation.
    Attach(String),
    /// Run this content as a turn, through the prompt's own path.
    ///
    /// Always text: both producers — `/edit` and `/regenerate` — hand back
    /// words somebody wrote, and a re-run of an attachment is the attachment's
    /// own message being replayed rather than rebuilt here.
    Turn(String),
}

/// What the slash commands need of a model catalogue.
///
/// One method, because `/model` asks one question. The catalogue also probes a
/// single endpoint and drops its cache, and neither is any of this module's
/// business — stating the whole of it here would make a prompt command depend
/// on how a model list is fetched.
pub trait SlashModels: Send + Sync {
    /// Every reachable model, and every endpoint that did not answer.
    fn list(&self) -> BoxFuture<'_, Result<ModelsResponse>>;
}

impl SlashModels for ModelCatalogue {
    fn list(&self) -> BoxFuture<'_, Result<ModelsResponse>> {
        // Never `refresh`: a prompt command asking twice in a minute is somebody
        // reading the list, not somebody who has just pulled a model.
        Box::pin(ModelCatalogue::list(self, false))
    }
}

/// Everything a slash command is allowed to reach.
///
/// The collaborators arrive here rather than being looked up, which is what
/// lets the whole surface be driven with a temporary home, a recording
/// renderer and a menu that answers without drawing.
pub struct SlashContext<'a> {
    /// Where a command writes.
    pub renderer: &'a mut TurnRenderer,
    /// The composition root.
    pub runtime: &'a ChatRuntime,
    /// The terminal's translations, which also carry the locale.
    pub t: &'a Translations,
    /// The conversation the prompt is attached to right now.
    pub session_key: &'a str,
    /// Where a *new* conversation lands. `None` means the default.
    ///
    /// Written through rather than reported back: the prompt owns this state
    /// and a command moving it is the command's whole effect, so a callback
    /// would be a second way to spell one assignment.
    pub workspace_id: &'a mut Option<String>,
    /// Which agent a *new* conversation runs on. `None` is the default.
    pub agent_id: &'a mut Option<String>,
    /// How a command asks a question with arrow keys.
    ///
    /// Unavailable on a pipe, under `--json` and on a dumb terminal, so a
    /// command that wants to offer a picker asks whether one is available
    /// rather than working out for itself whether this is a terminal.
    pub menu: &'a dyn PickerMenu,
    /// What models the configured endpoints answered with, when asked.
    pub models: &'a dyn SlashModels,
    /// Whether `--model` pinned the model for this process.
    ///
    /// That flag is a statement about *this process* that the config cannot
    /// move, so `/model` would appear to work and change nothing. It refuses
    /// instead, naming the flag.
    pub model_pinned: bool,
}

impl std::fmt::Debug for SlashContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlashContext")
            .field("session_key", &self.session_key)
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("model_pinned", &self.model_pinned)
            .finish_non_exhaustive()
    }
}

// The table

/// One row of `/help`: what you type, and what it does.
///
/// The two halves are kept apart because only one of them is language.
/// `/workspace move <from> <to>` is *syntax* — it is what the parser matches
/// on, so translating it would print a command that does not exist. The
/// description beside it is prose and belongs in the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRow {
    /// Typed by the operator, so never translated.
    pub syntax: String,
    /// Absent for a variant row that the row above it already described.
    pub key: Option<&'static str>,
}

impl CommandRow {
    /// A row with a description.
    fn new(syntax: &str, key: &'static str) -> CommandRow {
        CommandRow {
            syntax: syntax.to_owned(),
            key: Some(key),
        }
    }

    /// A variant row, which has no description of its own.
    fn variant(syntax: &str) -> CommandRow {
        CommandRow {
            syntax: syntax.to_owned(),
            key: None,
        }
    }
}

/// The palette's narrow view of a command.
///
/// The conversion lives here rather than in the palette so that the arrow
/// points one way: this module owns the table, and the palette is handed rows
/// it never reaches back for.
impl From<&CommandRow> for PaletteRow {
    fn from(row: &CommandRow) -> PaletteRow {
        PaletteRow {
            syntax: row.syntax.clone(),
            key: row.key,
        }
    }
}

/// One heading and the rows under it.
struct HelpSection {
    /// Absent for the opening rows, which sit above the first heading.
    heading: Option<&'static str>,
    rows: Vec<CommandRow>,
}

/// Every command, grouped as `/help` prints them.
fn help_layout() -> Vec<HelpSection> {
    vec![
        HelpSection {
            heading: None,
            rows: vec![
                CommandRow::new("/help", keys::slash::help::HELP),
                CommandRow::new("/messages [n]", keys::slash::help::MESSAGES),
                CommandRow::new("/clear", keys::slash::help::CLEAR),
                CommandRow::new("/exit, /quit", keys::slash::help::EXIT),
            ],
        },
        HelpSection {
            heading: Some(keys::slash::sections::SESSIONS),
            rows: vec![
                CommandRow::new("/sessions [n]", keys::slash::help::SESSIONS),
                CommandRow::new("/new [title]", keys::slash::help::NEW),
                CommandRow::new("/session [key]", keys::slash::help::SESSION),
                CommandRow::new("/rename <title>", keys::slash::help::RENAME),
                CommandRow::new("/delete [key]", keys::slash::help::DELETE),
                CommandRow::new("/branch [ref]", keys::slash::help::BRANCH),
            ],
        },
        HelpSection {
            heading: Some(keys::slash::sections::MESSAGES),
            rows: vec![
                CommandRow::new("/edit <ref> <text>", keys::slash::help::EDIT),
                CommandRow::new("/regenerate [ref]", keys::slash::help::REGENERATE),
            ],
        },
        HelpSection {
            heading: Some(keys::slash::sections::CONTEXT),
            rows: vec![
                CommandRow::new("/context", keys::slash::help::CONTEXT),
                CommandRow::new("/stats [n]", keys::slash::help::STATS),
            ],
        },
        HelpSection {
            heading: Some(keys::slash::sections::OUTPUT),
            rows: vec![
                CommandRow::new("/output", keys::slash::help::OUTPUT),
                CommandRow::new("/output <field> [on|off]", keys::slash::help::OUTPUT_SET),
            ],
        },
        HelpSection {
            heading: Some(keys::slash::sections::AGENTS),
            rows: vec![
                CommandRow::new("/agent [id]", keys::slash::help::AGENT),
                CommandRow::new("/model [id]", keys::slash::help::MODEL),
                CommandRow::new("/effort [level]", keys::slash::help::EFFORT),
                CommandRow::new("/temperature [n]", keys::slash::help::TEMPERATURE),
            ],
        },
        HelpSection {
            heading: Some(keys::slash::sections::MEMORY),
            rows: vec![
                CommandRow::new("/memory", keys::slash::help::MEMORY),
                CommandRow::new("/memory on|off", keys::slash::help::MEMORY_ON_OFF),
                CommandRow::new("/skills", keys::slash::help::SKILLS),
            ],
        },
        HelpSection {
            heading: Some(keys::slash::sections::WORKSPACES),
            rows: vec![
                CommandRow::new("/workspaces", keys::slash::help::WORKSPACES),
                CommandRow::new("/workspace <id>", keys::slash::help::WORKSPACE),
                CommandRow::variant("/workspace new <name>"),
                CommandRow::variant("/workspace rename <id> <name>"),
                CommandRow::new("/workspace rm <id>", keys::slash::help::WORKSPACE_RM),
                CommandRow::new(
                    "/workspace move <from> <to>",
                    keys::slash::help::WORKSPACE_MOVE,
                ),
            ],
        },
    ]
}

/// Every command, flattened out of the sections `/help` groups them into.
///
/// Public so the palette and the Tab completer read the same table this page
/// does. One table means a command cannot exist in one and not the other,
/// which is the failure a second list beside this one would eventually
/// produce.
#[must_use]
pub fn command_rows() -> Vec<CommandRow> {
    help_layout()
        .into_iter()
        .flat_map(|section| section.rows)
        .collect()
}

/// The rows above, plus whatever extensions contribute right now.
///
/// A function of the runtime rather than a constant, because the answer
/// changes while the prompt is open: approving an extension in a browser adds
/// a command to a terminal that is already running. The Tab completer and the
/// palette call this; [`help_text`] does not, because `/help` is laid out in
/// sections and an extension's command belongs to no section this file wrote.
#[must_use]
pub fn command_rows_for(runtime: &ChatRuntime) -> Vec<CommandRow> {
    let mut rows = command_rows();
    if let Some(host) = runtime.extensions() {
        rows.extend(
            host.commands()
                .into_iter()
                .map(|command| CommandRow::variant(&format!("/{}", command.id))),
        );
    }
    rows
}

/// The same rows, as the palette wants them.
#[must_use]
pub fn palette_rows(runtime: &ChatRuntime) -> Vec<PaletteRow> {
    command_rows_for(runtime)
        .iter()
        .map(PaletteRow::from)
        .collect()
}

/// The `/help` listing, measured rather than typed out.
///
/// The description column is derived from the longest syntax line rather than
/// being a run of hand-counted spaces. That held only while every description
/// was English — the first longer translation would have pushed its own row
/// out and left the others where they were.
///
/// The width is measured over the rows that *have* a description, so the two
/// variant rows that carry none cannot push the column right for everybody
/// else.
#[must_use]
pub fn help_text(t: &Translations) -> String {
    let layout = help_layout();
    let width = layout
        .iter()
        .flat_map(|section| &section.rows)
        .filter(|row| row.key.is_some())
        .map(|row| visible_width(&row.syntax))
        .max()
        .unwrap_or(0);

    let blocks: Vec<String> = layout
        .iter()
        .map(|section| {
            let rows: Vec<String> = section
                .rows
                .iter()
                .map(|row| match row.key {
                    None => format!("  {}", row.syntax),
                    Some(key) => {
                        format!("  {}  {}", pad_to_width(&row.syntax, width), t.t(key))
                    }
                })
                .collect();
            match section.heading {
                None => rows.join("\n"),
                Some(heading) => format!("  {}\n{}", t.t(heading), rows.join("\n")),
            }
        })
        .collect();

    format!(
        "{}\n\n  {}",
        blocks.join("\n\n"),
        t.t(keys::slash::REF_NOTE)
    )
}

// The dispatcher

/// Runs one slash command.
///
/// Never fails for anything a person typed: a refusal from a primitive is
/// rendered as a warning and the prompt comes back.
pub async fn run_slash_command(input: &str, ctx: &mut SlashContext<'_>) -> SlashOutcome {
    let trimmed = input.trim();
    let word = trimmed.split_whitespace().next().unwrap_or("");
    let name = word.strip_prefix('/').unwrap_or(word).to_owned();
    let argv: Vec<String> = trimmed
        .split_whitespace()
        .skip(1)
        .map(str::to_owned)
        .collect();
    let tail = trimmed.strip_prefix(word).unwrap_or("").trim().to_owned();

    match dispatch(&name, &argv, &tail, ctx).await {
        Ok(outcome) => outcome,
        Err(error) => {
            ctx.renderer.warn(&error.message);
            SlashOutcome::Continue
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one match arm per command; splitting it would hide the table of \
              verbs that is the point of the function"
)]
async fn dispatch(
    name: &str,
    argv: &[String],
    tail: &str,
    ctx: &mut SlashContext<'_>,
) -> Result<SlashOutcome> {
    match name {
        "exit" | "quit" => Ok(SlashOutcome::Exit),

        "help" => {
            let text = help_text(ctx.t);
            ctx.renderer.note(&text);
            Ok(SlashOutcome::Continue)
        }

        "clear" => {
            ctx.runtime.store().clear_messages(ctx.session_key)?;
            let note = ctx.t.t(keys::slash::notes::HISTORY_CLEARED);
            ctx.renderer.note(&note);
            Ok(SlashOutcome::Continue)
        }

        "messages" => messages_command(argv, ctx),

        // ── Sessions ──────────────────────────────────────────────
        "sessions" => sessions_command(argv, ctx).await,
        "new" => new_command(tail, ctx),
        "session" => session_command(argv, ctx),
        "rename" => rename_command(tail, ctx),
        "delete" => delete_command(argv, ctx),
        "branch" => branch_command(argv, ctx),

        // ── Messages ──────────────────────────────────────────────
        "edit" => edit_command(argv, tail, ctx),
        "regenerate" => regenerate_command(argv, ctx),

        // ── Context and cost ──────────────────────────────────────
        "context" => context_command(ctx).await,
        "memory" => memory_command(argv, ctx),
        "skills" => skills_command(ctx),
        "stats" => stats_command(argv, ctx),

        // ── Workspaces ────────────────────────────────────────────
        "workspaces" => workspaces_command(ctx),
        "workspace" => workspace_command(argv, ctx).await,

        // ── Agents ────────────────────────────────────────────────
        "agent" => agent_command(argv.first().map(String::as_str), ctx).await,
        "model" => model_command(argv.first().map(String::as_str), ctx).await,
        "effort" => {
            sample_command(SampleField::Effort, argv.first().map(String::as_str), ctx).await
        }
        "temperature" => {
            sample_command(
                SampleField::Temperature,
                argv.first().map(String::as_str),
                ctx,
            )
            .await
        }

        // ── What a turn shows ─────────────────────────────────────
        "output" => output_command(
            argv.first().map(String::as_str),
            argv.get(1).map(String::as_str),
            ctx,
        ),

        // Before the refusal, not instead of it: an extension's command reaches
        // the terminal through the *host* rather than through this table,
        // because there is one definition of it and three surfaces that have to
        // find it. The table above stays hand-written for the reason its header
        // gives — these are the commands this surface implements, and an
        // extension's is one it merely forwards.
        _ => extension_command(name, tail, ctx).await,
    }
}

// Messages and sessions

fn messages_command(argv: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let count = positive_count(argv.first()).unwrap_or(DEFAULT_MESSAGE_LINES);
    let rows = recent_messages(ctx.runtime.store(), ctx.session_key, count)?;
    if rows.is_empty() {
        let note = ctx.t.t(keys::slash::notes::NOTHING_SAID);
        ctx.renderer.note(&note);
        return Ok(SlashOutcome::Continue);
    }
    let text = rows
        .iter()
        .map(|row| {
            format!(
                "{:>SEQ_COLUMN$}  {}  {}",
                row.seq,
                row.role,
                clip(&row.text, MESSAGE_CLIP_CHARS)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    ctx.renderer.note(&text);
    Ok(SlashOutcome::Continue)
}

/// `/sessions` — a picker on a terminal, the listing on a pipe.
///
/// The same shape `/agent` and `/workspace` take. The argument is the page size
/// either way, so a script and a person are asking the same question of the
/// same rows.
async fn sessions_command(argv: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let limit = positive_count(argv.first()).unwrap_or(DEFAULT_SESSION_LINES);
    let store = ctx.runtime.store();
    let workspace_id = store
        .get_session(ctx.session_key)?
        .map(|session| session.workspace_id);
    let rows = store.list_sessions(&ListSessions {
        limit: Some(limit),
        workspace_id,
        ..ListSessions::default()
    })?;
    if rows.is_empty() {
        let note = ctx.t.t(keys::slash::notes::NO_SESSIONS);
        ctx.renderer.note(&note);
        return Ok(SlashOutcome::Continue);
    }

    if ctx.menu.available() {
        let target = pick_session(ctx.menu, &rows, ctx.session_key, ctx.t).await;
        if let Some(target) = target {
            store.ensure_session(
                &target,
                CreateSession {
                    origin: Some("cli".to_owned()),
                    ..CreateSession::default()
                },
            )?;
            return Ok(SlashOutcome::Attach(target));
        }
        return Ok(SlashOutcome::Continue);
    }

    let text = rows
        .iter()
        .map(|row| {
            let mark = if row.session.key == ctx.session_key {
                '*'
            } else {
                ' '
            };
            let title = if row.session.title.is_empty() {
                "(unnamed)"
            } else {
                &row.session.title
            };
            format!(
                "{mark} {title}  ·  {}  ·  {} messages",
                row.session.key, row.message_count
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    ctx.renderer.note(&text);
    Ok(SlashOutcome::Continue)
}

fn new_command(tail: &str, ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let key = format!("cli-{}", random_key());
    ctx.runtime.store().ensure_session(
        &key,
        CreateSession {
            origin: Some("cli".to_owned()),
            title: if tail.is_empty() {
                None
            } else {
                Some(tail.to_owned())
            },
            workspace_id: ctx.workspace_id.clone(),
            ..CreateSession::default()
        },
    )?;
    Ok(SlashOutcome::Attach(key))
}

fn session_command(argv: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let store = ctx.runtime.store();
    let Some(target) = argv.first() else {
        let session = store.get_session(ctx.session_key)?;
        let title = match session.as_ref() {
            Some(session) if !session.title.is_empty() => session.title.clone(),
            _ => "(unnamed)".to_owned(),
        };
        // The session's *own* workspace, not the pending one. They differ after
        // a `/workspace` switch, and showing the pending one here would report
        // where the next conversation lands as though it were where this one is.
        let workspace = session
            .as_ref()
            .map_or_else(|| "—".to_owned(), |session| session.workspace_id.clone());
        let count = store.message_count(ctx.session_key)?;
        let text = format!(
            "{title}\n  {}  ·  {count} messages  ·  workspace {workspace}",
            ctx.session_key
        );
        ctx.renderer.note(&text);
        return Ok(SlashOutcome::Continue);
    };

    store.ensure_session(
        target,
        CreateSession {
            origin: Some("cli".to_owned()),
            ..CreateSession::default()
        },
    )?;
    Ok(SlashOutcome::Attach(target.clone()))
}

fn rename_command(tail: &str, ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    if tail.is_empty() {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            ctx.t.t(keys::slash::errors::USAGE_RENAME),
        ));
    }
    ctx.runtime.store().update_session(
        ctx.session_key,
        UpdateSession {
            title: Some(tail.to_owned()),
            ..UpdateSession::default()
        },
    )?;
    let note = ctx
        .t
        .tr(keys::slash::notes::RENAMED_TO, args!["title" => tail]);
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

fn delete_command(argv: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let key = argv
        .first()
        .cloned()
        .unwrap_or_else(|| ctx.session_key.to_owned());
    if !ctx.runtime.store().delete_session(&key)? {
        return Err(WireError::new(
            ErrorKind::NotFound,
            ctx.t.tr(
                keys::slash::errors::NO_SESSION,
                args!["key" => key.as_str()],
            ),
        ));
    }
    let note = ctx
        .t
        .tr(keys::slash::notes::DELETED, args!["key" => key.as_str()]);
    ctx.renderer.note(&note);
    if key != ctx.session_key {
        return Ok(SlashOutcome::Continue);
    }
    // The one it was attached to is gone, so it needs somewhere to be.
    let next = format!("cli-{}", random_key());
    ctx.runtime.store().ensure_session(
        &next,
        CreateSession {
            origin: Some("cli".to_owned()),
            ..CreateSession::default()
        },
    )?;
    Ok(SlashOutcome::Attach(next))
}

fn branch_command(argv: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let store = ctx.runtime.store();
    let seq = resolve_seq(store, ctx.session_key, argv.first().map(String::as_str))?;
    let fork = store.fork_session(
        ctx.session_key,
        seq,
        ForkSession {
            origin: Some("cli".to_owned()),
            ..ForkSession::default()
        },
    )?;
    let text = format!(
        "branched at {} · {} messages · {}",
        fork.seq, fork.copied, fork.session.key
    );
    ctx.renderer.note(&text);
    Ok(SlashOutcome::Attach(fork.session.key))
}

fn edit_command(argv: &[String], tail: &str, ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let reference = argv.first();
    let text = reference
        .and_then(|reference| tail.strip_prefix(reference.as_str()))
        .unwrap_or("")
        .trim();
    let (Some(reference), false) = (reference, text.is_empty()) else {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            ctx.t.t(keys::slash::errors::USAGE_EDIT),
        ));
    };

    let store = ctx.runtime.store();
    let seq = resolve_seq(store, ctx.session_key, Some(reference.as_str()))?;
    let seq = require_user_message(store, ctx.session_key, seq, ctx.t)?;
    // Below the edited message: the loop appends the replacement itself, so
    // cutting *at* it would leave the old wording above the new one.
    store.truncate_after(ctx.session_key, seq - 1)?;
    Ok(SlashOutcome::Turn(text.to_owned()))
}

fn regenerate_command(argv: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let store = ctx.runtime.store();
    let seq = resolve_seq(store, ctx.session_key, argv.first().map(String::as_str))?;
    let seq = require_user_message(store, ctx.session_key, seq, ctx.t)?;
    let records = store.messages(
        ctx.session_key,
        &ReadMessages {
            after_seq: Some(seq - 1),
            before_seq: Some(seq + 1),
            ..ReadMessages::default()
        },
    )?;
    let Some(record) = records.first() else {
        return Err(WireError::new(
            ErrorKind::NotFound,
            ctx.t.t(keys::slash::errors::MESSAGE_GONE),
        ));
    };
    let content = text_of(&record.message);
    // Minus one, and for the same reason as the hub's: the loop appends the
    // question unconditionally, so truncating *to* `seq` and re-running would
    // write it twice.
    store.truncate_after(ctx.session_key, seq - 1)?;
    Ok(SlashOutcome::Turn(content))
}

/// Only a message the operator wrote can be edited or re-run.
fn require_user_message(
    store: &SessionStore,
    session_key: &str,
    seq: i64,
    t: &Translations,
) -> Result<i64> {
    let records = store.messages(
        session_key,
        &ReadMessages {
            after_seq: Some(seq - 1),
            before_seq: Some(seq + 1),
            ..ReadMessages::default()
        },
    )?;
    let is_user = records
        .first()
        .is_some_and(|record| matches!(record.message, darkwire_protocol::ChatMessage::User(_)));
    if !is_user {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            t.tr(keys::slash::errors::NOT_YOURS, args!["seq" => seq]),
        ));
    }
    Ok(seq)
}

// Context and cost

async fn context_command(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let agent_id = agent_for_settings(ctx)?;
    let agent_loop = ctx.runtime.require_loop()?;
    let config = ctx.runtime.config();
    let report = describe_context(
        ctx.runtime.store(),
        &agent_loop,
        &ctx.runtime.tools().definitions(),
        &PromptPreviewInput {
            session_key: ctx.session_key.to_owned(),
            channel: Some("cli".to_owned()),
            agent_id: Some(agent_id.clone()),
        },
        // This agent's budget — see `settings_of`.
        settings_of(&config, Some(&agent_id)).context_window_tokens,
    )
    .await?;

    let Some(report) = report else {
        let note = ctx.t.t(keys::slash::notes::NOTHING_TO_MEASURE);
        ctx.renderer.note(&note);
        return Ok(SlashOutcome::Continue);
    };
    let text = format_context(&report, ctx.t.locale().as_str());
    ctx.renderer.note(&text);
    Ok(SlashOutcome::Continue)
}

/// The `/context` breakdown.
///
/// Grouped with the locale's own separator rather than the machine's, which is
/// the same rule the browser applies: one install must not print `8.192` at a
/// prompt and `8,192` in a tab.
fn format_context(report: &darkwire_agent::ContextReport, locale: &str) -> String {
    let window = report.context_window_tokens.max(1);
    let estimated = u64::try_from(report.estimated_tokens).unwrap_or(u64::MAX);
    // Integer arithmetic with rounding applied by hand: a percentage of two
    // token counts has no business going through a float.
    let percent = (estimated.saturating_mul(100) + window / 2) / window;
    let n = |value: usize| format_number(i64::try_from(value).unwrap_or(i64::MAX), locale);
    let breakdown = &report.breakdown;
    [
        format!(
            "{} of {} tokens · {percent}%",
            n(report.estimated_tokens),
            format_number(i64::try_from(window).unwrap_or(i64::MAX), locale)
        ),
        format!(
            "  {}{}",
            pad_to_width("system", CONTEXT_LABEL_COLUMN),
            n(breakdown.system_prompt)
        ),
        format!(
            "  {}{}",
            pad_to_width("tools", CONTEXT_LABEL_COLUMN),
            n(breakdown.tools)
        ),
        format!(
            "  {}{}",
            pad_to_width("messages", CONTEXT_LABEL_COLUMN),
            n(breakdown.messages)
        ),
        // Last because it is last in the request, and called out because it is
        // the only one of the four paid again on every step of a turn.
        format!(
            "  {}{} (per step)",
            pad_to_width("live", CONTEXT_LABEL_COLUMN),
            n(breakdown.runtime_block)
        ),
        format!(
            "  {}{} messages",
            pad_to_width("in window", CONTEXT_LABEL_COLUMN),
            report.messages.len()
        ),
    ]
    .join("\n")
}

fn stats_command(argv: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let limit = positive_count(argv.first()).unwrap_or(DEFAULT_STATS_LINES);
    let rows = ctx
        .runtime
        .store()
        .turn_stats(ctx.session_key, Some(limit))?;
    if rows.is_empty() {
        let note = ctx.t.t(keys::slash::notes::NO_TURNS);
        ctx.renderer.note(&note);
        return Ok(SlashOutcome::Continue);
    }
    ctx.renderer.stats(&rows);
    Ok(SlashOutcome::Continue)
}

// What a turn shows

/// The parts of a turn that can be turned off, and how to read each one.
///
/// A table rather than a `match`, because `/output` with no argument has to
/// list them — and a listing derived from the same place the command reads
/// cannot go out of step with what the command accepts.
///
/// The names are what an operator types, so they are syntax and not prose.
const OUTPUT_FIELDS: [&str; 2] = ["reasoning", "stats"];

/// Whether one field is currently shown.
fn output_shown(field: &str, renderer: &TurnRenderer) -> Option<bool> {
    match field {
        "reasoning" => Some(renderer.reasoning_shown()),
        "stats" => Some(renderer.stats_shown()),
        _ => None,
    }
}

/// Shows or hides one field. Silently ignores a name nothing knows, which the
/// caller has already refused.
fn output_set(field: &str, renderer: &mut TurnRenderer, on: bool) {
    match field {
        "reasoning" => renderer.set_reasoning_shown(on),
        "stats" => renderer.set_stats_shown(on),
        _ => {}
    }
}

/// `/output` — what a turn prints, and what it does not.
///
/// One command rather than one per switch. The next thing worth hiding is then
/// a row in the table above rather than a new verb, a new help line and a new
/// pair of keys — and the bare form listing what is on is what makes the
/// switches discoverable at all, which two separate commands never were.
///
/// Naming a field with no word flips it, which is what a hand reaching for a
/// switch expects; `on` and `off` say it outright, for one that has lost track.
/// The setting lasts as long as the process: `--no-reasoning` is how a script
/// says it once, and a prompt asking to see less for the next few turns has not
/// made a decision worth writing to `config.yaml`.
fn output_command(
    field: Option<&str>,
    word: Option<&str>,
    ctx: &mut SlashContext<'_>,
) -> Result<SlashOutcome> {
    let Some(field) = field else {
        let column = OUTPUT_FIELDS
            .iter()
            .map(|name| visible_width(name))
            .max()
            .unwrap_or(0);
        let text = OUTPUT_FIELDS
            .iter()
            .map(|name| {
                let shown = output_shown(name, ctx.renderer).unwrap_or(false);
                let state = ctx.t.t(if shown {
                    keys::slash::notes::SHOWN
                } else {
                    keys::slash::notes::HIDDEN
                });
                format!("  {}  {state}", pad_to_width(name, column))
            })
            .collect::<Vec<_>>()
            .join("\n");
        ctx.renderer.note(&text);
        return Ok(SlashOutcome::Continue);
    };

    let Some(shown) = output_shown(field, ctx.renderer) else {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            ctx.t.tr(
                keys::slash::errors::NO_OUTPUT_FIELD,
                args!["field" => field],
            ),
        ));
    };

    let wanted = match word {
        Some("on") => true,
        Some("off") => false,
        // A word it does not know is a flip rather than a refusal: the hand
        // reaching for the switch has already said which switch.
        _ => !shown,
    };
    output_set(field, ctx.renderer, wanted);
    let note = ctx.t.tr(
        if wanted {
            keys::slash::notes::OUTPUT_SHOWN
        } else {
            keys::slash::notes::OUTPUT_HIDDEN
        },
        args!["field" => field],
    );
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

// Agents

/// `/agent` — show them, or move this conversation onto one.
///
/// The same decision `/workspace <id>` makes with the nouns changed, and that
/// is the argument for it reading the same way: both answer "which of these
/// does the next turn belong to".
///
/// With no argument this opens a picker on a terminal and prints a listing
/// anywhere else — so a pipe still gets an answer, and the answer it gets is
/// the one a person would have read off the menu. A cancelled picker falls
/// through to the same listing rather than to silence.
async fn agent_command(id: Option<&str>, ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let agents = ctx.runtime.agents();
    let current = ctx
        .runtime
        .store()
        .get_session(ctx.session_key)?
        .and_then(|session| session.agent_id)
        .or_else(|| ctx.agent_id.clone());

    let chosen = match id {
        Some(id) => Some(id.to_owned()),
        None if ctx.menu.available() => {
            pick_agent(ctx.menu, &agents, current.as_deref(), ctx.t).await
        }
        None => None,
    };

    let Some(chosen) = chosen else {
        let text = agent_listing(&agents, current.as_deref(), ctx.t);
        ctx.renderer.note(&text);
        return Ok(SlashOutcome::Continue);
    };
    if !agents.iter().any(|agent| agent.id == chosen) {
        return Err(WireError::new(
            ErrorKind::NotFound,
            ctx.t.tr(
                keys::slash::errors::NO_AGENT,
                args!["id" => chosen.as_str()],
            ),
        ));
    }

    *ctx.agent_id = Some(chosen.clone());

    // A conversation that exists moves; one nobody has spoken in has no row to
    // move, and the choice is only what the next one will run on.
    //
    // The `get_session` guard is load-bearing for the reason `/workspace` gives
    // below: patching a session creates it, so patching an unspoken one would
    // mint an empty row — which is what makes it show up in the sidebar.
    if ctx.runtime.store().get_session(ctx.session_key)?.is_none() {
        let note = ctx.t.tr(
            keys::slash::notes::WILL_RUN_ON,
            args!["agent" => chosen.as_str()],
        );
        ctx.renderer.note(&note);
        return Ok(SlashOutcome::Continue);
    }
    ctx.runtime.store().update_session(
        ctx.session_key,
        UpdateSession {
            agent_id: Some(Some(chosen.clone())),
            ..UpdateSession::default()
        },
    )?;
    let note = ctx.t.tr(
        keys::slash::notes::MOVED_AGENT,
        args!["agent" => chosen.as_str()],
    );
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

/// Which agent the next turn in this conversation would run on.
///
/// The hub's rule, called rather than restated: the stored session wins over
/// the prompt's preference, because a history built under one agent's prompt
/// and tools does not silently continue under another's. The fallback is the
/// default agent, which is what a turn would land on anyway once a departed id
/// stops resolving.
///
/// This is the agent `/model`, `/effort` and `/temperature` edit. They change
/// *an agent*, not a session — there is no per-conversation model — so the one
/// thing they must get right is which agent that is.
fn agent_for_settings(ctx: &SlashContext<'_>) -> Result<String> {
    let agents = ctx.runtime.agents();
    let stored = ctx
        .runtime
        .store()
        .get_session(ctx.session_key)?
        .and_then(|session| session.agent_id);
    let resolves = |id: &str| agents.iter().any(|agent| agent.id == id);
    Ok(
        agent_for_turn(stored.as_deref(), ctx.agent_id.as_deref(), &resolves)
            .unwrap_or_else(|| DEFAULT_AGENT_ID.to_owned()),
    )
}

/// That agent's label, for a note that has to name what it moved.
fn agent_label(ctx: &SlashContext<'_>, agent_id: &str) -> String {
    ctx.runtime
        .agents()
        .into_iter()
        .find(|agent| agent.id == agent_id)
        .map_or_else(|| agent_id.to_owned(), |agent| agent.label)
}

/// `/model` — what this install can reach, and which of them this agent uses.
///
/// **It edits the agent this conversation runs on, and it saves.** An agent is
/// the only place a model lives, so the agent the conversation is bound to is
/// the one that has to move; and the settings panel writes the same field, so
/// this writes it the same way rather than scoping a half-edit to one process.
///
/// The patch itself comes from `agent_settings_patch`, which is where the rule
/// that `agents.list.*` replaces wholesale lives. Do not build one here: a
/// patch naming `model` alone does not set one field on a named agent, it
/// replaces the agent and deletes its prompt, tools and container with it.
///
/// **It refuses outright under `--model`.** That flag is a statement about this
/// process that the config cannot move, so a `/model` that appeared to work and
/// changed nothing would be worse than one that will not.
async fn model_command(id: Option<&str>, ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    if ctx.model_pinned {
        return Err(WireError::new(
            ErrorKind::Conflict,
            ctx.t.t(keys::slash::errors::MODEL_PINNED),
        ));
    }

    let chosen = match id {
        Some(id) => Some(id.to_owned()),
        None => choose_model(ctx).await?,
    };
    let Some(chosen) = chosen else {
        return Ok(SlashOutcome::Continue);
    };

    // Validated even when typed straight past the picker. `/agent` refuses an
    // id that names nothing, and this needs the lookup anyway: a model and the
    // endpoint that serves it are one setting, and the provider is the half
    // only the catalogue knows.
    let catalogue = ctx.models.list().await?;
    let Some(info) = catalogue.models.iter().find(|model| model.id == chosen) else {
        return Err(WireError::new(
            ErrorKind::NotFound,
            ctx.t.tr(
                keys::slash::errors::NO_MODEL,
                args!["id" => chosen.as_str()],
            ),
        ));
    };

    let agent_id = agent_for_settings(ctx)?;
    let patch = agent_settings_patch(
        &ctx.runtime.config(),
        &agent_id,
        &AgentSettingsChange {
            model: Some(info.id.clone()),
            provider: Some(info.provider_id.clone()),
            ..AgentSettingsChange::default()
        },
    );
    save_settings(ctx.runtime, &patch)?;
    let note = ctx.t.tr(
        keys::slash::notes::MODEL_SET,
        args!["model" => info.id.as_str(), "agent" => agent_label(ctx, &agent_id)],
    );
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

/// The picker, or the listing, or a reason there is neither.
async fn choose_model(ctx: &mut SlashContext<'_>) -> Result<Option<String>> {
    let catalogue = ctx.models.list().await?;

    // Which endpoint went quiet, said out loud. A silently shorter list reads
    // as "that model is gone" rather than "that laptop is shut".
    for line in model_errors(&catalogue, ctx.t) {
        ctx.renderer.warn(&line);
    }

    if catalogue.models.is_empty() {
        let note = ctx.t.t(keys::slash::notes::NO_MODELS);
        ctx.renderer.note(&note);
        return Ok(None);
    }

    // The *conversation's* model, not the install's. The runtime's is the
    // default agent's, and a conversation moved onto another agent runs on that
    // one — so marking the current row means asking the agent, not the runtime.
    let agent_id = agent_for_settings(ctx)?;
    let current = model_of_agent(ctx, &agent_id);

    if !ctx.menu.available() {
        let text = model_listing(&catalogue, &current);
        ctx.renderer.note(&text);
        return Ok(None);
    }

    Ok(pick_model(ctx.menu, &catalogue, &current, ctx.t).await)
}

/// The model one agent would actually send, resolved.
///
/// Off the agent's *loop* rather than through a fresh provider resolution,
/// which is the same expression the server's port uses. Resolving again would
/// open the credential vault, and opening the vault can mint a keychain entry —
/// too much to do to label a menu row.
fn model_of_agent(ctx: &SlashContext<'_>, agent_id: &str) -> String {
    let configured = ctx
        .runtime
        .agents()
        .into_iter()
        .find(|agent| agent.id == agent_id)
        .map(|agent| agent.settings.model)
        .unwrap_or_default();
    ctx.runtime
        .loop_for(Some(agent_id))
        .ok()
        .flatten()
        .map_or(configured, |one| one.model().to_owned())
}

/// One of the two settings `/effort` and `/temperature` move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleField {
    /// `reasoningEffort` in the settings, `effort` at the prompt.
    Effort,
    /// One name in both places.
    Temperature,
}

impl SampleField {
    /// What was typed, which is what a note quotes.
    ///
    /// A sentence about "reasoningEffort" is describing the config file rather
    /// than answering the person at the prompt.
    fn word(self) -> &'static str {
        match self {
            SampleField::Effort => "effort",
            SampleField::Temperature => "temperature",
        }
    }
}

/// What one of the two settings was set to.
enum SampleValue {
    /// A reasoning level.
    Effort(ReasoningEffort),
    /// A sampling temperature.
    Temperature(f64),
}

impl SampleValue {
    /// The value as the note prints it.
    fn display(&self) -> String {
        match self {
            SampleValue::Effort(effort) => effort_value(*effort).to_owned(),
            SampleValue::Temperature(value) => format!("{value}"),
        }
    }

    /// The change that applies it.
    fn change(&self) -> AgentSettingsChange {
        match self {
            SampleValue::Effort(effort) => AgentSettingsChange {
                reasoning_effort: Some(Some(*effort)),
                ..AgentSettingsChange::default()
            },
            SampleValue::Temperature(value) => AgentSettingsChange {
                temperature: Some(Some(*value)),
                ..AgentSettingsChange::default()
            },
        }
    }
}

/// The change that clears one of the two settings.
fn clearing(field: SampleField) -> AgentSettingsChange {
    match field {
        SampleField::Effort => AgentSettingsChange {
            reasoning_effort: Some(None),
            ..AgentSettingsChange::default()
        },
        SampleField::Temperature => AgentSettingsChange {
            temperature: Some(None),
            ..AgentSettingsChange::default()
        },
    }
}

/// `/effort` and `/temperature` — the two sampling settings that have a
/// "say nothing at all" state.
///
/// They are one function because they are one decision with two spellings.
/// Unset is not a value: an absent temperature means the request carries no
/// such parameter and the provider applies its own, which is the only thing
/// that works against the endpoints that reject the field outright. So
/// `default` is a word rather than a number, and it reaches the patch as the
/// `null` that clears — which the patch turns into a deleted key, because an
/// entry replaces wholesale and an absent key is how "send nothing" is spelled.
///
/// **Only one of the two can be picked from a menu**, and the asymmetry is the
/// subject rather than an omission. Effort is an enum, so it has the same shape
/// as `/model` and `/agent`: no argument opens the list, and what is in force is
/// the row the cursor starts on. A temperature is a number in a range, which a
/// list cannot enumerate — so bare `/temperature` answers the question a picker
/// would have answered by opening, and says what is being sent now.
///
/// Neither refuses under `--model`. That flag pins a model; it says nothing
/// about how hard the model thinks.
async fn sample_command(
    field: SampleField,
    raw: Option<&str>,
    ctx: &mut SlashContext<'_>,
) -> Result<SlashOutcome> {
    let agent_id = agent_for_settings(ctx)?;
    let agent = agent_label(ctx, &agent_id);
    let name = field.word();

    let chosen = match raw {
        Some(raw) => Some(raw.to_owned()),
        None => choose_sample(field, &agent_id, ctx).await,
    };
    let Some(chosen) = chosen else {
        return Ok(SlashOutcome::Continue);
    };

    let config = ctx.runtime.config();
    if chosen == DEFAULT_LEVEL {
        let patch = agent_settings_patch(&config, &agent_id, &clearing(field));
        save_settings(ctx.runtime, &patch)?;
        let note = ctx.t.tr(
            keys::slash::notes::SAMPLE_DEFAULT,
            args!["field" => name, "agent" => agent.as_str()],
        );
        ctx.renderer.note(&note);
        return Ok(SlashOutcome::Continue);
    }

    let value = parse_sample(field, &chosen, ctx.t)?;
    let patch = agent_settings_patch(&config, &agent_id, &value.change());
    save_settings(ctx.runtime, &patch)?;
    let note = ctx.t.tr(
        keys::slash::notes::SAMPLE_SET,
        args!["field" => name, "value" => value.display(), "agent" => agent.as_str()],
    );
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

/// The picker, the listing, or the readout — whichever the field and the
/// terminal can offer.
///
/// `None` means "nothing to apply", which covers all three of a cancelled menu,
/// a listing printed to a pipe and a temperature reported rather than changed.
/// The caller treats them alike.
async fn choose_sample(
    field: SampleField,
    agent_id: &str,
    ctx: &mut SlashContext<'_>,
) -> Option<String> {
    let settings = ctx
        .runtime
        .agents()
        .into_iter()
        .find(|agent| agent.id == agent_id)
        .map(|agent| agent.settings);

    if field == SampleField::Effort {
        let level = settings.and_then(|settings| settings.reasoning_effort);
        if !ctx.menu.available() {
            let text = effort_listing(level, ctx.t);
            ctx.renderer.note(&text);
            return None;
        }
        return pick_effort(ctx.menu, level, ctx.t).await;
    }

    let current = settings.and_then(|settings| settings.temperature);
    let label = agent_label(ctx, agent_id);
    let note = match current {
        None => ctx.t.tr(
            keys::slash::notes::SAMPLE_UNSET,
            args!["field" => field.word(), "agent" => label.as_str()],
        ),
        Some(value) => ctx.t.tr(
            keys::slash::notes::SAMPLE_IS,
            args!["field" => field.word(), "value" => format!("{value}"), "agent" => label.as_str()],
        ),
    };
    ctx.renderer.note(&note);
    None
}

/// The one value each field accepts, or why this is not one.
fn parse_sample(field: SampleField, raw: &str, t: &Translations) -> Result<SampleValue> {
    if field == SampleField::Effort {
        return crate::pickers::effort::parse_effort(raw)
            .map(SampleValue::Effort)
            .ok_or_else(|| {
                let levels = LEVELS
                    .into_iter()
                    .map(effort_value)
                    .collect::<Vec<_>>()
                    .join(", ");
                WireError::new(
                    ErrorKind::InvalidInput,
                    t.tr(
                        keys::slash::errors::USAGE_EFFORT,
                        args!["levels" => levels.as_str()],
                    ),
                )
            });
    }

    // A whole parse rather than a leading one, which would read `0.5abc` as
    // `0.5`. A temperature that was half a typo is a refusal, not a guess.
    let value = raw.parse::<f64>().ok().filter(|value| value.is_finite());
    match value {
        Some(value) if (0.0..=2.0).contains(&value) => Ok(SampleValue::Temperature(value)),
        _ => Err(WireError::new(
            ErrorKind::InvalidInput,
            t.t(keys::slash::errors::USAGE_TEMPERATURE),
        )),
    }
}

// Memory and skills

/// `/memory`, and its two verbs.
///
/// The switch is the `memory` tool's permission, not a setting of its own. That
/// is why `on`/`off` reconfigure rather than writing to the session row: the
/// capability belongs to the agent, and a session-scoped override would be a
/// second source of truth for one question.
///
/// There is no verb that folds a conversation into memory. The memory folder
/// holds facts about a workspace, and a summary of one session is not one of
/// those.
fn memory_command(argv: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    match argv.first().map(String::as_str) {
        None => memory_status(ctx),
        Some(verb @ ("on" | "off")) => set_memory_permission(verb == "on", ctx),
        Some(_) => Err(WireError::new(
            ErrorKind::InvalidInput,
            ctx.t.t(keys::slash::errors::USAGE_MEMORY),
        )),
    }
}

/// Which agent this conversation runs on, whether or not it has spoken yet.
fn memory_agent_id(ctx: &SlashContext<'_>) -> Result<String> {
    let stored = ctx
        .runtime
        .store()
        .get_session(ctx.session_key)?
        .and_then(|session| session.agent_id);
    Ok(stored
        .or_else(|| ctx.agent_id.clone())
        .unwrap_or_else(|| DEFAULT_AGENT_ID.to_owned()))
}

/// The workspace this conversation's files live in.
fn workspace_of_session(ctx: &SlashContext<'_>) -> Result<String> {
    Ok(ctx
        .runtime
        .store()
        .get_session(ctx.session_key)?
        .map_or_else(
            || DEFAULT_WORKSPACE_ID.to_owned(),
            |session| session.workspace_id,
        ))
}

/// Whether one agent holds a tool. Absent counts as denied.
fn tool_granted(ctx: &SlashContext<'_>, agent_id: &str, tool: &str) -> bool {
    ctx.runtime
        .agents()
        .iter()
        .find(|agent| agent.id == agent_id)
        .and_then(|agent| agent.tools.get(tool).copied())
        .is_some_and(|permission| permission != ToolPermission::Deny)
}

fn memory_status(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let agent_id = memory_agent_id(ctx)?;

    if !tool_granted(ctx, &agent_id, "memory") {
        let note = ctx.t.tr(
            keys::slash::notes::MEMORY_OFF,
            args!["agent" => agent_id.as_str()],
        );
        ctx.renderer.note(&note);
        return Ok(SlashOutcome::Continue);
    }

    let workspace = workspace_of_session(ctx)?;
    let jail = ctx.runtime.jails().for_workspace(&workspace);
    let memories = read_memories(jail.root());

    // A count and what the index costs, which are the two numbers an operator
    // can act on. The line this replaced measured one file, and there is no one
    // file.
    let note = if memories.is_empty() {
        ctx.t.tr(
            keys::slash::notes::MEMORY_EMPTY,
            args!["path" => MEMORY_DIRNAME],
        )
    } else {
        let index = memories
            .iter()
            .map(|memory| memory.description.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let tokens = format_number(
            i64::try_from(estimate_tokens(&index)).unwrap_or(i64::MAX),
            ctx.t.locale().as_str(),
        );
        ctx.t.tr(
            keys::slash::notes::MEMORY_COUNT,
            args![
                "count" => memories.len(),
                "path" => MEMORY_DIRNAME,
                "tokens" => tokens.as_str(),
            ],
        )
    };
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

/// Flips the `memory` permission on this conversation's agent.
///
/// **The whole entry is rewritten, not patched.** `agents.list.*` replaces
/// wholesale, so sending the one permission would delete every other permission
/// and every other override this agent holds. The effective map is read back
/// first and written whole.
fn set_memory_permission(on: bool, ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let agent_id = memory_agent_id(ctx)?;
    let agents = ctx.runtime.agents();
    let Some(agent) = agents.iter().find(|entry| entry.id == agent_id) else {
        return Err(WireError::new(
            ErrorKind::NotFound,
            ctx.t.tr(
                keys::slash::errors::NO_AGENT,
                args!["id" => agent_id.as_str()],
            ),
        ));
    };

    let config = ctx.runtime.config();
    let mut entry = config
        .agents
        .list
        .get(&agent_id)
        .cloned()
        .unwrap_or_default();
    let mut tools = agent.tools.clone();
    tools.insert(
        "memory".to_owned(),
        if on {
            ToolPermission::Allow
        } else {
            ToolPermission::Deny
        },
    );
    entry.tools = tools;

    let entry = serde_json::to_value(&entry).map_err(|error| {
        WireError::new(ErrorKind::Internal, "The agent entry could not be encoded")
            .with_source(error)
    })?;
    ctx.runtime.reconfigure(&serde_json::json!({
        "agents": { "list": { agent_id.clone(): entry } }
    }))?;

    let note = ctx.t.tr(
        if on {
            keys::slash::notes::MEMORY_ENABLED
        } else {
            keys::slash::notes::MEMORY_DISABLED
        },
        args!["agent" => agent_id.as_str()],
    );
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

/// The workspace's skills, as a list.
///
/// The catalogue is already in the prompt, so this is not what tells the
/// *model* about a skill — the agent opens the sheet itself when the
/// description says it applies. It is what tells the person which sheets this
/// workspace holds.
fn skills_command(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let agent_id = memory_agent_id(ctx)?;

    // The same gate the contributor uses, and the same reason: a denial takes
    // the catalogue out of the prompt, so listing sheets would be listing
    // something this agent cannot reach. Absent counts as denied.
    if !tool_granted(ctx, &agent_id, "skill") {
        let note = ctx.t.tr(
            keys::slash::notes::SKILLS_OFF,
            args!["agent" => agent_id.as_str()],
        );
        ctx.renderer.note(&note);
        return Ok(SlashOutcome::Continue);
    }

    let workspace = workspace_of_session(ctx)?;
    let jail = ctx.runtime.jails().for_workspace(&workspace);
    let skills = read_skills(jail.root());

    let text = if skills.is_empty() {
        ctx.t.tr(
            keys::slash::notes::SKILLS_EMPTY,
            args!["path" => SKILLS_DIRNAME],
        )
    } else {
        skills
            .iter()
            .map(|skill| {
                // Every sheet, with the ones out of scope marked rather than
                // hidden. This is what the workspace holds, and somebody
                // running `/skills` because a sheet is not working needs to see
                // it and be told why — not to find it missing from a list too.
                let scoped = !skill.agents.is_empty() && !skill.agents.contains(&agent_id);
                let suffix = if scoped {
                    format!(
                        "  ·  {}",
                        ctx.t.tr(
                            keys::slash::notes::SKILLS_SCOPE,
                            args!["agents" => skill.agents.join(", ")],
                        )
                    )
                } else {
                    String::new()
                };
                format!("{}  ·  {}{suffix}", skill.name, skill.description)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    ctx.renderer.note(&text);
    Ok(SlashOutcome::Continue)
}

// Workspaces

fn workspaces_command(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let store = ctx.runtime.store();
    let current = match ctx.workspace_id.clone() {
        Some(id) => Some(id),
        None => store
            .get_session(ctx.session_key)?
            .map(|session| session.workspace_id),
    };
    let mut lines = Vec::new();
    for workspace in ctx.runtime.workspaces().list()? {
        let mark = if Some(&workspace.id) == current.as_ref() {
            '*'
        } else {
            ' '
        };
        let count = store.count_by_workspace(&workspace.id)?;
        lines.push(format!(
            "{mark} {}  ·  {}  ·  {count} sessions",
            workspace.id, workspace.name
        ));
    }
    let text = lines.join("\n");
    ctx.renderer.note(&text);
    Ok(SlashOutcome::Continue)
}

async fn workspace_command(argv: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let verb = argv.first().map(String::as_str);
    let rest = argv.get(1..).unwrap_or(&[]);

    match verb {
        None => workspace_pending(ctx).await,
        Some("new") => workspace_new(rest, ctx),
        Some("rename") => workspace_rename(rest, ctx),
        Some("rm") => workspace_rm(rest, ctx),
        Some("move") => workspace_move(rest, ctx),
        // Not a verb, so it is an id: `/workspace <id>` switches.
        Some(id) => switch_workspace(id, ctx),
    }
}

/// Bare `/workspace` — a picker on a terminal, the note everywhere else.
async fn workspace_pending(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let pending = ctx
        .workspace_id
        .clone()
        .unwrap_or_else(|| DEFAULT_WORKSPACE_ID.to_owned());
    if ctx.menu.available() {
        let workspaces = ctx.runtime.workspaces().list()?;
        let chosen = pick_workspace(ctx.menu, &workspaces, Some(&pending), ctx.t).await;
        if let Some(chosen) = chosen {
            return switch_workspace(&chosen, ctx);
        }
    }
    let note = ctx.t.tr(
        keys::slash::notes::LAND_IN,
        args!["workspace" => pending.as_str()],
    );
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

fn workspace_new(rest: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let name = rest.join(" ").trim().to_owned();
    if name.is_empty() {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            ctx.t.t(keys::slash::errors::USAGE_WORKSPACE_NEW),
        ));
    }
    let created = ctx.runtime.workspaces().create(CreateWorkspace {
        name,
        ..CreateWorkspace::default()
    })?;
    let note = ctx.t.tr(
        keys::slash::notes::CREATED,
        args!["id" => created.id.as_str()],
    );
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

fn workspace_rename(rest: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let id = rest.first();
    let name = rest.get(1..).unwrap_or(&[]).join(" ").trim().to_owned();
    let (Some(id), false) = (id, name.is_empty()) else {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            ctx.t.t(keys::slash::errors::USAGE_WORKSPACE_RENAME),
        ));
    };
    ctx.runtime.workspaces().rename(id, &name)?;
    let note = ctx.t.tr(
        keys::slash::notes::RENAMED_WORKSPACE,
        args!["id" => id.as_str(), "name" => name.as_str()],
    );
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

/// `/workspace rm <id>` — detach, and only when nothing still names it.
///
/// The same refusal the web manager makes, and for the same reason: a detached
/// workspace whose conversations still name it would leave them resolving to
/// files nothing lists. Two explicit steps, not one silent one.
fn workspace_rm(rest: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let Some(id) = rest.first() else {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            ctx.t.t(keys::slash::errors::USAGE_WORKSPACE_RM),
        ));
    };
    let count = ctx.runtime.store().count_by_workspace(id)?;
    if count > 0 {
        return Err(WireError::new(
            ErrorKind::Conflict,
            ctx.t.tr(
                keys::slash::errors::WORKSPACE_IN_USE,
                args!["count" => count, "id" => id.as_str()],
            ),
        ));
    }
    ctx.runtime.workspaces().delete(id)?;
    let note = ctx
        .t
        .tr(keys::slash::notes::DETACHED, args!["id" => id.as_str()]);
    ctx.renderer.note(&note);
    if ctx.workspace_id.as_deref() == Some(id.as_str()) {
        *ctx.workspace_id = None;
    }
    Ok(SlashOutcome::Continue)
}

fn workspace_move(rest: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let (Some(from), Some(to)) = (rest.first(), rest.get(1)) else {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            ctx.t.t(keys::slash::errors::USAGE_WORKSPACE_MOVE),
        ));
    };
    if ctx.runtime.workspaces().get(to)?.is_none() {
        return Err(WireError::new(
            ErrorKind::NotFound,
            ctx.t.tr(
                keys::slash::errors::NO_WORKSPACE,
                args!["id" => to.as_str()],
            ),
        ));
    }
    let moved = ctx.runtime.store().reassign_workspace(from, to)?;
    let note = ctx.t.tr(
        keys::slash::notes::MOVED,
        args!["count" => moved, "from" => from.as_str(), "to" => to.as_str()],
    );
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

/// `/workspace <id>`, and what the picker resolves to.
fn switch_workspace(id: &str, ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    if ctx.runtime.workspaces().get(id)?.is_none() {
        return Err(WireError::new(
            ErrorKind::NotFound,
            ctx.t
                .tr(keys::slash::errors::NO_WORKSPACE, args!["id" => id]),
        ));
    }
    *ctx.workspace_id = Some(id.to_owned());

    // A conversation that exists moves with the switch; one nobody has spoken
    // in has no row to move, and the choice is only where it will land.
    //
    // The `get_session` guard is load-bearing: patching a session creates it,
    // so patching an unspoken one would mint an empty row — which is what makes
    // it show up in the sidebar.
    if ctx.runtime.store().get_session(ctx.session_key)?.is_none() {
        let note = ctx
            .t
            .tr(keys::slash::notes::WILL_LAND_IN, args!["workspace" => id]);
        ctx.renderer.note(&note);
        return Ok(SlashOutcome::Continue);
    }
    ctx.runtime.store().update_session(
        ctx.session_key,
        UpdateSession {
            workspace_id: Some(id.to_owned()),
            ..UpdateSession::default()
        },
    )?;
    let note = ctx
        .t
        .tr(keys::slash::notes::MOVED_SESSION, args!["workspace" => id]);
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

// Extensions

/// `/<id>` for a command an extension contributed, or the usual refusal.
///
/// Answers with the extension's own text rather than a bundle key: its copy
/// ships with the extension and the terminal's bundle has never seen it.
async fn extension_command(
    name: &str,
    tail: &str,
    ctx: &mut SlashContext<'_>,
) -> Result<SlashOutcome> {
    let host = ctx.runtime.extensions().cloned();
    let known = host
        .as_ref()
        .is_some_and(|host| host.commands().iter().any(|command| command.id == name));

    let (Some(host), true) = (host, known) else {
        let note = ctx
            .t
            .tr(keys::slash::notes::UNKNOWN_COMMAND, args!["name" => name]);
        ctx.renderer.warn(&note);
        return Ok(SlashOutcome::Continue);
    };

    // The prompt has no per-command cancellation — Ctrl-C stops the turn — so
    // this is a token that never fires rather than a lie about one that does.
    // The web composer passes the request's, which does.
    let token = CancellationToken::new();
    let result = host
        .run_command(name, tail, Some(ctx.session_key), &token)
        .await?;

    if result.ok {
        ctx.renderer.note(&result.message);
    } else {
        ctx.renderer.warn(&result.message);
    }
    Ok(SlashOutcome::Continue)
}

// Adapters and small helpers

/// A [`crate::menu::Menu`] behind the asynchronous trait the pickers use.
///
/// The frame's own menu is synchronous and takes `&mut self`, because the frame
/// owner draws rows and reads keystrokes in one place. The pickers need `&self`
/// and a future, because a command holds the renderer while it opens one. The
/// lock is what reconciles the two, and it is never contended: a prompt opens
/// one menu at a time by construction.
pub struct SyncMenu<M> {
    inner: Mutex<M>,
}

impl<M: Menu + Send> SyncMenu<M> {
    /// Wraps one frame menu.
    pub fn new(menu: M) -> SyncMenu<M> {
        SyncMenu {
            inner: Mutex::new(menu),
        }
    }
}

impl<M> std::fmt::Debug for SyncMenu<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncMenu")
    }
}

impl<M: Menu + Send> PickerMenu for SyncMenu<M> {
    fn available(&self) -> bool {
        self.inner.lock().is_ok_and(|menu| menu.available())
    }

    fn choose<'a>(
        &'a self,
        request: MenuRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<usize>> + Send + 'a>> {
        let rows = request
            .items
            .iter()
            .map(|item| crate::menu::MenuRow {
                label: item.label.clone(),
                hint: item.hint.clone(),
                keywords: item.keywords.clone(),
                disabled: item.disabled,
            })
            .collect();
        let mut framed = crate::menu::MenuRequest::new(rows, request.labels);
        framed.index = request.index;
        let chosen = self
            .inner
            .lock()
            .ok()
            .and_then(|mut menu| menu.choose(framed));
        Box::pin(std::future::ready(chosen))
    }
}

/// A count an operator typed, or nothing when it was not one.
///
/// Only a whole number above zero: `/messages 0` and `/messages -3` are
/// mistakes rather than requests, and falling back to the default is what a
/// person meant by typing the command at all.
fn positive_count(value: Option<&String>) -> Option<usize> {
    value?.parse::<usize>().ok().filter(|count| *count > 0)
}

/// The same shape the create route mints: an origin and a uuid.
///
/// Off the host clock and the operating system's generator rather than an
/// injected pair. A session key is not a value anything asserts on — the tests
/// read it back from the outcome — and threading a clock through every command
/// to mint one would be injection for its own sake.
fn random_key() -> String {
    let mut random = [0u8; 10];
    RandomSource::fill(&OsRandom, &mut random);
    let now = Clock::now_ms(&SystemClock);
    new_uuid(u64::try_from(now).unwrap_or(0), &random)
}
