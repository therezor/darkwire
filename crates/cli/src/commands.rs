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
//! ## How a command opens a menu
//!
//! Through [`crate::pickers::PickerMenu`]. Its `choose` is asynchronous,
//! because a menu opens inside a running prompt and answers keystrokes later,
//! and it takes `&self`, so a command can open one while still holding the
//! renderer. What is on the other end is a channel to the loop, which is what
//! keeps the prompt reading the keyboard while the command waits.

use darkwire_agent::skills::{SKILLS_DIRNAME, Skill, read_skills};
use darkwire_agent::{PromptPreviewInput, describe_context};
use darkwire_core::memory::{MEMORY_DIRNAME, index_line, read_memories};
use darkwire_core::session_store::{
    ForkSession, ListSessions, ReadMessages, SessionSummaryRecord, UpdateSession,
};
use darkwire_core::workspace_store::{CreateWorkspace, WorkspaceRecord};
use darkwire_core::{Clock, ErrorKind, Result, SessionStore, SystemClock, WireError, text_of};
use darkwire_i18n::{args, format_number, keys};
use darkwire_protocol::config::{AgentSettingsChange, agent_settings_patch};
use darkwire_protocol::tasks::{TaskStatus, render_tasks};
use darkwire_protocol::{
    DEFAULT_AGENT_ID, DEFAULT_WORKSPACE_ID, ModelsResponse, ReasoningEffort, ToolPermission,
    new_uuid,
};
use darkwire_providers::{estimate_message_tokens, estimate_tokens, estimate_tool_tokens};
use darkwire_security::random::{OsRandom, RandomSource};
use darkwire_server::agent_for_turn;
use darkwire_tui::{
    Page, PagesLabels, SelectItem, pad_to_width, truncate_to_width, visible_width, wrap_to_width,
};
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

use crate::i18n::Translations;
use crate::menu::{columns_or_default, terminal_columns};
use crate::messages::resolve_seq;
use crate::models::ModelCatalogue;
use crate::pickers::PickerMenu;
use crate::pickers::agents::{agent_listing, pick_agent};
use crate::pickers::effort::{DEFAULT_LEVEL, LEVELS, effort_listing, effort_value, pick_effort};
use crate::pickers::memories::{MemoryChoice, show_memories};
use crate::pickers::models::{model_errors, model_listing, pick_model};
use crate::pickers::palette::PaletteRow;
use crate::pickers::sessions::{SessionVerb, pick_session};
use crate::pickers::skills::{SkillChoice, out_of_scope, show_skills, skill_items};
use crate::pickers::tasks::show_tasks;
use crate::pickers::workspaces::{
    WorkspaceRow, WorkspaceVerb, manage_workspaces, pick_destination,
};
use crate::pickers::{AskRequest, ListingRequest, Placement, choose_from};
use crate::render::TurnRenderer;
use crate::runtime::{ChatRuntime, save_settings, settings_of};

/// How many sessions `/sessions` lists when no count is given.
const DEFAULT_SESSION_LINES: usize = 20;

/// How much of an overlay's width the two-space indent and a margin take.
const OVERLAY_GUTTER: usize = 4;

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
    /// Lay the whole transcript over the prompt.
    Transcript,
    /// Hand this back to the composer for the operator to finish.
    ///
    /// What a modal answers with instead of doing the thing. A rename needs a
    /// name typed and a removal deserves a look before it happens, and the
    /// prompt is already a place to type and to look. Nothing is put anywhere
    /// on a surface with no composer, which is why the modal that offers this
    /// never opens on one.
    Compose(String),
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
    /// A name for the conversation being started, until there is one to name.
    ///
    /// `/new <title>` is a decision made before the row exists, and the row is
    /// written by the first turn. This is where the decision waits.
    pub pending_title: &'a mut Option<String>,
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

/// Every key the prompt binds, as `/help` prints them.
///
/// Deliberately not a section of [`help_layout`]. That table is flattened by
/// [`command_rows`] and handed to the palette, to Tab completion and to the
/// list a slash command opens, so a Keys section there would offer `ctrl-t` as
/// a command to run. These are keys, they are never typed, and they belong to a
/// listing rather than to a vocabulary.
fn key_layout() -> Vec<(&'static str, &'static str)> {
    vec![
        ("ctrl-g", keys::slash::keys::PALETTE),
        ("tab", keys::slash::keys::COMPLETE),
        ("return", keys::slash::keys::RUN),
        ("ctrl-t", keys::slash::keys::TRANSCRIPT),
        ("ctrl-o", keys::slash::keys::TOOLS),
        ("ctrl-y", keys::slash::keys::STATS),
        ("ctrl-l", keys::slash::keys::REDRAW),
        ("ctrl-c", keys::slash::keys::CANCEL),
        ("ctrl-d", keys::slash::keys::LEAVE),
        ("up, down", keys::slash::keys::HISTORY),
        (
            "ctrl-a, ctrl-e, ctrl-b, ctrl-f",
            keys::slash::keys::EDIT_LINE,
        ),
        ("alt-left, alt-right", keys::slash::keys::EDIT_WORD),
        ("ctrl-u, ctrl-k, ctrl-w", keys::slash::keys::KILL),
    ]
}

