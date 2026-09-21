//! `src/commands.rs` — the slash commands, driven over a real install.
//!
//! Every case here builds a runtime on a temporary home with the vault, the MCP
//! client and the extension host switched off, so nothing reaches a keychain, a
//! socket or a child process. The renderer writes into a buffer and the menu
//! answers without drawing, which is the state every scripted caller is in.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture helpers in an integration test"
)]

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use darkwire::commands::{
    SlashContext, SlashModels, SlashOutcome, command_rows, command_rows_for, help_text,
    palette_rows, run_slash_command,
};
use darkwire::i18n::Translations;
use darkwire::pickers::palette::{PaletteRow, command_items, command_value, complete_command};
use darkwire::pickers::{MenuAnswer, MenuRequest, NoMenu, PickerMenu};
use darkwire::render::{TurnRenderer, TurnRendererOptions};
use darkwire::runtime::ChatRuntime;
use darkwire_core::session_store::{AppendOptions, CreateSession, UpdateSession};
use darkwire_core::workspace_store::CreateWorkspace;
use darkwire_core::{Database, Result};
use darkwire_protocol::rest::{ModelInfo, ModelsResponse};
use darkwire_protocol::tasks::{TaskItem, TaskStatus};
use darkwire_protocol::{Config, ReasoningEffort, ToolPermission};
use darkwire_runtime::{ExtensionChoice, McpChoice, RuntimeOptions, VaultChoice, create_runtime};
use darkwire_tui::Page;
use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::{Value, json};
use tempfile::TempDir;

// The harness

/// A temporary install, kept alive for the length of a case.
struct Install {
    /// Dropped last, which is what removes the tree.
    _temp: TempDir,
    runtime: ChatRuntime,
}

impl Install {
    /// An install whose `config.yaml` is `config`, or a bare one for `None`.
    fn new(config: Option<&Value>) -> Install {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        std::fs::create_dir_all(root.join("workspace")).unwrap();
        if let Some(config) = config {
            std::fs::write(
                root.join("config.yaml"),
                serde_json::to_string_pretty(config).unwrap(),
            )
            .unwrap();
        }
        let runtime = create_runtime(RuntimeOptions {
            home: Some(root.to_string_lossy().into_owned()),
            workspaces: Some(root.join("workspaces").to_string_lossy().into_owned()),
            env: Some(std::collections::HashMap::new()),
            // Explicit rather than defaulted: the default opens a vault on
            // demand, and opening one writes a key to the OS keychain.
            vault: VaultChoice::None,
            mcp: McpChoice::Off,
            extensions: ExtensionChoice::Off,
            database: Some(Database::in_memory().unwrap()),
            ..RuntimeOptions::default()
        })
        .unwrap();
        Install {
            _temp: temp,
            runtime,
        }
    }

    /// A bare install: no config file at all, which is a fresh machine.
    fn bare() -> Install {
        Install::new(None)
    }

    /// An install with two agents beside the default, which no install has by
    /// default.
    fn with_agents() -> Install {
        Install::new(Some(&json!({
            "agents": { "list": { "reviewer": { "label": "Reviewer" }, "scout": {} } }
        })))
    }

    fn root(&self) -> std::path::PathBuf {
        self.runtime.paths().root
    }

    fn config(&self) -> Config {
        self.runtime.config()
    }

    /// The config as it was written to disk, which is not the same question.
    fn saved(&self) -> Config {
        let text = std::fs::read_to_string(self.root().join("config.yaml")).unwrap();
        serde_yaml_ng::from_str(&text).unwrap()
    }
}

/// The renderer's sink, readable afterwards.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Sink {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }

    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A catalogue that dials nothing.
struct FakeModels {
    response: ModelsResponse,
}

impl FakeModels {
    fn none() -> FakeModels {
        FakeModels {
            response: ModelsResponse {
                models: Vec::new(),
                errors: IndexMap::new(),
            },
        }
    }

    fn with(models: &[(&str, &str)]) -> FakeModels {
        FakeModels {
            response: ModelsResponse {
                models: models
                    .iter()
                    .map(|(id, provider)| ModelInfo {
                        id: (*id).to_owned(),
                        provider_id: (*provider).to_owned(),
                        provider_type: None,
                        display_name: None,
                        context_window_tokens: None,
                        supports_tools: None,
                        supports_vision: None,
                        supports_reasoning: None,
                    })
                    .collect(),
                errors: IndexMap::new(),
            },
        }
    }

    fn failing(models: &[(&str, &str)], id: &str, reason: &str) -> FakeModels {
        let mut built = FakeModels::with(models);
        built
            .response
            .errors
            .insert(id.to_owned(), reason.to_owned());
        built
    }
}

impl SlashModels for FakeModels {
    fn list(&self) -> BoxFuture<'_, Result<ModelsResponse>> {
        Box::pin(std::future::ready(Ok(self.response.clone())))
    }
}

/// A menu that answers with the row whose label it was given, without drawing.
///
/// By label rather than by value, which is what a person choosing from a menu
/// actually does — and it exercises the picker's own row building, which a
/// double that answered a bare value would step straight past.
struct AnsweringMenu {
    label: Option<String>,
    /// Which verb to fire on that row, `None` to simply choose it.
    action: Option<usize>,
    /// How many more times it answers before it starts giving up, so a test of
    /// a window that re-opens after a verb terminates.
    answers: Mutex<usize>,
    seen: Mutex<Vec<Vec<String>>>,
}

impl AnsweringMenu {
    fn choosing(label: &str) -> AnsweringMenu {
        AnsweringMenu {
            label: Some(label.to_owned()),
            action: None,
            answers: Mutex::new(usize::MAX),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Fires verb `action` on that row `times` times, then gives up.
    fn acting(label: &str, action: usize, times: usize) -> AnsweringMenu {
        AnsweringMenu {
            label: Some(label.to_owned()),
            action: Some(action),
            answers: Mutex::new(times),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Available, and cancelled.
    fn cancelled() -> AnsweringMenu {
        AnsweringMenu {
            label: None,
            action: None,
            answers: Mutex::new(usize::MAX),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The labels of every menu that was opened.
    fn offered(&self) -> Vec<Vec<String>> {
        self.seen.lock().unwrap().clone()
    }
}

impl PickerMenu for AnsweringMenu {
    fn available(&self) -> bool {
        true
    }

    fn choose<'a>(
        &'a self,
        request: MenuRequest,
    ) -> Pin<Box<dyn Future<Output = Option<MenuAnswer>> + Send + 'a>> {
        let labels: Vec<String> = request
            .items
            .iter()
            .map(|item| item.label.clone())
            .collect();
        self.seen.lock().unwrap().push(labels);
        let mut left = self.answers.lock().unwrap();
        let answer = if *left == 0 {
            None
        } else {
            *left = left.saturating_sub(1);
            self.label
                .as_ref()
                .and_then(|want| request.items.iter().position(|item| &item.label == want))
                .map(|row| MenuAnswer {
                    row,
                    action: self.action,
                })
        };
        drop(left);
        Box::pin(std::future::ready(answer))
    }

    fn ask<'a>(
        &'a self,
        request: darkwire::pickers::AskRequest,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
        drop(request);
        Box::pin(std::future::ready(None))
    }

    /// Nothing to lay a listing over; a scripted menu has no screen.
    fn show<'a>(
        &'a self,
        request: darkwire::pickers::ListingRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        drop(request);
        Box::pin(std::future::ready(false))
    }
}

/// A menu with a screen, which records the documents laid over it.
///
/// Separate from [`AnsweringMenu`] because the two answer the same trait in
/// opposite ways: that one draws no listing so a case can assert the prose a
/// pipe gets, and this one draws every listing so a case can assert the tabs.
struct ShowingMenu {
    /// A row to choose, for the windows that open a document from one.
    label: Option<String>,
    answers: Mutex<usize>,
    shown: Mutex<Vec<Vec<Page>>>,
}

impl ShowingMenu {
    /// Draws listings and chooses nothing.
    fn reading() -> ShowingMenu {
        ShowingMenu {
            label: None,
            answers: Mutex::new(0),
            shown: Mutex::new(Vec::new()),
        }
    }

    /// Chooses that row once, then gives up, and draws what follows.
    fn opening(label: &str) -> ShowingMenu {
        ShowingMenu {
            label: Some(label.to_owned()),
            answers: Mutex::new(1),
            shown: Mutex::new(Vec::new()),
        }
    }

    /// The tabs of the one listing that was opened, by title.
    fn titles(&self) -> Vec<String> {
        self.shown.lock().unwrap()[0]
            .iter()
            .map(|page| page.title.clone())
            .collect()
    }

    /// The rows of the tab called `title`, trimmed of the indent.
    fn rows(&self, title: &str) -> Vec<String> {
        self.shown.lock().unwrap()[0]
            .iter()
            .find(|page| page.title == title)
            .unwrap_or_else(|| panic!("no tab called {title}"))
            .rows
            .iter()
            .map(|row| row.trim().to_owned())
            .collect()
    }
}

impl PickerMenu for ShowingMenu {
    fn available(&self) -> bool {
        true
    }

    fn choose<'a>(
        &'a self,
        request: MenuRequest,
    ) -> Pin<Box<dyn Future<Output = Option<MenuAnswer>> + Send + 'a>> {
        let mut left = self.answers.lock().unwrap();
        if *left == 0 {
            return Box::pin(std::future::ready(None));
        }
        *left -= 1;
        drop(left);
        let row = self
            .label
            .as_ref()
            .and_then(|want| request.items.iter().position(|item| &item.label == want));
        Box::pin(std::future::ready(
            row.map(|row| MenuAnswer { row, action: None }),
        ))
    }