/// Every command, grouped as `/help` prints them.
fn help_layout() -> Vec<HelpSection> {
    vec![
        HelpSection {
            heading: None,
            rows: vec![
                CommandRow::new("/help", keys::slash::help::HELP),
                CommandRow::new("/transcript", keys::slash::help::TRANSCRIPT),
                CommandRow::new("/clear", keys::slash::help::CLEAR),
                CommandRow::new("/exit, /quit", keys::slash::help::EXIT),
            ],
        },
        HelpSection {
            heading: Some(keys::slash::sections::SESSIONS),
            rows: vec![
                CommandRow::new("/session [key]", keys::slash::help::SESSION),
                CommandRow::new("/new [title]", keys::slash::help::NEW),
                CommandRow::new("/rename <title>", keys::slash::help::RENAME),
                CommandRow::new("/delete", keys::slash::help::DELETE),
                CommandRow::new("/branch", keys::slash::help::BRANCH),
            ],
        },
        HelpSection {
            heading: Some(keys::slash::sections::MESSAGES),
            rows: vec![
                CommandRow::new("/edit <text>", keys::slash::help::EDIT),
                CommandRow::new("/regenerate", keys::slash::help::REGENERATE),
            ],
        },
        HelpSection {
            heading: Some(keys::slash::sections::CONTEXT),
            rows: vec![
                CommandRow::new("/context", keys::slash::help::CONTEXT),
                CommandRow::new("/tasks", keys::slash::help::TASKS),
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
                CommandRow::new("/skills", keys::slash::help::SKILLS),
            ],
        },
        HelpSection {
            heading: Some(keys::slash::sections::WORKSPACES),
            rows: vec![CommandRow::new(
                "/workspace [id]",
                keys::slash::help::WORKSPACE,
            )],
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

    let bindings = key_layout();
    let key_width = bindings
        .iter()
        .map(|(binding, _)| visible_width(binding))
        .max()
        .unwrap_or(0);
    let key_rows: Vec<String> = bindings
        .iter()
        .map(|(binding, key)| format!("  {}  {}", pad_to_width(binding, key_width), t.t(key)))
        .collect();
    let key_block = format!(
        "  {}\n{}",
        t.t(keys::slash::sections::KEYS),
        key_rows.join("\n")
    );

    format!("{}\n\n{key_block}", blocks.join("\n\n"))
}

/// `/help` as tabs, for a terminal that can lay an overlay over the prompt.
///
/// The same rows [`help_text`] writes, grouped into four places instead of one
/// scroll. A listing printed into the conversation is worse than it looks: it
/// goes into the terminal's own history, between the question and the answer to
/// it, and stays there for the rest of the session.
///
/// The section headings survive inside the tabs, so the grouping a reader
/// already knows is still what they see.
#[must_use]
pub fn help_pages(t: &Translations) -> Vec<Page> {
    let layout = help_layout();
    let width = layout
        .iter()
        .flat_map(|section| &section.rows)
        .filter(|row| row.key.is_some())
        .map(|row| visible_width(&row.syntax))
        .max()
        .unwrap_or(0);

    // Which heading belongs on which tab. Named rather than counted, so moving
    // a section between tabs is one edit here and nothing else.
    let commands = [
        None,
        Some(keys::slash::sections::SESSIONS),
        Some(keys::slash::sections::MESSAGES),
    ];
    let turn = [Some(keys::slash::sections::CONTEXT)];

    let mut tabs = vec![
        Page {
            title: t.t(keys::slash::tabs::COMMANDS),
            rows: section_rows(&layout, width, t, |heading| commands.contains(&heading)),
        },
        Page {
            title: t.t(keys::slash::tabs::TURN),
            rows: section_rows(&layout, width, t, |heading| turn.contains(&heading)),
        },
        Page {
            title: t.t(keys::slash::tabs::SETUP),
            rows: section_rows(&layout, width, t, |heading| {
                !commands.contains(&heading) && !turn.contains(&heading)
            }),
        },
    ];

    let bindings = key_layout();
    let key_width = bindings
        .iter()
        .map(|(binding, _)| visible_width(binding))
        .max()
        .unwrap_or(0);
    tabs.push(Page {
        title: t.t(keys::slash::sections::KEYS),
        rows: bindings
            .iter()
            .map(|(binding, key)| format!("  {}  {}", pad_to_width(binding, key_width), t.t(key)))
            .collect(),
    });
    tabs
}

/// The rows of every section `wanted` accepts, with a blank row between them.
fn section_rows(
    layout: &[HelpSection],
    width: usize,
    t: &Translations,
    wanted: impl Fn(Option<&'static str>) -> bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for section in layout.iter().filter(|section| wanted(section.heading)) {
        if !out.is_empty() {
            out.push(String::new());
        }
        if let Some(heading) = section.heading {
            out.push(format!("  {}", t.t(heading)));
        }
        for row in &section.rows {
            out.push(match row.key {
                None => format!("  {}", row.syntax),
                Some(key) => format!("  {}  {}", pad_to_width(&row.syntax, width), t.t(key)),
            });
        }
    }
    out
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

        "transcript" => Ok(SlashOutcome::Transcript),

        "help" => {
            // The overlay when there is a screen to lay it over, and the same
            // rows written to the stream when there is not. A pipe, `--json`
            // and a dumb terminal all still get the answer.
            let shown = ctx
                .menu
                .show(ListingRequest {
                    pages: help_pages(ctx.t),
                    labels: PagesLabels {
                        footer: ctx.t.t(keys::slash::help::FOOTER),
                    },
                })
                .await;
            if !shown {
                let text = help_text(ctx.t);
                ctx.renderer.note(&text);
            }
            Ok(SlashOutcome::Continue)
        }

        "clear" => {
            ctx.runtime.store().clear_messages(ctx.session_key)?;
            let note = ctx.t.t(keys::slash::notes::HISTORY_CLEARED);
            ctx.renderer.note(&note);
            Ok(SlashOutcome::Continue)
        }

        // ── Sessions ──────────────────────────────────────────────
        "new" => Ok(new_command(tail, ctx)),
        "session" => session_command(argv, ctx).await,
        "rename" => rename_command(tail, ctx),
        "delete" => delete_command(ctx),
        "branch" => branch_command(ctx),

        // ── Messages ──────────────────────────────────────────────
        "edit" => edit_command(tail, ctx),
        "regenerate" => regenerate_command(ctx),

        // ── Context and cost ──────────────────────────────────────
        "context" => context_command(ctx).await,
        "tasks" => tasks_command(ctx).await,
        "memory" => memory_command(ctx).await,
        "skills" => skills_command(ctx).await,

        // ── Workspaces ────────────────────────────────────────────
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

/// `/session` — this one, another by key, or a picker over all of them.
///
/// One name for one subject. It used to be two: `/sessions` listed and
/// `/session` — this one, another by key, or a picker over all of them.
///
/// One name for one subject. It used to be two: `/sessions` listed and
/// `/session` showed, so the plural and the singular were a guess about which
/// half you wanted rather than a difference in what you were asking about.
/// Bare opens the picker, which is the listing you can act on; a key attaches.
///
/// The same shape `/agent`, `/model` and `/workspace` take.
async fn session_command(argv: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    if let Some(target) = argv.first() {
        // Named, not created: a key nobody has spoken under is a name for a
        // conversation that has not happened, and the turn writes the row.
        return Ok(SlashOutcome::Attach(target.clone()));
    }

    let mut rows = recent_sessions(ctx)?;

    if ctx.menu.available() && !rows.is_empty() {
        loop {
            let Some((target, verb)) = pick_session(ctx.menu, &rows, ctx.session_key, ctx.t).await
            else {
                return Ok(SlashOutcome::Continue);
            };
            match verb {
                // Picked out of the listing, so the row is there by
                // construction.
                None => return Ok(SlashOutcome::Attach(target)),
                Some(SessionVerb::Delete) => {
                    // Deleting the one the prompt is on moves it somewhere and
                    // the window is done. Deleting any other leaves the prompt
                    // where it is, so the list comes back — read again, because
                    // the copy it was drawn from is a row out of date.
                    let outcome = delete_session(&target, ctx).await?;
                    if !matches!(outcome, SlashOutcome::Continue) {
                        return Ok(outcome);
                    }
                    rows = recent_sessions(ctx)?;
                    if rows.is_empty() {
                        return Ok(SlashOutcome::Continue);
                    }
                }
            }
        }
    }

    // No screen to draw a picker on, so the question is answered in prose:
    // where you are, and then what else there is.
    let here = current_session(ctx)?;
    ctx.renderer.note(&here);
    let text = if rows.is_empty() {
        ctx.t.t(keys::slash::notes::NO_SESSIONS)
    } else {
        session_listing(&rows, ctx.session_key)
    };
    ctx.renderer.note(&text);
    Ok(SlashOutcome::Continue)
}

/// The newest conversations in this one's workspace.
fn recent_sessions(ctx: &SlashContext<'_>) -> Result<Vec<SessionSummaryRecord>> {
    let store = ctx.runtime.store();
    let workspace_id = store
        .get_session(ctx.session_key)?
        .map(|session| session.workspace_id);
    store.list_sessions(&ListSessions {
        limit: Some(DEFAULT_SESSION_LINES),
        workspace_id,
        ..ListSessions::default()
    })
}

/// The conversation the prompt is on, as a couple of lines.
fn current_session(ctx: &SlashContext<'_>) -> Result<String> {
    let store = ctx.runtime.store();
    let session = store.get_session(ctx.session_key)?;
    let title = match session.as_ref() {
        Some(session) if !session.title.is_empty() => session.title.clone(),
        _ => "(unnamed)".to_owned(),
    };
    // The session's *own* workspace, not the pending one. They differ after a
    // `/workspace` switch, and showing the pending one here would report where
    // the next conversation lands as though it were where this one is.
    let workspace = session
        .as_ref()
        .map_or_else(|| "—".to_owned(), |session| session.workspace_id.clone());
    let count = store.message_count(ctx.session_key)?;
    Ok(format!(
        "{title}\n  {}  ·  {count} messages  ·  workspace {workspace}",
        ctx.session_key
    ))
}

/// Every conversation as one row, with the current one marked.
fn session_listing(rows: &[SessionSummaryRecord], current: &str) -> String {
    rows.iter()
        .map(|row| {
            let mark = if row.session.key == current { '*' } else { ' ' };
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
        .join("\n")
}

/// `/new [title]` — a key to talk under, and nothing stored yet.
///
/// The row is written by the first turn, not here. A `/new` that created one
/// left an empty untitled session behind every time somebody opened a prompt,
/// changed their mind and closed it, and those are indistinguishable in the
/// listing from conversations that mattered. The turn creates it with the
/// workspace and the agent it actually ran under, which is the same path the
/// browser and Telegram take.
///
/// A title given here is held rather than written, and applied once the row
/// exists. The loop names a session after its first message; a name somebody
/// typed on purpose wins over one derived from what they happened to ask.
fn new_command(tail: &str, ctx: &mut SlashContext<'_>) -> SlashOutcome {
    let key = format!("cli-{}", random_key());
    if tail.is_empty() {
        return SlashOutcome::Attach(key);
    }
    // A title is a decision, and it has to outlive a row that does not exist
    // yet. The loop names a session after its first message, so this is held
    // and applied over that: whoever typed a name meant it.
    *ctx.pending_title = Some(tail.to_owned());
    SlashOutcome::Attach(key)
}

/// `/rename <title>` — the name, on the row if there is one.
///
/// A conversation nobody has spoken in has no row, and naming one must not
/// conjure it: that is the empty session this whole arrangement exists to
/// avoid. The name waits with the one `/new` takes, and the first turn puts
/// both on the row it writes.
fn rename_command(tail: &str, ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    if tail.is_empty() {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            ctx.t.t(keys::slash::errors::USAGE_RENAME),
        ));
    }
    if ctx.runtime.store().get_session(ctx.session_key)?.is_none() {
        *ctx.pending_title = Some(tail.to_owned());
        let note = ctx
            .t
            .tr(keys::slash::notes::RENAMED_TO, args!["title" => tail]);
        ctx.renderer.note(&note);
        return Ok(SlashOutcome::Continue);
    }
    ctx.runtime.store().update_session(
        ctx.session_key,
        UpdateSession {
            title: Some(tail.to_owned()),
            ..UpdateSession::default()
        },
    )?;
    // The row has the name now, so nothing is waiting to give it one.
    *ctx.pending_title = None;
    let note = ctx
        .t
        .tr(keys::slash::notes::RENAMED_TO, args!["title" => tail]);
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

/// Deletes one conversation, having asked. A verb on the row in `/session`.
///
/// The prompt has to be somewhere afterwards, and somewhere is a key rather
/// than a row: deleting a conversation and immediately storing an empty one in
/// its place is the listing filling up with the debris of housekeeping.
async fn delete_session(key: &str, ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    if !confirm(ctx, keys::menu::titles::DELETE_SESSION, key).await {
        return Ok(SlashOutcome::Continue);
    }
    if !ctx.runtime.store().delete_session(key)? {
        return Err(WireError::new(
            ErrorKind::NotFound,
            ctx.t
                .tr(keys::slash::errors::NO_SESSION, args!["key" => key]),
        ));
    }
    let note = ctx.t.tr(keys::slash::notes::DELETED, args!["key" => key]);
    ctx.renderer.note(&note);
    if key != ctx.session_key {
        return Ok(SlashOutcome::Continue);
    }
    Ok(SlashOutcome::Attach(format!("cli-{}", random_key())))
}

/// `/delete` — drop the last exchange.
///
/// **A message, not a session.** It used to take a session key and delete the
/// whole conversation, which put the most destructive thing the prompt can do
/// behind the shortest word — and left the message commands without the one
/// verb the set was missing. Deleting a session is a verb on the row in
/// `/session` now, where the thing being deleted is named and on screen.
///
/// The exchange, not the message: dropping a question and leaving the answer
/// to it is a transcript that reads as though the model volunteered it.
fn delete_command(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let store = ctx.runtime.store();
    let seq = resolve_seq(store, ctx.session_key, None)?;
    let seq = require_user_message(store, ctx.session_key, seq, ctx.t)?;
    // Below the question, which takes the answer with it. History is
    // append-only for the provider's cache, so what this drops is a suffix.
    store.truncate_after(ctx.session_key, seq - 1)?;
    let note = ctx.t.t(keys::slash::notes::DELETED_MESSAGE);
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

/// `/branch` — fork the conversation here and carry on in the fork.
fn branch_command(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let store = ctx.runtime.store();
    let seq = resolve_seq(store, ctx.session_key, None)?;
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

/// `/edit <text>` — replace the last thing you said, and run it again.
///
/// **No message reference any more.** It took a seq number, and the only place
/// to read one was `/messages`, which was a listing of a conversation the
/// terminal is already showing. One command reading a number off another
/// command's output is a workflow, not an interface. A reference to something
/// further back belongs where the message is on screen and can be pointed at.
fn edit_command(tail: &str, ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let text = tail.trim();
    if text.is_empty() {
        return Err(WireError::new(
            ErrorKind::InvalidInput,
            ctx.t.t(keys::slash::errors::USAGE_EDIT),
        ));
    }

    let store = ctx.runtime.store();
    let seq = resolve_seq(store, ctx.session_key, None)?;
    let seq = require_user_message(store, ctx.session_key, seq, ctx.t)?;
    // Below the edited message: the loop appends the replacement itself, so
    // cutting *at* it would leave the old wording above the new one.
    store.truncate_after(ctx.session_key, seq - 1)?;
    Ok(SlashOutcome::Turn(text.to_owned()))
}

/// `/regenerate` — run the last turn again, discarding the answer it gave.
fn regenerate_command(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let store = ctx.runtime.store();
    let seq = resolve_seq(store, ctx.session_key, None)?;
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

    let summary = format_context(&report, ctx.t.locale().as_str());
    let shown = ctx
        .menu
        .show(ListingRequest {
            pages: context_pages(&report, &summary, ctx.t),
            labels: PagesLabels {
                footer: ctx.t.t(keys::slash::help::FOOTER),
            },
        })
        .await;
    if !shown {
        ctx.renderer.note(&summary);
    }
    Ok(SlashOutcome::Continue)
}

/// `/context` as four tabs: the numbers, and the three things behind them.
///
/// The summary is the six lines this command has always printed, unchanged, so
/// a pipe and a terminal agree. The other three exist because the numbers are
/// only ever the start of the question: the one thing anybody asks after
/// "tools: 4,102" is *which* tools, and the report already carries the answer.
/// It used to be measured and thrown away.
fn context_pages(
    report: &darkwire_agent::ContextReport,
    summary: &str,
    t: &Translations,
) -> Vec<Page> {
    let width = columns_or_default(terminal_columns())
        .saturating_sub(OVERLAY_GUTTER)
        .max(20);
    let locale = t.locale();
    let locale = locale.as_str();
    let n = |value: usize| format_number(i64::try_from(value).unwrap_or(i64::MAX), locale);
    let fold = |text: &str| -> Vec<String> {
        text.lines()
            .flat_map(|line| wrap_to_width(line, width))
            .map(|line| format!("  {line}"))
            .collect()
    };

    let mut system = fold(&report.system_prompt);
    if !report.runtime_block.is_empty() {
        system.push(String::new());
        system.push(format!("  {}", t.t(keys::slash::tabs::LIVE)));
        system.push(String::new());
        system.extend(fold(&report.runtime_block));
    }

    let name_width = report
        .tools
        .iter()
        .map(|tool| visible_width(&tool.name))
        .max()
        .unwrap_or(0);
    let tools = report
        .tools
        .iter()
        .map(|tool| {
            // Priced one at a time here and all at once in the breakdown, so
            // these sum to slightly less than the figure on the summary tab:
            // the brackets and separators of the array are billed once and
            // belong to none of the rows.
            let cost = estimate_tool_tokens(std::slice::from_ref(tool));
            let head = tool.description.lines().next().unwrap_or_default();
            format!(
                "  {}  {:>8}  {head}",
                pad_to_width(&tool.name, name_width),
                n(cost)
            )
        })
        .collect();

    let messages = report
        .messages
        .iter()
        .map(|record| {
            let text = text_of(&record.message).replace('\n', " ");
            format!(
                "  {:>5}  {:<9}  {:>8}  {}",
                record.seq,
                role_of(&record.message),
                n(estimate_message_tokens(&record.message)),
                truncate_to_width(text.trim(), width.saturating_sub(28), "…")
            )
        })
        .collect();

    vec![
        Page {
            title: t.t(keys::slash::tabs::SUMMARY),
            rows: summary.lines().map(|line| format!("  {line}")).collect(),
        },
        Page {
            title: t.t(keys::slash::tabs::SYSTEM),
            rows: system,
        },
        Page {
            title: t.t(keys::slash::tabs::TOOLS),
            rows: tools,
        },
        Page {
            title: t.t(keys::slash::tabs::MESSAGES),
            rows: messages,
        },
    ]
}

/// Which side of the conversation a stored message is, for a column.
fn role_of(message: &darkwire_protocol::ChatMessage) -> &'static str {
    match message {
        darkwire_protocol::ChatMessage::System(_) => "system",
        darkwire_protocol::ChatMessage::User(_) => "user",
        darkwire_protocol::ChatMessage::Assistant(_) => "assistant",
        darkwire_protocol::ChatMessage::Tool(_) => "tool",
    }
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

/// `/memory` — what this workspace remembers, and the key that stops it.
///
/// **`/memory on|off` is gone.** The switch lives on the window now, beside the
/// thing it switches: a command spelled out to do what a key does while you are
/// looking at the list is the second name for one thing, which is what
/// `/sessions`, `/workspaces` and `/tasks clear` just lost.
///
/// The switch is the `memory` tool's permission, not a setting of its own, so
/// flipping it reconfigures the agent rather than writing to the session row:
/// the capability belongs to the agent, and a session-scoped override would be
/// a second source of truth for one question.
///
/// There is no verb that folds a conversation into memory. The memory folder
/// holds facts about a workspace, and a summary of one session is not one of
/// those.
async fn memory_command(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let agent_id = memory_agent_id(ctx)?;
    let granted = tool_granted(ctx, &agent_id, "memory");

    let memories = if granted {
        let workspace = workspace_of_session(ctx)?;
        let jail = ctx.runtime.jails().for_workspace(&workspace);
        read_memories(jail.root())
    } else {
        Vec::new()
    };

    // A pipe gets the sentence it always got, and no way to flip the switch:
    // the switch is a key on a window, and there is no window.
    //
    // Nothing remembered gets the sentence too, but only while the tool is on.
    // A window whose one row is "nothing here" is a screenful saying nothing —
    // except when it is off, because then that row is the only way to reach
    // the switch that turns it on. Same rule the skills window follows.
    if !ctx.menu.available() || (granted && memories.is_empty()) {
        ctx.renderer
            .note(&memory_summary(&agent_id, &memories, granted, ctx));
        return Ok(SlashOutcome::Continue);
    }

    let summary = memory_summary(&agent_id, &memories, granted, ctx);
    let mut at = None;
    loop {
        match show_memories(ctx.menu, &memories, &summary, granted, at, ctx.t).await {
            None => return Ok(SlashOutcome::Continue),
            Some(MemoryChoice::Toggle) => return set_tool_permission("memory", !granted, ctx),
            Some(MemoryChoice::Read(row)) => {
                let Some(memory) = memories.get(row) else {
                    return Ok(SlashOutcome::Continue);
                };
                read_document(&memory.key, &memory.content, ctx).await;
                // Back on the row it was read from, rather than at the top.
                at = Some(row);
            }
        }
    }
}

/// One document, over the window, exactly as it is on disk.
///
/// Its own overlay rather than a tab of the list it came from: the list is
/// reminders and this is the thing itself, and laying every file out as a tab
/// would be a tab bar as long as the folder.
///
/// Shared by the memories and the skills windows, which ask the same question
/// of two folders.
async fn read_document(title: &str, body: &str, ctx: &SlashContext<'_>) {
    let width = columns_or_default(terminal_columns())
        .saturating_sub(OVERLAY_GUTTER)
        .max(20);
    let rows = body
        .lines()
        .flat_map(|line| wrap_to_width(line, width))
        .map(|line| format!("  {line}"))
        .collect();
    ctx.menu
        .show(ListingRequest {
            pages: vec![Page {
                title: title.to_owned(),
                rows,
            }],
            labels: PagesLabels {
                footer: ctx.t.t(keys::slash::help::FOOTER),
            },
        })
        .await;
}

/// The count and what the index costs, or why there is neither.
fn memory_summary(
    agent_id: &str,
    memories: &[darkwire_core::memory::Memory],
    granted: bool,
    ctx: &SlashContext<'_>,
) -> String {
    if !granted {
        return ctx
            .t
            .tr(keys::slash::notes::MEMORY_OFF, args!["agent" => agent_id]);
    }
    if memories.is_empty() {
        return ctx.t.tr(
            keys::slash::notes::MEMORY_EMPTY,
            args!["path" => MEMORY_DIRNAME],
        );
    }
    let index = memories
        .iter()
        .map(index_line)
        .collect::<Vec<_>>()
        .join("\n");
    let tokens = format_number(
        i64::try_from(estimate_tokens(&index)).unwrap_or(i64::MAX),
        ctx.t.locale().as_str(),
    );
    // A count and what the index costs, which are the two numbers an operator
    // can act on. The line this replaced measured one file, and there is no one
    // file.
    ctx.t.tr(
        keys::slash::notes::MEMORY_COUNT,
        args![
            "count" => memories.len(),
            "path" => MEMORY_DIRNAME,
            "tokens" => tokens.as_str(),
        ],
    )
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

/// Flips one tool's permission on this conversation's agent.
///
/// **The whole entry is rewritten, not patched.** `agents.list.*` replaces
/// wholesale, so sending the one permission would delete every other permission
/// and every other override this agent holds. The effective map is read back
/// first and written whole.
fn set_tool_permission(tool: &str, on: bool, ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
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
        tool.to_owned(),
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

/// `/skills` — the sheets this workspace holds, and the key that stops them.
///
/// The catalogue is already in the prompt, so this is not what tells the
/// *model* about a sheet — the agent opens one itself when the description says
/// it applies. It is what tells the person which sheets this workspace holds,
/// and lets them read one.
///
/// **Read-only.** There is no `save_skill` or `delete_skill` to call: a skill
/// is a directory a person commits beside the project it describes, and it may
/// hold a checklist or a script beside its `SKILL.md`. Removing one means
/// removing a tree somebody put files in, which is not a keystroke.
async fn skills_command(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let agent_id = memory_agent_id(ctx)?;
    // The same gate the contributor uses, and the same reason: a denial takes
    // the catalogue out of the prompt. Absent counts as denied.
    let granted = tool_granted(ctx, &agent_id, "skill");

    let workspace = workspace_of_session(ctx)?;
    let jail = ctx.runtime.jails().for_workspace(&workspace);
    let skills = read_skills(jail.root());
    let summary = skills_summary(&agent_id, &skills, granted, ctx);

    if !ctx.menu.available() || skills.is_empty() {
        ctx.renderer.note(&if skills.is_empty() {
            summary
        } else {
            skills_listing(&skills, &agent_id, ctx)
        });
        return Ok(SlashOutcome::Continue);
    }

    // Built once: the scope note is the same on every opening, and a sheet is
    // not going to change scope while somebody is reading it.
    let items: Vec<SelectItem<usize>> = skill_items(&skills, &agent_id, &summary)
        .into_iter()
        .enumerate()
        .map(|(row, mut item)| {
            // The first row is the summary, so a sheet's own index is one less.
            let Some(skill) = row.checked_sub(1).and_then(|at| skills.get(at)) else {
                return item;
            };
            if let Some(note) = out_of_scope(skill, &agent_id, ctx.t) {
                item.hint = Some(format!("{}  ·  {note}", skill.description));
            }
            item
        })
        .collect();

    let mut at = None;
    loop {
        match show_skills(ctx.menu, items.clone(), granted, at, ctx.t).await {
            None => return Ok(SlashOutcome::Continue),
            Some(SkillChoice::Toggle) => return set_tool_permission("skill", !granted, ctx),
            Some(SkillChoice::Read(row)) => {
                let Some(skill) = skills.get(row) else {
                    return Ok(SlashOutcome::Continue);
                };
                read_document(&skill.name, &skill.body, ctx).await;
                at = Some(row);
            }
        }
    }
}

/// How many sheets there are, or why there are none to reach.
fn skills_summary(
    agent_id: &str,
    skills: &[Skill],
    granted: bool,
    ctx: &SlashContext<'_>,
) -> String {
    if !granted {
        return ctx
            .t
            .tr(keys::slash::notes::SKILLS_OFF, args!["agent" => agent_id]);
    }
    if skills.is_empty() {
        return ctx.t.tr(
            keys::slash::notes::SKILLS_EMPTY,
            args!["path" => SKILLS_DIRNAME],
        );
    }
    ctx.t.tr(
        keys::slash::notes::SKILLS_COUNT,
        args!["count" => skills.len(), "path" => SKILLS_DIRNAME],
    )
}

/// Every sheet as one row, with the ones out of this agent's catalogue marked.
///
/// Marked rather than hidden. This is what the workspace holds, and somebody
/// running `/skills` because a sheet is not working needs to see it and be told
/// why — not to find it missing from a list too.
fn skills_listing(skills: &[Skill], agent_id: &str, ctx: &SlashContext<'_>) -> String {
    skills
        .iter()
        .map(|skill| {
            let suffix = out_of_scope(skill, agent_id, ctx.t)
                .map_or_else(String::new, |note| format!("  ·  {note}"));
            format!("{}  ·  {}{suffix}", skill.name, skill.description)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Tasks

/// A stored list as the rows a surface draws.
fn plan_rows(tasks: &[darkwire_protocol::tasks::TaskItem]) -> Vec<(TaskStatus, String)> {
    tasks
        .iter()
        .map(|task| (task.status, task.text.clone()))
        .collect()
}

/// `/tasks` — the plan, over the prompt, with a key that empties it.
///
/// The store directly, with no loop involved, for the reason `/context` reaches
/// its primitive: the list is a property of the conversation, and a terminal
/// asking what the plan is must not have to start a turn to find out.
///
/// The markers are the ones the prompt uses rather than the terminal's ticks,
/// because this answers "what does the model see" and the answer should look
/// like it.
///
/// **`/tasks clear` is gone, and so is dropping one task.** The verb lives on
/// the list now: a command spelled out to do what a key in the window does is
/// the second name for one thing that `/session` and `/workspace` just lost.
/// Dropping a single task went with it — the `todo` tool replaces the whole
/// list on its next planning step, so a task removed by hand comes straight
/// back, and a gesture that does not hold is worse than no gesture.
async fn tasks_command(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    let tasks = ctx.runtime.store().tasks(ctx.session_key)?;
    ctx.renderer.plan(&plan_rows(&tasks));

    // An empty plan is a sentence, not a screen. A list drawn over the window
    // to say "nothing matches" is a screenful saying nothing.
    if !ctx.menu.available() || tasks.is_empty() {
        let text = if tasks.is_empty() {
            ctx.t.t(keys::slash::notes::TASKS_EMPTY)
        } else {
            render_tasks(&tasks)
        };
        ctx.renderer.note(&text);
        return Ok(SlashOutcome::Continue);
    }

    if !show_tasks(ctx.menu, &tasks, ctx.t).await {
        return Ok(SlashOutcome::Continue);
    }

    ctx.runtime.store().set_tasks(ctx.session_key, &[])?;
    // The store and the frame both, because the frame's copy is not read from
    // the store on every draw: clearing one and not the other leaves a plan
    // above the composer that nothing is running.
    ctx.renderer.plan(&[]);
    let note = ctx.t.t(keys::slash::notes::TASKS_CLEARED);
    ctx.renderer.note(&note);
    Ok(SlashOutcome::Continue)
}

// Workspaces

/// `/workspace`, and `/workspace <id>`.
///
/// **The verbs are gone from the command.** `new`, `rename`, `rm` and `move`
/// were four spellings of things the manager can do while you are looking at
/// the rows they act on, which is the same duplication `/sessions` and
/// `/workspaces` were. What is left is the window, and an id for a script.
async fn workspace_command(argv: &[String], ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    match argv.first() {
        None => workspace_manager(ctx).await,
        Some(id) => switch_workspace(id, ctx),
    }
}

/// Bare `/workspace` — the manager on a terminal, the listing on a pipe.
///
/// One name for one subject, the way `/session` is. `/workspaces` listed and
/// `/workspace` switched, which put the answer to "what is there" and the way
/// to act on it behind two names; now the listing is the thing you act on.
async fn workspace_manager(ctx: &mut SlashContext<'_>) -> Result<SlashOutcome> {
    if !ctx.menu.available() {
        let pending = pending_workspace(ctx);
        let (workspaces, counts) = workspace_rows(ctx)?;
        ctx.renderer
            .note(&workspace_listing(&workspaces, &counts, &pending));
        return Ok(SlashOutcome::Continue);
    }

    // Re-read and re-open after every verb. The picker layer holds no store on
    // purpose, so a verb closes the window and this applies it; reopening is
    // what makes renaming three workspaces one visit rather than three.
    loop {
        let pending = pending_workspace(ctx);
        let (workspaces, counts) = workspace_rows(ctx)?;
        let Some((row, verb)) =
            manage_workspaces(ctx.menu, &workspaces, &counts, Some(&pending), ctx.t).await
        else {
            return Ok(SlashOutcome::Continue);
        };

        let id = match row {
            WorkspaceRow::New => {
                workspace_new(ctx).await?;
                continue;
            }
            WorkspaceRow::Workspace(id) => id,
        };
        match verb {
            // Switching is the answer to the question the list asks, so it is
            // the one verb that closes it.
            None => return switch_workspace(&id, ctx),
            Some(WorkspaceVerb::Rename) => workspace_rename(&id, &workspaces, ctx).await?,
            // Whether it went or was refused, the list comes back: the loop
            // re-reads it either way, so there is nothing to answer with.
            Some(WorkspaceVerb::Remove) => workspace_remove(&id, ctx).await?,
            Some(WorkspaceVerb::Move) => workspace_move(&id, &workspaces, ctx).await?,
        }
    }
}

/// Where new sessions land, which is what the list marks.
fn pending_workspace(ctx: &SlashContext<'_>) -> String {
    ctx.workspace_id
        .clone()
        .unwrap_or_else(|| DEFAULT_WORKSPACE_ID.to_owned())
}

/// Every workspace and how many sessions are in it, in registry order.
fn workspace_rows(ctx: &SlashContext<'_>) -> Result<(Vec<WorkspaceRecord>, Vec<usize>)> {
    let workspaces = ctx.runtime.workspaces().list()?;
    let mut counts = Vec::with_capacity(workspaces.len());
    for workspace in &workspaces {
        counts.push(ctx.runtime.store().count_by_workspace(&workspace.id)?);
    }
    Ok((workspaces, counts))
}

/// Every workspace as one row, with the one new sessions land in marked.
fn workspace_listing(workspaces: &[WorkspaceRecord], counts: &[usize], current: &str) -> String {
    workspaces
        .iter()
        .enumerate()
        .map(|(at, workspace)| {
            let mark = if workspace.id == current { '*' } else { ' ' };
            let count = counts.get(at).copied().unwrap_or_default();
            format!(
                "{mark} {}  ·  {}  ·  {count} sessions",
                workspace.id, workspace.name
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The name of the workspace `id`, for a question to open on.
fn name_of(workspaces: &[WorkspaceRecord], id: &str) -> String {
    workspaces
        .iter()
        .find(|workspace| workspace.id == id)
        .map_or_else(|| id.to_owned(), |workspace| workspace.name.clone())
}

/// Asks for a name and makes one. A blank answer makes nothing.
async fn workspace_new(ctx: &mut SlashContext<'_>) -> Result<()> {
    let Some(name) = ask_for(ctx, keys::menu::titles::WORKSPACE_NAME, "").await else {
        return Ok(());
    };
    let created = ctx.runtime.workspaces().create(CreateWorkspace {
        name,
        ..CreateWorkspace::default()
    })?;
    let note = ctx.t.tr(
        keys::slash::notes::CREATED,
        args!["id" => created.id.as_str()],
    );
    ctx.renderer.note(&note);
    Ok(())
}

/// Asks for another name and puts it on. Nothing on disk moves.
async fn workspace_rename(
    id: &str,
    workspaces: &[WorkspaceRecord],
    ctx: &mut SlashContext<'_>,
) -> Result<()> {
    // Opened on the name it has: a rename is nearly always a correction to the
    // name being replaced rather than a different one altogether.
    let was = name_of(workspaces, id);
    let Some(name) = ask_for(ctx, keys::menu::titles::WORKSPACE_NAME, &was).await else {
        return Ok(());
    };
    ctx.runtime.workspaces().rename(id, &name)?;
    let note = ctx.t.tr(
        keys::slash::notes::RENAMED_WORKSPACE,
        args!["id" => id, "name" => name.as_str()],
    );
    ctx.renderer.note(&note);
    Ok(())
}

/// Asks, then detaches — and only when nothing still names it.
///
/// The same refusal the web manager makes, and for the same reason: a detached
/// workspace whose conversations still name it would leave them resolving to
/// files nothing lists. The count is on the row, so the refusal is visible
/// before the key is pressed rather than after.
async fn workspace_remove(id: &str, ctx: &mut SlashContext<'_>) -> Result<()> {
    let count = ctx.runtime.store().count_by_workspace(id)?;
    if count > 0 {
        let warning = ctx.t.tr(
            keys::slash::errors::WORKSPACE_IN_USE,
            args!["count" => count, "id" => id],
        );
        ctx.renderer.warn(&warning);
        return Ok(());
    }
    if !confirm(ctx, keys::menu::titles::REMOVE_WORKSPACE, id).await {
        return Ok(());
    }
    ctx.runtime.workspaces().delete(id)?;
    let note = ctx.t.tr(keys::slash::notes::DETACHED, args!["id" => id]);
    ctx.renderer.note(&note);
    if ctx.workspace_id.as_deref() == Some(id) {
        *ctx.workspace_id = None;
    }
    Ok(())
}

/// Asks where to, then sends every session there.
async fn workspace_move(
    from: &str,
    workspaces: &[WorkspaceRecord],
    ctx: &mut SlashContext<'_>,
) -> Result<()> {
    let elsewhere: Vec<WorkspaceRecord> = workspaces
        .iter()
        .filter(|workspace| workspace.id != from)
        .cloned()
        .collect();
    if elsewhere.is_empty() {
        ctx.renderer
            .warn(&ctx.t.t(keys::slash::errors::NOWHERE_TO_MOVE));
        return Ok(());
    }
    let Some(to) = pick_destination(ctx.menu, &elsewhere, ctx.t).await else {
        return Ok(());
    };
    let moved = ctx.runtime.store().reassign_workspace(from, &to)?;
    let note = ctx.t.tr(
        keys::slash::notes::MOVED,
        args!["count" => moved, "from" => from, "to" => to.as_str()],
    );
    ctx.renderer.note(&note);
    Ok(())
}

/// One line typed into the window, trimmed, or nothing for a blank answer.
async fn ask_for(ctx: &SlashContext<'_>, title: &'static str, initial: &str) -> Option<String> {
    let answer = ctx
        .menu
        .ask(AskRequest {
            title: ctx.t.t(title),
            initial: initial.to_owned(),
        })
        .await?;
    let trimmed = answer.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// A yes or a no, as two rows rather than a typed word.
async fn confirm(ctx: &SlashContext<'_>, title: &'static str, id: &str) -> bool {
    let items = vec![
        SelectItem::new(false, &ctx.t.t(keys::menu::NO)),
        SelectItem::new(true, &ctx.t.t(keys::menu::YES)),
    ];
    // Opened on "no". A confirmation whose dangerous answer is one Return away
    // is a confirmation in name only.
    choose_from(
        ctx.menu,
        items,
        &ctx.t.tr(title, args!["id" => id]),
        Some(0),
        ctx.t,
        Placement::Window,
    )
    .await
    .unwrap_or(false)
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