    fn ask<'a>(
        &'a self,
        request: darkwire::pickers::AskRequest,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
        drop(request);
        Box::pin(std::future::ready(None))
    }

    fn show<'a>(
        &'a self,
        request: darkwire::pickers::ListingRequest,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        self.shown.lock().unwrap().push(request.pages);
        Box::pin(std::future::ready(true))
    }
}

/// A menu that answers a typed question with a line, once.
struct TypingMenu {
    label: String,
    action: usize,
    typed: String,
    answers: Mutex<usize>,
}

impl TypingMenu {
    fn new(label: &str, action: usize, typed: &str) -> TypingMenu {
        TypingMenu {
            label: label.to_owned(),
            action,
            typed: typed.to_owned(),
            answers: Mutex::new(1),
        }
    }
}

impl PickerMenu for TypingMenu {
    fn available(&self) -> bool {
        true
    }

    fn choose<'a>(
        &'a self,
        request: MenuRequest,
    ) -> Pin<Box<dyn Future<Output = Option<MenuAnswer>> + Send + 'a>> {
        let mut left = self.answers.lock().unwrap();
        if *left == 0 {
            return Box::pin(std::future::ready(None));
        }
        *left -= 1;
        drop(left);
        let row = request
            .items
            .iter()
            .position(|item| item.label == self.label);
        Box::pin(std::future::ready(row.map(|row| MenuAnswer {
            row,
            action: Some(self.action),
        })))
    }

    fn ask<'a>(
        &'a self,
        request: darkwire::pickers::AskRequest,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
        drop(request);
        Box::pin(std::future::ready(Some(self.typed.clone())))
    }

    fn show<'a>(
        &'a self,
        request: darkwire::pickers::ListingRequest,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        drop(request);
        Box::pin(std::future::ready(false))
    }
}

/// Everything one case holds, so the context can borrow it field by field.
struct Harness {
    install: Install,
    renderer: TurnRenderer,
    sink: Sink,
    t: Translations,
    session_key: String,
    workspace_id: Option<String>,
    agent_id: Option<String>,
    pending_title: Option<String>,
    model_pinned: bool,
}

impl Harness {
    fn new(install: Install, session_key: &str) -> Harness {
        let sink = Sink::default();
        let renderer = TurnRenderer::new(TurnRendererOptions {
            // Stated rather than detected: a case must not print escape codes
            // into its own assertions because the runner happened to own a tty.
            colors: Some(false),
            ..TurnRendererOptions::new(Box::new(darkwire::render::PlainSink::new(sink.clone())))
        });
        Harness {
            install,
            renderer,
            sink,
            t: Translations::default(),
            session_key: session_key.to_owned(),
            workspace_id: None,
            agent_id: None,
            pending_title: None,
            model_pinned: false,
        }
    }

    fn bare(session_key: &str) -> Harness {
        Harness::new(Install::bare(), session_key)
    }

    fn runtime(&self) -> &ChatRuntime {
        &self.install.runtime
    }

    fn text(&self) -> String {
        self.sink.text()
    }

    /// Runs one command with no menu and no models, which is the scripted state.
    async fn run(&mut self, input: &str) -> SlashOutcome {
        self.run_with(input, &NoMenu, &FakeModels::none()).await
    }

    /// Runs one command with a catalogue.
    async fn run_models(&mut self, input: &str, models: &dyn SlashModels) -> SlashOutcome {
        self.run_with(input, &NoMenu, models).await
    }

    /// Runs one command with a menu.
    async fn run_menu(&mut self, input: &str, menu: &dyn PickerMenu) -> SlashOutcome {
        self.run_with(input, menu, &FakeModels::none()).await
    }

    async fn run_with(
        &mut self,
        input: &str,
        menu: &dyn PickerMenu,
        models: &dyn SlashModels,
    ) -> SlashOutcome {
        let mut ctx = SlashContext {
            renderer: &mut self.renderer,
            runtime: &self.install.runtime,
            t: &self.t,
            session_key: &self.session_key,
            workspace_id: &mut self.workspace_id,
            agent_id: &mut self.agent_id,
            pending_title: &mut self.pending_title,
            menu,
            models,
            model_pinned: self.model_pinned,
        };
        run_slash_command(input, &mut ctx).await
    }
}

/// One message somebody typed.
fn said(text: &str) -> darkwire_protocol::ChatMessage {
    darkwire_protocol::ChatMessage::User(darkwire_core::user_message(text))
}

/// One message the model answered with.
fn answered(text: &str) -> darkwire_protocol::ChatMessage {
    darkwire_protocol::ChatMessage::Assistant(darkwire_core::assistant_message(
        text,
        darkwire_core::messages::AssistantOptions::default(),
    ))
}

/// One agent entry from the live config.
///
/// Only for an agent the config actually names. The default agent is
/// synthesised when no entry declares it, which is what [`resolved`] is for.
fn entry<'a>(config: &'a Config, id: &str) -> &'a darkwire_protocol::config::AgentEntry {
    config.agents.list.get(id).unwrap()
}

/// One agent as the runtime resolved it, entry or no entry.
fn resolved(h: &Harness, id: &str) -> darkwire_runtime::EffectiveAgent {
    h.runtime()
        .agents()
        .into_iter()
        .find(|agent| agent.id == id)
        .unwrap()
}

// help_text

/// Every line of `/help` that is a command row.
fn help_lines(help: &str) -> Vec<String> {
    help.lines()
        .filter(|line| line.trim_start().starts_with('/'))
        .map(str::to_owned)
        .collect()
}

/// Where a row's description begins, when it has one.
fn description_column(line: &str) -> Option<usize> {
    let body = line.strip_prefix("  ")?;
    let gap = body.find("  ")?;
    let after = body[gap..].find(|c: char| c != ' ')? + gap;
    Some(2 + after)
}

#[test]
fn help_lists_every_command_a_reader_can_type() {
    let help = help_text(&Translations::default());
    assert!(help.contains("/session [key]"));
    assert!(help.contains("pick a session to continue, or attach to one by key"));
}

#[test]
fn help_groups_the_commands_under_headings() {
    let help = help_text(&Translations::default());
    for heading in ["sessions", "messages", "context and cost", "workspaces"] {
        assert!(
            help.contains(&format!("\n  {heading}\n")),
            "no heading {heading} in\n{help}"
        );
    }
}

#[test]
fn help_aligns_every_description_in_one_column() {
    // The bug this replaces: the column was a fixed number of spaces typed in
    // by hand, so it held only while every description was English — and the
    // first row was two characters out even then.
    let help = help_text(&Translations::default());
    let columns: BTreeSet<usize> = help_lines(&help)
        .iter()
        .filter_map(|line| description_column(line))
        .collect();
    let described = help_lines(&help)
        .iter()
        .filter(|line| description_column(line).is_some())
        .count();

    assert!(described > 15, "only {described} described rows");
    assert_eq!(columns.len(), 1, "descriptions start at {columns:?}");
}

#[test]
fn help_lists_the_keys_as_well_as_the_commands() {
    // The keys were undiscoverable: two of them existed and nothing in the
    // program said so.
    let help = help_text(&Translations::default());
    for binding in ["ctrl-g", "ctrl-t", "ctrl-o", "ctrl-l", "tab"] {
        assert!(help.contains(binding), "{binding} is not in /help");
    }
}

#[test]
fn a_key_is_never_offered_as_a_command() {
    // `command_rows` is flattened into the palette, into Tab completion and
    // into the list a slash command opens. A Keys *section* of the help layout
    // would have put `ctrl-t` in all three as something to run, which is why
    // the keys are a listing of their own rather than a section.
    for row in command_rows() {
        assert!(
            row.syntax.starts_with('/'),
            "{:?} is not a command",
            row.syntax
        );
    }
    let t = Translations::default();
    for item in command_items(
        &command_rows()
            .iter()
            .map(PaletteRow::from)
            .collect::<Vec<_>>(),
        &t,
    ) {
        assert!(!item.label.contains("ctrl-"), "{:?} is a key", item.label);
    }
}

#[test]
fn help_indents_every_row_the_same_including_the_first() {
    let help = help_text(&Translations::default());
    for line in help_lines(&help) {
        assert!(line.starts_with("  /"), "badly indented: {line:?}");
    }
}

#[test]
fn help_prints_the_syntax_verbatim_never_through_the_bundle() {
    // The left column is what a person types, so it must survive translation
    // untouched. Asserted against the table rather than against a second
    // locale: with one language shipped, a locale comparison would pass for a
    // syntax line that had been translated as well.
    let help = help_text(&Translations::default());
    for row in command_rows() {
        assert!(
            help.contains(&row.syntax),
            "{:?} is in the table and not in the page",
            row.syntax
        );
    }
}

// The table, and the palette that reads it

#[test]
fn an_install_with_no_extension_host_offers_the_table_alone() {
    let install = Install::bare();
    assert_eq!(command_rows_for(&install.runtime), command_rows());
}

#[test]
fn the_palette_reads_the_same_table_the_help_page_does() {
    let install = Install::bare();
    let expected: Vec<PaletteRow> = command_rows().iter().map(PaletteRow::from).collect();
    assert_eq!(palette_rows(&install.runtime), expected);
}

#[test]
fn the_real_rows_round_trip_through_the_palette_and_the_completer() {
    // The half the palette's own suite could not write: it tests the grammar
    // against a fixture, and this tests the grammar against the table that
    // actually ships.
    let t = Translations::default();
    let install = Install::bare();
    let rows = palette_rows(&install.runtime);
    let items = command_items(&rows, &t);

    assert_eq!(items.len(), rows.len());
    for (item, row) in items.iter().zip(&rows) {
        // The label is the syntax, so the list reads as the help page does.
        assert_eq!(item.label, row.syntax);
        // And the value is something that can actually be typed.
        assert!(item.value.command.starts_with('/'), "{:?}", item.value);
        assert!(!item.value.command.contains('<'));
        assert!(!item.value.command.contains('['));
        // A row needing an argument is typed rather than submitted.
        assert_eq!(item.value.submit, !row.syntax.contains('<'));
    }

    // Every distinct command the table holds is reachable by completing its
    // own prefix, and each appears once however many syntax lines describe it.
    let (all, typed) = complete_command("/", &rows);
    assert_eq!(typed, "/");
    for row in &rows {
        assert!(
            all.contains(&command_value(&row.syntax)),
            "{:?} is not completable",
            row.syntax
        );
    }
    let unique: BTreeSet<&String> = all.iter().collect();
    assert_eq!(
        unique.len(),
        all.len(),
        "a command is offered twice: {all:?}"
    );

    // One command, one completion. There were six here: `/workspaces` beside
    // `/workspace`, and four verbs after it. The verbs are buttons in the
    // window now and the plural is gone, so the prefix answers with the one
    // thing it can mean.
    let (candidates, _) = complete_command("/works", &rows);
    assert_eq!(candidates, ["/workspace".to_owned()]);
}

// The dispatcher

#[tokio::test]
async fn exit_and_quit_both_leave() {
    let mut h = Harness::bare("cli:1");
    assert_eq!(h.run("/exit").await, SlashOutcome::Exit);
    assert_eq!(h.run("/quit").await, SlashOutcome::Exit);
}

#[tokio::test]
async fn help_is_printed_rather_than_returned() {
    let mut h = Harness::bare("cli:1");
    assert_eq!(h.run("/help").await, SlashOutcome::Continue);
    assert!(h.text().contains("/session [key]"));
}

#[tokio::test]
async fn a_name_nobody_registered_is_a_warning_not_an_exit() {
    let mut h = Harness::bare("cli:1");
    assert_eq!(h.run("/nonsense").await, SlashOutcome::Continue);
    assert!(h.text().contains("nonsense"));
}

#[tokio::test]
async fn clear_forgets_the_history_and_says_so() {
    let h = Harness::bare("cli:1");
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    let mut h = h;
    assert_eq!(h.run("/clear").await, SlashOutcome::Continue);
    assert!(h.text().contains("history cleared"));
}

// /workspace <id>

#[tokio::test]
async fn workspace_moves_a_conversation_that_exists() {
    let h = Harness::new(Install::bare(), "cli:1");
    h.runtime()
        .workspaces()
        .create(CreateWorkspace {
            name: "Research".to_owned(),
            id: Some("research".to_owned()),
            ..CreateWorkspace::default()
        })
        .unwrap();
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    h.run("/workspace research").await;

    assert_eq!(h.workspace_id.as_deref(), Some("research"));
    assert_eq!(
        h.runtime()
            .store()
            .get_session("cli:1")
            .unwrap()
            .unwrap()
            .workspace_id,
        "research"
    );
}

#[tokio::test]
async fn workspace_does_not_mint_a_row_for_a_session_nobody_has_spoken_in() {
    // Patching a session creates it, so patching an unspoken conversation would
    // put an empty session in every listing as though it were real.
    let h = Harness::new(Install::bare(), "cli:unspoken");
    h.runtime()
        .workspaces()
        .create(CreateWorkspace {
            name: "Research".to_owned(),
            id: Some("research".to_owned()),
            ..CreateWorkspace::default()
        })
        .unwrap();

    let mut h = h;
    h.run("/workspace research").await;

    assert_eq!(h.workspace_id.as_deref(), Some("research"));
    assert!(
        h.runtime()
            .store()
            .get_session("cli:unspoken")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn workspace_refuses_one_that_does_not_exist_without_moving_anything() {
    // Warned rather than propagated: a mistyped command must not end the prompt.
    let h = Harness::new(Install::bare(), "cli:1");
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    assert_eq!(h.run("/workspace nope").await, SlashOutcome::Continue);

    assert!(h.text().contains("nope"));
    assert_eq!(h.workspace_id, None);
    assert_eq!(
        h.runtime()
            .store()
            .get_session("cli:1")
            .unwrap()
            .unwrap()
            .workspace_id,
        "default"
    );
}

// /workspace with no argument

#[tokio::test]
async fn bare_workspace_switches_to_what_the_picker_answered() {
    let h = Harness::new(Install::bare(), "cli:1");
    h.runtime()
        .workspaces()
        .create(CreateWorkspace {
            name: "Research".to_owned(),
            id: Some("research".to_owned()),
            ..CreateWorkspace::default()
        })
        .unwrap();
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    // By the name, which is what the row says; the value behind it is the id.
    h.run_menu("/workspace", &AnsweringMenu::choosing("Research"))
        .await;

    assert_eq!(h.workspace_id.as_deref(), Some("research"));
    assert_eq!(
        h.runtime()
            .store()
            .get_session("cli:1")
            .unwrap()
            .unwrap()
            .workspace_id,
        "research"
    );
}

// /output

// /model

const MODELS: [(&str, &str); 2] = [("qwen3", "ollama"), ("llama3", "ollama")];

#[tokio::test]
async fn model_moves_the_default_agent_onto_the_named_model() {
    let mut h = Harness::bare("cli:1");
    h.run_models("/model qwen3", &FakeModels::with(&MODELS))
        .await;

    assert_eq!(h.runtime().model(), "qwen3");
    assert!(h.text().contains("now runs qwen3"), "{}", h.text());
}

#[tokio::test]
async fn model_writes_the_provider_beside_it_because_the_pair_is_the_setting() {
    // A model and the endpoint that serves it are one setting: writing the
    // model alone leaves the provider naming an instance that never offered it.
    let mut h = Harness::bare("cli:1");
    h.run_models("/model qwen3", &FakeModels::with(&MODELS))
        .await;

    assert_eq!(
        entry(&h.install.config(), "default").settings.provider,
        "ollama"
    );
}

#[tokio::test]
async fn model_saves_so_the_next_run_starts_where_this_one_left_off() {
    // The settings panel writes the same field and saves it, so a model chosen
    // at the prompt has to survive the next launch too.
    let mut h = Harness::bare("cli:1");
    h.run_models("/model qwen3", &FakeModels::with(&MODELS))
        .await;

    assert_eq!(entry(&h.install.saved(), "default").settings.model, "qwen3");
}

#[tokio::test]
async fn model_edits_the_agent_this_session_runs_on_not_the_default() {
    // A model lives on the agent and nowhere else, so the agent the
    // conversation is bound to is the one that has to move.
    let h = Harness::new(
        Install::new(Some(&json!({
            "agents": { "list": { "coder": {
                "label": "Coder", "systemPrompt": "Write code.", "model": "llama3"
            } } }
        }))),
        "cli:1",
    );
    h.runtime()
        .store()
        .update_session(
            "cli:1",
            UpdateSession {
                agent_id: Some(Some("coder".to_owned())),
                ..UpdateSession::default()
            },
        )
        .unwrap();

    let mut h = h;
    h.run_models("/model qwen3", &FakeModels::with(&MODELS))
        .await;

    let config = h.install.config();
    let coder = entry(&config, "coder");
    assert_eq!(coder.settings.model, "qwen3");
    // `agents.list.*` replaces wholesale, so a patch naming `model` alone would
    // have taken these with it.
    assert_eq!(coder.label, "Coder");
    assert_eq!(coder.system_prompt, "Write code.");
    // The default agent did not move. Read off the *resolved* agents rather
    // than the config map: a config that names `coder` alone holds no `default`
    // entry, and the default agent is synthesised on top of it.
    assert_eq!(resolved(&h, "default").settings.model, "");
    assert!(h.text().contains("Coder"), "{}", h.text());
}

#[tokio::test]
async fn model_refuses_one_no_endpoint_offered() {
    // It needs the lookup anyway, for the provider half of the pair.
    let mut h = Harness::bare("cli:1");
    h.run_models("/model nope", &FakeModels::with(&MODELS))
        .await;

    assert!(h.text().contains("No model nope"), "{}", h.text());
    assert_eq!(entry(&h.install.config(), "default").settings.model, "");
}

#[tokio::test]
async fn model_refuses_under_the_flag_rather_than_appearing_to_work() {
    let mut h = Harness::bare("cli:1");
    h.model_pinned = true;
    h.run_models("/model qwen3", &FakeModels::with(&MODELS))
        .await;

    assert!(
        h.text().contains("--model pinned the model"),
        "{}",
        h.text()
    );
    assert_ne!(h.runtime().model(), "qwen3");
}

#[tokio::test]
async fn model_lists_what_the_endpoints_answered_when_there_is_no_menu() {
    let mut h = Harness::bare("cli:1");
    h.run_models("/model", &FakeModels::with(&MODELS)).await;

    assert!(h.text().contains("qwen3"));
    assert!(h.text().contains("ollama"));
}

#[tokio::test]
async fn model_says_which_endpoint_went_quiet_rather_than_showing_a_shorter_list() {
    // A silently shorter list reads as "that model is gone" rather than "that
    // laptop is shut", and those send an operator to different places.
    let mut h = Harness::bare("cli:1");
    let models = FakeModels::failing(&MODELS, "openai", "connect ECONNREFUSED");
    h.run_models("/model", &models).await;

    assert!(h.text().contains("openai did not answer"), "{}", h.text());
    assert!(h.text().contains("ECONNREFUSED"));
}

#[tokio::test]
async fn model_says_so_when_nothing_published_a_list_at_all() {
    let mut h = Harness::bare("cli:1");
    h.run("/model").await;
    assert!(h.text().contains("no endpoint published a model list"));
}

#[tokio::test]
async fn model_opens_a_picker_on_a_terminal() {
    let mut h = Harness::bare("cli:1");
    let menu = AnsweringMenu::choosing("llama3");
    h.run_with("/model", &menu, &FakeModels::with(&MODELS))
        .await;

    assert_eq!(h.runtime().model(), "llama3");
    // And the menu was offered both models, in the catalogue's own order.
    assert_eq!(
        menu.offered(),
        vec![vec!["qwen3".to_owned(), "llama3".to_owned()]]
    );
}

// /effort and /temperature

#[tokio::test]
async fn temperature_reports_what_is_sent_and_that_none_is_a_real_answer() {
    // A temperature is a number in a range, which a list cannot enumerate — so
    // bare `/temperature` answers the question a picker would have answered by
    // opening, rather than opening one.
    let mut h = Harness::bare("cli:1");
    h.run("/temperature").await;
    assert!(h.text().contains("no temperature at all"), "{}", h.text());
}

#[tokio::test]
async fn effort_opens_a_picker_so_the_levels_need_not_be_memorised() {
    let mut h = Harness::bare("cli:1");
    h.run_menu("/effort", &AnsweringMenu::choosing("medium"))
        .await;

    assert_eq!(
        entry(&h.install.config(), "default")
            .settings
            .reasoning_effort,
        Some(ReasoningEffort::Medium)
    );
}

#[tokio::test]
async fn effort_offers_default_as_a_row_like_any_other() {
    let mut h = Harness::bare("cli:1");
    h.run("/effort high").await;

    let menu = AnsweringMenu::choosing("default");
    h.run_menu("/effort", &menu).await;

    assert_eq!(
        entry(&h.install.config(), "default")
            .settings
            .reasoning_effort,
        None
    );
    // `default` is the first row, before every level.
    let offered = menu.offered();
    assert_eq!(offered[0][0], "default");
}

#[tokio::test]
async fn effort_changes_nothing_when_the_picker_is_cancelled() {
    let mut h = Harness::bare("cli:1");
    h.run("/effort high").await;

    h.run_menu("/effort", &AnsweringMenu::cancelled()).await;

    assert_eq!(
        entry(&h.install.config(), "default")
            .settings
            .reasoning_effort,
        Some(ReasoningEffort::High)
    );
}

#[tokio::test]
async fn effort_lists_the_levels_without_a_menu() {
    // So a pipe still gets an answer.
    let mut h = Harness::bare("cli:1");
    h.run("/effort low").await;
    h.sink.clear();

    h.run("/effort").await;

    assert!(h.text().contains("* low"), "{}", h.text());
    assert!(h.text().contains("xhigh"));
    assert_eq!(
        entry(&h.install.config(), "default")
            .settings
            .reasoning_effort,
        Some(ReasoningEffort::Low)
    );
}

#[tokio::test]
async fn effort_sets_and_saves_a_level_the_schema_knows() {
    let mut h = Harness::bare("cli:1");
    h.run("/effort high").await;

    assert_eq!(
        entry(&h.install.config(), "default")
            .settings
            .reasoning_effort,
        Some(ReasoningEffort::High)
    );
    assert!(h.text().contains("effort high"), "{}", h.text());
    assert_eq!(
        entry(&h.install.saved(), "default")
            .settings
            .reasoning_effort,
        Some(ReasoningEffort::High)
    );
}

#[tokio::test]
async fn effort_keeps_off_apart_from_cleared() {
    // `off` sends a parameter asking for none; cleared sends no parameter at
    // all, which is the only thing that works against an endpoint that rejects
    // the field outright.
    let mut h = Harness::bare("cli:1");

    h.run("/effort off").await;
    assert_eq!(
        entry(&h.install.config(), "default")
            .settings
            .reasoning_effort,
        Some(ReasoningEffort::Off)
    );

    h.run("/effort default").await;
    assert_eq!(
        entry(&h.install.config(), "default")
            .settings
            .reasoning_effort,
        None
    );
}

#[tokio::test]
async fn effort_refuses_a_level_nothing_spells_naming_the_ones_that_exist() {
    let mut h = Harness::bare("cli:1");
    h.run("/effort maximum").await;

    assert!(h.text().contains("minimal"), "{}", h.text());
    assert_eq!(
        entry(&h.install.config(), "default")
            .settings
            .reasoning_effort,
        None
    );
}

#[tokio::test]
async fn temperature_accepts_one_in_range_zero_included() {
    // `0` is a value, not an absence.
    let mut h = Harness::bare("cli:1");
    h.run("/temperature 0").await;

    assert_eq!(
        entry(&h.install.config(), "default").settings.temperature,
        Some(0.0)
    );
}

#[tokio::test]
async fn temperature_refuses_what_it_cannot_read_rather_than_guessing() {
    // `0.5abc` is the one a leading-number parse would have read as `0.5`.
    for raw in ["-1", "2.5", "warm", "0.5abc"] {
        let mut h = Harness::bare("cli:1");
        h.run(&format!("/temperature {raw}")).await;

        assert!(
            h.text().contains("usage: /temperature"),
            "{raw}: {}",
            h.text()
        );
        assert_eq!(
            entry(&h.install.config(), "default").settings.temperature,
            None,
            "{raw} was accepted"
        );
    }
}

#[tokio::test]
async fn temperature_edits_the_agent_this_session_runs_on_keeping_the_rest() {
    let h = Harness::new(
        Install::new(Some(&json!({
            "agents": { "list": { "coder": {
                "label": "Coder", "systemPrompt": "Write code.", "model": "llama3"
            } } }
        }))),
        "cli:1",
    );
    h.runtime()
        .store()
        .update_session(
            "cli:1",
            UpdateSession {
                agent_id: Some(Some("coder".to_owned())),
                ..UpdateSession::default()
            },
        )
        .unwrap();

    let mut h = h;
    h.run("/temperature 0.2").await;

    let config = h.install.config();
    assert_eq!(entry(&config, "coder").settings.temperature, Some(0.2));
    assert_eq!(entry(&config, "coder").system_prompt, "Write code.");
    assert_eq!(resolved(&h, "default").settings.temperature, None);
}

// /agent

#[tokio::test]
async fn agent_moves_a_conversation_that_exists_onto_the_named_one() {
    let h = Harness::new(Install::with_agents(), "cli:1");
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    h.run("/agent reviewer").await;

    assert_eq!(h.agent_id.as_deref(), Some("reviewer"));
    assert_eq!(
        h.runtime()
            .store()
            .get_session("cli:1")
            .unwrap()
            .unwrap()
            .agent_id
            .as_deref(),
        Some("reviewer")
    );
    assert!(h.text().contains("now runs on reviewer"), "{}", h.text());
}

#[tokio::test]
async fn agent_records_a_preference_without_minting_a_row() {
    // The same guard `/workspace` needs and for the same reason: patching an
    // unspoken conversation would create it and put an empty session in every
    // listing.
    let mut h = Harness::new(Install::with_agents(), "cli:unspoken");
    h.run("/agent reviewer").await;

    assert_eq!(h.agent_id.as_deref(), Some("reviewer"));
    assert!(
        h.runtime()
            .store()
            .get_session("cli:unspoken")
            .unwrap()
            .is_none()
    );
    assert!(
        h.text().contains("next session runs on reviewer"),
        "{}",
        h.text()
    );
}

#[tokio::test]
async fn agent_refuses_one_that_does_not_exist_without_moving_anything() {
    let h = Harness::new(Install::with_agents(), "cli:1");
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    h.run("/agent nope").await;

    assert!(h.text().contains("nope"));
    assert_eq!(h.agent_id, None);
    assert_eq!(
        h.runtime()
            .store()
            .get_session("cli:1")
            .unwrap()
            .unwrap()
            .agent_id,
        None
    );
}

#[tokio::test]
async fn agent_lists_them_marking_the_current_one_when_there_is_no_menu() {
    let h = Harness::new(Install::with_agents(), "cli:1");
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    h.run("/agent reviewer").await;
    h.sink.clear();

    h.run("/agent").await;

    assert!(h.text().contains("* reviewer"), "{}", h.text());
    assert!(h.text().contains("  scout"), "{}", h.text());
    // Listing is not choosing: nothing moved.
    assert_eq!(h.agent_id.as_deref(), Some("reviewer"));
}

#[tokio::test]
async fn agent_names_one_with_no_label_of_its_own_by_its_id() {
    // A resolved agent's label is documented as never empty; `scout` has none
    // in the config, so this is the fallback doing its job.
    let mut h = Harness::new(Install::with_agents(), "cli:1");
    h.run("/agent").await;

    assert!(h.text().contains("scout  ·  scout"), "{}", h.text());
}

#[tokio::test]
async fn agent_opens_a_picker_on_a_terminal() {
    let h = Harness::new(Install::with_agents(), "cli:1");
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    h.run_menu("/agent", &AnsweringMenu::choosing("Reviewer"))
        .await;

    assert_eq!(h.agent_id.as_deref(), Some("reviewer"));
}

// /memory

/// A memory file, as the reader expects to find one.
///
/// A heading and a body, with no frontmatter: the file *is* the memory and its
/// title is its first heading. The fixture this replaced still carried the
/// frontmatter of the shape before that, which made every title read `---`.
fn memory(h: &Harness, name: &str) {
    let dir = h.runtime().jail().root().join("memory");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.md")),
        format!("# About {name}\n\nBody.\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn memory_says_the_tool_is_not_granted_on_an_install_that_predates_it() {
    // The migration case: the default permissions seed a *new* agent, so an
    // existing config has no `memory` key and the answer must say so rather
    // than reporting an empty memory.
    let mut h = Harness::new(
        Install::new(Some(&json!({
            "agents": { "list": { "default": { "tools": { "read": "allow" } } } }
        }))),
        "cli:1",
    );
    h.run("/memory").await;

    assert!(
        h.text().contains("does not have the memory tool"),
        "{}",
        h.text()
    );
}

#[tokio::test]
async fn memory_reports_an_empty_one_once_the_tool_is_granted() {
    let mut h = Harness::bare("cli:1");
    h.run("/memory").await;
    assert!(h.text().contains("nothing remembered yet"), "{}", h.text());
}

#[tokio::test]
async fn memory_counts_them_and_says_what_their_index_costs() {
    let h = Harness::bare("cli:1");
    memory(&h, "rem-over-px");
    memory(&h, "ci-gate");

    let mut h = h;
    h.run("/memory").await;

    assert!(h.text().contains("2 memories"), "{}", h.text());
    assert!(h.text().contains("tokens"));
}

#[tokio::test]
async fn memory_mints_no_session_row_for_a_conversation_that_never_spoke() {
    // A command that wrote to the session row here would put an empty
    // conversation in the web sidebar.
    let mut h = Harness::bare("cli:never-spoken");

    h.run("/memory").await;
    h.run("/memory off").await;

    assert!(
        h.runtime()
            .store()
            .get_session("cli:never-spoken")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn memory_keeps_every_other_permission_when_it_flips_this_one() {
    // `agents.list.*` replaces wholesale, so a patch carrying only the one
    // permission would delete the rest of the map. This is the most likely way
    // for this command to be wrong.
    let mut h = Harness::new(
        Install::new(Some(&json!({
            "agents": { "list": { "default": {
                "label": "Primary",
                "tools": { "read": "allow", "exec": "deny", "memory": "allow" }
            } } }
        }))),
        "cli:1",
    );
    memory(&h, "ci-gate");

    h.run_menu(
        "/memory",
        &AnsweringMenu::acting("ci-gate: About ci-gate", 0, 1),
    )
    .await;

    let config = h.install.config();
    let default = entry(&config, "default");
    assert_eq!(default.tools.get("read"), Some(&ToolPermission::Allow));
    assert_eq!(default.tools.get("exec"), Some(&ToolPermission::Deny));
    assert_eq!(default.tools.get("memory"), Some(&ToolPermission::Deny));
    // And nothing else on the entry was dropped either.
    assert_eq!(default.label, "Primary");
}

// /skills

/// A sheet on disk, as the reader expects to find one.
fn sheet(h: &Harness, name: &str, description: &str, agents: Option<&str>) {
    let dir = h.runtime().jail().root().join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let scope = agents.map_or_else(String::new, |agents| format!("agents: {agents}\n"));
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\ndescription: {description}\n{scope}---\n\nBody of {name}.\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn skills_says_the_tool_is_not_granted_on_an_install_that_predates_it() {
    // The same gate the contributor uses: a denial takes the catalogue out of
    // the prompt, so listing sheets would be listing what this agent cannot
    // open. An absent key counts as denied, which is what an upgrade looks like.
    let mut h = Harness::new(
        Install::new(Some(&json!({
            "agents": { "list": { "default": { "tools": { "read": "allow" } } } }
        }))),
        "cli:1",
    );
    h.run("/skills").await;

    assert!(
        h.text().contains("does not have the skill tool"),
        "{}",
        h.text()
    );
}

#[tokio::test]
async fn skills_says_so_when_the_workspace_holds_none() {
    let mut h = Harness::bare("cli:1");
    h.run("/skills").await;
    assert!(h.text().contains("no skills yet"), "{}", h.text());
}

#[tokio::test]
async fn skills_lists_every_sheet_with_its_description() {
    let h = Harness::bare("cli:1");
    sheet(&h, "deploy", "Ship a release.", None);
    sheet(&h, "code-review", "Review a diff.", None);

    let mut h = h;
    h.run("/skills").await;

    assert!(h.text().contains("deploy"));
    assert!(h.text().contains("Ship a release."));
    assert!(h.text().contains("code-review"));
    assert!(h.text().contains("Review a diff."));
}

#[tokio::test]
async fn skills_lists_one_scoped_to_another_agent_and_marks_it() {
    // Marked rather than dropped. Somebody runs `/skills` precisely when a
    // sheet is not working, and a listing that hides it leaves nowhere to find
    // out why.
    let h = Harness::bare("cli:1");
    sheet(&h, "deploy", "Ship a release.", None);
    sheet(&h, "triage", "Sort the inbox.", Some("lead"));

    let mut h = h;
    h.run("/skills").await;

    assert!(h.text().contains("triage"));
    assert!(
        h.text().contains("for lead only, not this agent"),
        "{}",
        h.text()
    );
    // And the unscoped one carries no marking at all.
    assert!(
        h.text().contains("deploy  ·  Ship a release.\n"),
        "{}",
        h.text()
    );
}

#[tokio::test]
async fn skills_prints_a_name_and_nothing_to_type_it_into() {
    // The listing is informational: it says which sheets exist so a person can
    // ask for one in words. A row carrying syntax would be teaching a way to
    // invoke a skill that does not exist — the agent opens the file itself.
    let h = Harness::bare("cli:1");
    sheet(&h, "deploy", "Ship a release.", None);

    let mut h = h;
    h.run("/skills").await;

    assert!(!h.text().contains('@'));
}

#[tokio::test]
async fn skills_mints_no_session_row_for_a_conversation_that_never_spoke() {
    // It reads the session to find the workspace, and a read that wrote would
    // put an empty conversation in the web sidebar.
    let mut h = Harness::bare("cli:never-spoken");
    h.run("/skills").await;

    assert!(
        h.runtime()
            .store()
            .get_session("cli:never-spoken")
            .unwrap()
            .is_none()
    );
}

// Sessions

#[tokio::test]
async fn new_attaches_to_a_fresh_conversation_without_storing_one() {
    // A key to talk under, and nothing written. Pressing new, changing your
    // mind and leaving must put nothing in the listing, where an empty row is
    // indistinguishable from a conversation that mattered.
    let mut h = Harness::bare("cli:1");
    let outcome = h.run("/new Notes on the build").await;

    let SlashOutcome::Attach(key) = outcome else {
        panic!("expected an attach, got {outcome:?}");
    };
    assert!(key.starts_with("cli-"));
    assert!(
        h.runtime().store().get_session(&key).unwrap().is_none(),
        "an empty conversation was stored"
    );
    // The name waits for the row the first turn writes.
    assert_eq!(h.pending_title.as_deref(), Some("Notes on the build"));
}

#[tokio::test]
async fn new_without_a_name_leaves_nothing_waiting() {
    let mut h = Harness::bare("cli:1");
    h.run("/new").await;
    assert_eq!(h.pending_title, None);
}

#[tokio::test]
async fn renaming_a_conversation_nobody_has_spoken_in_does_not_conjure_one() {
    // The same rule from the other direction: naming a session is not saying
    // something in it.
    let mut h = Harness::bare("cli:1");
    h.run("/rename Build notes").await;

    assert!(h.runtime().store().get_session("cli:1").unwrap().is_none());
    assert_eq!(h.pending_title.as_deref(), Some("Build notes"));
    assert!(h.text().contains("Build notes"), "{}", h.text());
}

#[tokio::test]
async fn new_lands_in_the_pending_workspace() {
    let h = Harness::new(Install::bare(), "cli:1");
    h.runtime()
        .workspaces()
        .create(CreateWorkspace {
            name: "Research".to_owned(),
            id: Some("research".to_owned()),
            ..CreateWorkspace::default()
        })
        .unwrap();

    let mut h = h;
    h.run("/workspace research").await;
    let SlashOutcome::Attach(key) = h.run("/new").await else {
        panic!("expected an attach");
    };

    // Nothing is written here, so the workspace is not on a row yet: it is on
    // the prompt, and the turn that writes the row is what carries it there.
    // `a_session_created_by_a_turn_lands_in_the_workspace_it_was_given` is the
    // other end of that.
    assert!(h.runtime().store().get_session(&key).unwrap().is_none());
    assert_eq!(h.workspace_id.as_deref(), Some("research"));
}

#[tokio::test]
async fn session_with_a_key_attaches_and_without_one_describes() {
    let mut h = Harness::bare("cli:1");
    assert_eq!(
        h.run("/session cli:other").await,
        SlashOutcome::Attach("cli:other".to_owned())
    );

    h.sink.clear();
    h.run("/session").await;
    assert!(h.text().contains("(unnamed)"), "{}", h.text());
    assert!(h.text().contains("cli:1"));
}

#[tokio::test]
async fn rename_with_no_title_says_how_to_use_it() {
    let mut h = Harness::bare("cli:1");
    h.run("/rename").await;
    assert!(h.text().contains("usage: /rename"), "{}", h.text());
}

#[tokio::test]
async fn rename_names_the_conversation() {
    let h = Harness::bare("cli:1");
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    h.run("/rename The build is red").await;

    assert_eq!(
        h.runtime()
            .store()
            .get_session("cli:1")
            .unwrap()
            .unwrap()
            .title,
        "The build is red"
    );
    assert!(h.text().contains("The build is red"));
}

#[tokio::test]
async fn session_says_so_when_there_are_none() {
    let mut h = Harness::bare("cli:1");
    h.run("/session").await;
    assert!(h.text().contains("no sessions yet"), "{}", h.text());
}

#[tokio::test]
async fn session_lists_them_with_the_current_one_marked() {
    let h = Harness::bare("cli:1");
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    h.run("/session").await;

    assert!(h.text().contains("* (unnamed)"), "{}", h.text());
    assert!(h.text().contains("cli:1"));
}

// /edit and /regenerate

#[tokio::test]
async fn edit_without_a_replacement_says_how_to_use_it() {
    let mut h = Harness::bare("cli:1");
    h.run("/edit").await;
    assert!(h.text().contains("usage: /edit"), "{}", h.text());
}

#[tokio::test]
async fn regenerate_with_nothing_said_is_a_refusal_rather_than_a_turn() {
    let mut h = Harness::bare("cli:1");
    let outcome = h.run("/regenerate").await;
    assert_eq!(outcome, SlashOutcome::Continue);
    assert!(!h.text().is_empty(), "a refusal has to say something");
}

#[tokio::test]
async fn edit_hands_the_replacement_back_rather_than_running_it() {
    // One turn-running code path in the terminal, so a re-run inherits the
    // renderer, the cancellation and the exit codes rather than a second copy.
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", said("what is here?"), &AppendOptions::default())
        .unwrap();

    let mut h = h;
    let outcome = h.run("/edit what changed here?").await;

    assert_eq!(outcome, SlashOutcome::Turn("what changed here?".to_owned()));
    // Cut below the edited message: the loop appends the replacement itself.
    assert_eq!(h.runtime().store().message_count("cli:1").unwrap(), 0);
}

#[tokio::test]
async fn regenerate_hands_the_original_back_and_truncates_below_it() {
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", said("say it again"), &AppendOptions::default())
        .unwrap();

    let mut h = h;
    let outcome = h.run("/regenerate").await;

    assert_eq!(outcome, SlashOutcome::Turn("say it again".to_owned()));
    assert_eq!(h.runtime().store().message_count("cli:1").unwrap(), 0);
}

#[tokio::test]
async fn edit_says_so_when_there_is_nothing_of_yours_to_edit() {
    // The refusal moved. It used to be "message 2 is not one of yours", which
    // you could only reach by naming a seq off `/messages`; without a
    // reference the only message `/edit` can address is your own last one, so
    // the case that remains is having said nothing at all.
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", answered("I said this."), &AppendOptions::default())
        .unwrap();

    let mut h = h;
    h.run("/edit no you did not").await;

    assert!(h.text().contains("not said anything"), "{}", h.text());
    // And nothing was cut.
    assert_eq!(h.runtime().store().message_count("cli:1").unwrap(), 1);
}

// /branch

#[tokio::test]
async fn branch_forks_and_attaches_to_the_fork() {
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", said("first"), &AppendOptions::default())
        .unwrap();

    let mut h = h;
    let outcome = h.run("/branch").await;

    let SlashOutcome::Attach(key) = outcome else {
        panic!("expected an attach, got {outcome:?}");
    };
    assert_ne!(key, "cli:1");
    assert!(h.text().contains("branched at"), "{}", h.text());
    // The source is untouched.
    assert_eq!(h.runtime().store().message_count("cli:1").unwrap(), 1);
}

// /workspaces

// /messages, /sessions, /session, /delete

#[tokio::test]
async fn session_prints_the_listing_where_there_is_no_menu_to_open() {
    // A pipe cannot draw one, and the listing is what a script reads.
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", said("hello"), &AppendOptions::default())
        .unwrap();

    let mut h = h;
    h.run("/session").await;

    let text = h.text();
    assert!(text.contains("* "), "the current one is marked: {text}");
    assert!(text.contains("cli:1"), "{text}");
    assert!(
        text.contains("(unnamed)"),
        "a session nothing has named says so: {text}"
    );
}

#[tokio::test]
async fn session_attaches_to_what_the_picker_answered() {
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    for key in ["cli:1", "cli:2"] {
        store.ensure_session(key, CreateSession::default()).unwrap();
        store
            .append(key, said("hello"), &AppendOptions::default())
            .unwrap();
    }
    store
        .update_session(
            "cli:2",
            UpdateSession {
                title: Some("The other one".to_owned()),
                ..UpdateSession::default()
            },
        )
        .unwrap();

    let mut h = h;
    let menu = AnsweringMenu::choosing("The other one");
    let outcome = h.run_menu("/session", &menu).await;

    assert_eq!(outcome, SlashOutcome::Attach("cli:2".to_owned()));
}

#[tokio::test]
async fn session_stays_put_when_the_picker_was_cancelled() {
    // Escape leaves the conversation where it was rather than picking the row
    // the cursor happened to rest on.
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", said("hello"), &AppendOptions::default())
        .unwrap();

    let mut h = h;
    let outcome = h.run_menu("/sessions", &AnsweringMenu::cancelled()).await;

    assert_eq!(outcome, SlashOutcome::Continue);
}

#[tokio::test]
async fn session_with_no_argument_reports_where_this_conversation_is() {
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", said("hello"), &AppendOptions::default())
        .unwrap();
    store
        .update_session(
            "cli:1",
            UpdateSession {
                title: Some("Porting the CLI".to_owned()),
                ..UpdateSession::default()
            },
        )
        .unwrap();

    let mut h = h;
    h.run("/session").await;

    let text = h.text();
    assert!(text.contains("Porting the CLI"), "{text}");
    assert!(text.contains("cli:1"), "{text}");
    assert!(text.contains("1 messages"), "{text}");
    assert!(text.contains("workspace default"), "{text}");
}

#[tokio::test]
async fn session_says_unnamed_and_no_workspace_for_a_conversation_nobody_has_spoken_in() {
    // The row does not exist yet, and minting one to answer a question about it
    // would put an empty session in every listing.
    let mut h = Harness::bare("cli:unspoken");
    h.run("/session").await;

    let text = h.text();
    assert!(text.contains("(unnamed)"), "{text}");
    assert!(text.contains('—'), "{text}");
}

#[tokio::test]
async fn session_with_an_argument_attaches_to_it() {
    let mut h = Harness::bare("cli:1");
    let outcome = h.run("/session cli:other").await;

    assert_eq!(outcome, SlashOutcome::Attach("cli:other".to_owned()));
    // Named rather than created. A key nobody has spoken under is a name for a
    // conversation that has not happened, and the first turn is what writes it.
    assert!(
        h.runtime()
            .store()
            .get_session("cli:other")
            .unwrap()
            .is_none()
    );
}

// /workspace rm, move, rename

// /stats and /context

#[tokio::test]
async fn stats_says_so_before_a_single_turn_has_run() {
    let mut h = Harness::bare("cli:1");
    h.run("/stats").await;

    assert!(
        !h.text().is_empty(),
        "an empty session has to say something"
    );
}

#[tokio::test]
async fn context_refuses_where_no_provider_could_be_resolved() {
    // The report is built from the loop's own prompt, so a bare install has
    // nothing to measure and says so rather than reporting zero.
    let mut h = Harness::bare("cli:1");
    h.run("/context").await;

    assert!(!h.text().is_empty(), "a refusal has to say something");
}

#[tokio::test]
async fn context_breaks_the_window_down_by_where_the_tokens_went() {
    // The breakdown is the whole value: "you are near the limit" is not
    // actionable, and "the tool definitions are half of it" is.
    let h = Harness::new(
        Install::new(Some(&json!({
            "providers": {"local": {"type": "ollama"}},
            "agents": {"list": {"default": {"provider": "local", "model": "qwen3:8b"}}},
        }))),
        "cli:1",
    );
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", said("what is here?"), &AppendOptions::default())
        .unwrap();

    let mut h = h;
    h.run("/context").await;

    let text = h.text();
    for label in ["system", "tools", "messages", "in window"] {
        assert!(text.contains(label), "{label} is missing from {text}");
    }
    assert!(text.contains('%'), "the share of the window: {text}");
}

// /tasks

#[tokio::test]
async fn tasks_says_there_is_no_plan_when_nothing_has_written_one() {
    let mut h = Harness::bare("cli:1");
    h.run("/tasks").await;
    assert!(h.text().contains("no plan"), "{}", h.text());
}

#[tokio::test]
async fn tasks_prints_the_plan_in_the_markers_the_prompt_uses() {
    let h = Harness::bare("cli:1");
    h.runtime()
        .store()
        .set_tasks(
            "cli:1",
            &[
                TaskItem {
                    text: "Inspect auth".to_owned(),
                    status: TaskStatus::Done,
                },
                TaskItem {
                    text: "Update sessions".to_owned(),
                    status: TaskStatus::Doing,
                },
            ],
        )
        .unwrap();

    let mut h = h;
    h.run("/tasks").await;

    let text = h.text();
    assert!(text.contains("[x] Inspect auth"), "{text}");
    assert!(text.contains("[>] Update sessions"), "{text}");
}

/// One session's plan is not another's, which is what keeps a subagent's list
/// out of its parent's.
#[tokio::test]
async fn tasks_reads_the_session_the_prompt_is_attached_to() {
    let h = Harness::bare("cli:1");
    h.runtime()
        .store()
        .set_tasks(
            "cli:2",
            &[TaskItem {
                text: "somebody else's".to_owned(),
                status: TaskStatus::Todo,
            }],
        )
        .unwrap();

    let mut h = h;
    h.run("/tasks").await;
    assert!(h.text().contains("no plan"), "{}", h.text());
}

#[test]
fn the_help_tabs_carry_every_row_the_text_does() {
    // Two renderings of one table, and the tabbed one is what a terminal shows.
    // A command that reached only the text would be a command nobody at a
    // prompt can find.
    let t = Translations::default();
    let text = darkwire::commands::help_text(&t);
    let pages = darkwire::commands::help_pages(&t);
    let tabbed: String = pages
        .iter()
        .flat_map(|page| page.rows.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");

    for line in text.lines() {
        let Some(syntax) = line.trim().split("  ").next() else {
            continue;
        };
        if !syntax.starts_with('/') {
            continue;
        }
        assert!(tabbed.contains(syntax), "the tabs are missing {syntax}");
    }
}

#[test]
fn every_help_tab_has_a_name_and_some_rows() {
    let pages = darkwire::commands::help_pages(&Translations::default());
    assert_eq!(pages.len(), 4);
    for page in &pages {
        assert!(!page.title.is_empty());
        assert!(!page.rows.is_empty(), "{} is empty", page.title);
    }
}

#[test]
fn the_keys_tab_lists_the_bindings_and_no_commands() {
    // A key is never typed as a command, which is why they were kept out of the
    // table the palette and Tab completion read from.
    let pages = darkwire::commands::help_pages(&Translations::default());
    let keys = pages.last().expect("there is a keys tab");
    assert!(keys.rows.iter().any(|row| row.contains("ctrl-g")));
    assert!(!keys.rows.iter().any(|row| row.trim().starts_with('/')));
}

// The windows

#[tokio::test]
async fn the_task_window_empties_the_plan_on_its_verb() {
    // `/tasks clear` is gone: a command spelled out to do what a key in the
    // window does is the second name for one thing.
    let h = Harness::bare("cli:1");
    h.runtime()
        .store()
        .set_tasks(
            "cli:1",
            &[TaskItem {
                text: "Inspect auth".to_owned(),
                status: TaskStatus::Done,
            }],
        )
        .unwrap();

    let mut h = h;
    h.run_menu("/tasks", &AnsweringMenu::acting("[x] Inspect auth", 0, 1))
        .await;

    assert!(h.runtime().store().tasks("cli:1").unwrap().is_empty());
    assert!(h.text().contains("task list cleared"), "{}", h.text());
}

#[tokio::test]
async fn a_plan_read_and_closed_is_left_alone() {
    let h = Harness::bare("cli:1");
    h.runtime()
        .store()
        .set_tasks(
            "cli:1",
            &[TaskItem {
                text: "Inspect auth".to_owned(),
                status: TaskStatus::Done,
            }],
        )
        .unwrap();

    let mut h = h;
    h.run_menu("/tasks", &AnsweringMenu::cancelled()).await;

    assert_eq!(h.runtime().store().tasks("cli:1").unwrap().len(), 1);
}

#[tokio::test]
async fn an_empty_plan_opens_no_window_at_all() {
    // A list drawn over the window to say "nothing matches" is a screenful
    // saying nothing. The sentence is the better answer.
    let mut h = Harness::bare("cli:1");
    let menu = AnsweringMenu::cancelled();
    h.run_menu("/tasks", &menu).await;

    assert!(menu.offered().is_empty(), "{:?}", menu.offered());
    assert!(h.text().contains("no plan"), "{}", h.text());
}

#[tokio::test]
async fn context_lays_the_measurement_out_in_tabs() {
    let h = Harness::new(
        Install::new(Some(&json!({
            "providers": {"local": {"type": "ollama"}},
            "agents": {"list": {"default": {"provider": "local", "model": "qwen3:8b"}}},
        }))),
        "cli:1",
    );
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append(
            "cli:1",
            said("what changed today"),
            &AppendOptions::default(),
        )
        .unwrap();

    let mut h = h;
    let menu = ShowingMenu::reading();
    h.run_menu("/context", &menu).await;

    assert_eq!(menu.titles(), ["summary", "system", "tools", "messages"]);
    // The numbers the command has always printed are the first tab, unchanged.
    assert!(
        menu.rows("summary")
            .iter()
            .any(|row| row.contains("tokens")),
        "{:?}",
        menu.rows("summary")
    );
    // And the message behind the count, which used to be measured and dropped.
    assert!(
        menu.rows("messages")
            .iter()
            .any(|row| row.contains("what changed today")),
        "{:?}",
        menu.rows("messages")
    );
}

#[tokio::test]
async fn memory_lists_the_index_the_prompt_carries() {
    // The list, not just the count. It was already built here to be measured
    // and then dropped, which made "how many" the only answer to a question
    // that was really "what does it remember".
    let h = Harness::bare("cli:1");
    memory(&h, "rem-over-px");
    memory(&h, "ci-gate");

    let mut h = h;
    let menu = AnsweringMenu::cancelled();
    h.run_menu("/memory", &menu).await;

    let offered = menu.offered();
    assert_eq!(offered.len(), 1, "{offered:?}");
    // The summary on a disabled first row, then one per memory.
    assert!(offered[0][0].contains("2 memories"), "{offered:?}");
    assert_eq!(
        &offered[0][1..],
        ["ci-gate: About ci-gate", "rem-over-px: About rem-over-px"]
    );
}

#[tokio::test]
async fn choosing_a_memory_opens_the_file_the_model_is_sent() {
    let h = Harness::bare("cli:1");
    memory(&h, "ci-gate");

    let mut h = h;
    let menu = ShowingMenu::opening("ci-gate: About ci-gate");
    h.run_menu("/memory", &menu).await;

    assert_eq!(menu.titles(), ["ci-gate"]);
    assert!(
        menu.rows("ci-gate").iter().any(|row| row.contains("Body.")),
        "{:?}",
        menu.rows("ci-gate")
    );
}

#[tokio::test]
async fn memory_opens_nothing_when_nothing_is_remembered() {
    let mut h = Harness::bare("cli:1");
    h.run_menu("/memory", &ShowingMenu::reading()).await;

    assert!(h.text().contains("nothing remembered yet"), "{}", h.text());
}

#[tokio::test]
async fn the_memory_window_flips_the_tool_on_the_agent() {
    let h = Harness::bare("cli:1");
    memory(&h, "ci-gate");

    let mut h = h;
    h.run_menu(
        "/memory",
        &AnsweringMenu::acting("ci-gate: About ci-gate", 0, 1),
    )
    .await;

    assert_eq!(
        entry(&h.install.config(), "default").tools.get("memory"),
        Some(&ToolPermission::Deny)
    );
}

#[tokio::test]
async fn the_workspace_manager_switches_on_a_plain_choice() {
    let h = Harness::new(Install::bare(), "cli:1");
    h.runtime()
        .workspaces()
        .create(CreateWorkspace {
            name: "Research".to_owned(),
            id: Some("research".to_owned()),
            ..CreateWorkspace::default()
        })
        .unwrap();

    let mut h = h;
    h.run_menu("/workspace", &AnsweringMenu::choosing("Research"))
        .await;

    assert_eq!(h.workspace_id.as_deref(), Some("research"));
}

#[tokio::test]
async fn renaming_happens_in_the_window_on_a_line_typed_into_it() {
    // Rather than a command handed back to the composer, which was the shape
    // before: a manager you have to leave in order to manage with is not one.
    let h = Harness::new(Install::bare(), "cli:1");
    h.runtime()
        .workspaces()
        .create(CreateWorkspace {
            name: "Research".to_owned(),
            id: Some("research".to_owned()),
            ..CreateWorkspace::default()
        })
        .unwrap();

    let mut h = h;
    h.run_menu("/workspace", &TypingMenu::new("Research", 0, "Field notes"))
        .await;

    assert_eq!(
        h.runtime()
            .workspaces()
            .get("research")
            .unwrap()
            .unwrap()
            .name,
        "Field notes"
    );
}

#[tokio::test]
async fn the_row_that_makes_one_asks_for_a_name_and_makes_it() {
    let mut h = Harness::new(Install::bare(), "cli:1");
    h.run_menu(
        "/workspace",
        &TypingMenu::new("new workspace…", usize::MAX, "Field notes"),
    )
    .await;

    let made = h.runtime().workspaces().list().unwrap();
    assert!(made.iter().any(|row| row.name == "Field notes"), "{made:?}");
}

#[tokio::test]
async fn a_closed_workspace_manager_changes_nothing_and_says_nothing() {
    // Silence, unlike the note this used to fall through to. The window marked
    // the one new sessions land in and was read; repeating it in the
    // conversation afterwards is an answer to a question already answered, in
    // the one place a terminal cannot take it back from.
    let mut h = Harness::bare("cli:1");
    h.run_menu("/workspace", &AnsweringMenu::cancelled()).await;

    assert_eq!(h.workspace_id, None);
    assert_eq!(h.text().trim(), "");
}

#[tokio::test]
async fn bare_workspace_lists_them_where_there_is_no_menu_to_open() {
    // A pipe cannot draw the manager, so it gets what the manager shows: every
    // workspace, its id, its count, and a mark on the one in force.
    let mut h = Harness::bare("cli:1");
    h.run("/workspace").await;

    let text = h.text();
    assert!(text.contains("* default"), "{text}");
    assert!(text.contains("0 sessions"), "{text}");
}

#[tokio::test]
async fn bare_session_shows_this_one_before_the_rest_where_there_is_no_picker() {
    // The detail `/session` used to print on its own survives as the pipe's
    // answer, above the listing `/sessions` used to print. One command, and a
    // script loses nothing in the merge.
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", said("hello"), &AppendOptions::default())
        .unwrap();

    let mut h = h;
    h.run("/session").await;

    let text = h.text();
    assert!(text.contains("workspace default"), "the detail: {text}");
    assert!(text.contains("* "), "and the listing under it: {text}");
}

#[tokio::test]
async fn session_with_a_key_attaches_without_opening_anything() {
    let mut h = Harness::bare("cli:1");
    let menu = AnsweringMenu::cancelled();
    let outcome = h.run_menu("/session cli:2", &menu).await;

    assert_eq!(outcome, SlashOutcome::Attach("cli:2".to_owned()));
    assert!(menu.offered().is_empty(), "{:?}", menu.offered());
}

#[tokio::test]
async fn delete_drops_the_last_exchange_rather_than_the_conversation() {
    // It used to take a session key and delete the whole thing, which put the
    // most destructive act behind the shortest word. Deleting a session is a
    // verb on its row in `/session` now.
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", said("first"), &AppendOptions::default())
        .unwrap();
    store
        .append("cli:1", answered("an answer"), &AppendOptions::default())
        .unwrap();
    store
        .append("cli:1", said("second"), &AppendOptions::default())
        .unwrap();

    let mut h = h;
    h.run("/delete").await;

    // The question and everything after it, and the conversation still there.
    assert_eq!(h.runtime().store().message_count("cli:1").unwrap(), 2);
    assert!(h.runtime().store().get_session("cli:1").unwrap().is_some());
}

#[tokio::test]
async fn the_skills_window_lists_every_sheet() {
    let h = Harness::bare("cli:1");
    sheet(&h, "deploy", "Ship a release.", None);
    sheet(&h, "triage", "Sort the inbox.", Some("lead"));

    let mut h = h;
    let menu = AnsweringMenu::cancelled();
    h.run_menu("/skills", &menu).await;

    let offered = menu.offered();
    assert_eq!(offered.len(), 1, "{offered:?}");
    assert_eq!(offered[0], ["2 sheets in skills/", "deploy", "triage"]);
}

#[tokio::test]
async fn choosing_a_sheet_opens_it() {
    let h = Harness::bare("cli:1");
    sheet(&h, "deploy", "Ship a release.", None);

    let mut h = h;
    let menu = ShowingMenu::opening("deploy");
    h.run_menu("/skills", &menu).await;

    assert_eq!(menu.titles(), ["deploy"]);
    assert!(
        menu.rows("deploy")
            .iter()
            .any(|row| row.contains("Body of deploy")),
        "{:?}",
        menu.rows("deploy")
    );
}

#[tokio::test]
async fn the_skills_window_flips_the_tool_and_opens_even_when_it_is_off() {
    // The one place the switch is needed is the place it is off, so the list
    // has to open to reach it.
    let mut h = Harness::new(
        Install::new(Some(&json!({
            "agents": { "list": { "default": { "tools": { "skill": "deny" } } } }
        }))),
        "cli:1",
    );
    sheet(&h, "deploy", "Ship a release.", None);

    h.run_menu("/skills", &AnsweringMenu::acting("deploy", 0, 1))
        .await;

    assert_eq!(
        entry(&h.install.config(), "default").tools.get("skill"),
        Some(&ToolPermission::Allow)
    );
}
