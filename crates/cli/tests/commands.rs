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
    CommandRow, SlashContext, SlashModels, SlashOutcome, command_rows, command_rows_for, help_text,
    palette_rows, run_slash_command,
};
use darkwire::i18n::Translations;
use darkwire::pickers::palette::{PaletteRow, command_items, command_value, complete_command};
use darkwire::pickers::{MenuRequest, NoMenu, PickerMenu};
use darkwire::render::{TurnRenderer, TurnRendererOptions};
use darkwire::runtime::ChatRuntime;
use darkwire_core::session_store::TurnStatsRecord;
use darkwire_core::session_store::{AppendOptions, CreateSession, UpdateSession};
use darkwire_core::workspace_store::CreateWorkspace;
use darkwire_core::{Database, Result};
use darkwire_protocol::rest::{ModelInfo, ModelsResponse};
use darkwire_protocol::tasks::{TaskItem, TaskStatus};
use darkwire_protocol::{Config, ReasoningEffort, ToolPermission};
use darkwire_protocol::{StopReason, Usage};
use darkwire_runtime::{ExtensionChoice, McpChoice, RuntimeOptions, VaultChoice, create_runtime};
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
    seen: Mutex<Vec<Vec<String>>>,
}

impl AnsweringMenu {
    fn choosing(label: &str) -> AnsweringMenu {
        AnsweringMenu {
            label: Some(label.to_owned()),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Available, and cancelled.
    fn cancelled() -> AnsweringMenu {
        AnsweringMenu {
            label: None,
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
    ) -> Pin<Box<dyn Future<Output = Option<usize>> + Send + 'a>> {
        let labels: Vec<String> = request
            .items
            .iter()
            .map(|item| item.label.clone())
            .collect();
        self.seen.lock().unwrap().push(labels);
        let at = self
            .label
            .as_ref()
            .and_then(|want| request.items.iter().position(|item| &item.label == want));
        Box::pin(std::future::ready(at))
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
    model_pinned: bool,
}

impl Harness {
    fn new(install: Install, session_key: &str) -> Harness {
        let sink = Sink::default();
        let renderer = TurnRenderer::new(TurnRendererOptions {
            // Stated rather than detected: a case must not print escape codes
            // into its own assertions because the runner happened to own a tty.
            colors: Some(false),
            ..TurnRendererOptions::new(Box::new(sink.clone()))
        });
        Harness {
            install,
            renderer,
            sink,
            t: Translations::default(),
            session_key: session_key.to_owned(),
            workspace_id: None,
            agent_id: None,
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
    assert!(help.contains("/messages [n]"));
    assert!(help.contains("/workspace move <from> <to>"));
    assert!(help.contains("the last n messages, with their seq numbers"));
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
fn the_variant_rows_carry_no_description_of_their_own() {
    // `/workspace new <name>` sits under `/workspace`, which already described
    // it. An invented sentence there would be a second description of one
    // command.
    let rows = command_rows();
    let variants: Vec<&CommandRow> = rows.iter().filter(|row| row.key.is_none()).collect();
    assert_eq!(variants.len(), 2);
    for row in variants {
        assert!(row.syntax.starts_with("/workspace "));
    }
}

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

    // A prefix answers with every command that extends it — `/workspace` and
    // its verbs are separate things to complete to, because each is a separate
    // command, and `/workspaces` is a sixth.
    let (candidates, _) = complete_command("/works", &rows);
    assert_eq!(
        candidates.iter().collect::<BTreeSet<_>>(),
        [
            "/workspaces".to_owned(),
            "/workspace".to_owned(),
            "/workspace new".to_owned(),
            "/workspace rename".to_owned(),
            "/workspace rm".to_owned(),
            "/workspace move".to_owned(),
        ]
        .iter()
        .collect::<BTreeSet<_>>()
    );
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
    assert!(h.text().contains("/messages [n]"));
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

#[tokio::test]
async fn messages_says_so_when_nothing_has_been_said() {
    let mut h = Harness::bare("cli:1");
    h.run("/messages").await;
    assert!(h.text().contains("nothing said in this session yet"));
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

#[tokio::test]
async fn workspace_rm_refuses_while_sessions_still_name_it() {
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
        .ensure_session(
            "cli:1",
            CreateSession {
                workspace_id: Some("research".to_owned()),
                ..CreateSession::default()
            },
        )
        .unwrap();

    let mut h = h;
    h.run("/workspace rm research").await;

    assert!(h.text().contains("still in research"), "{}", h.text());
    assert!(h.runtime().workspaces().get("research").unwrap().is_some());
}

#[tokio::test]
async fn workspace_new_and_rename_report_what_they_did() {
    let mut h = Harness::bare("cli:1");

    h.run("/workspace new Deep Research").await;
    assert!(h.text().contains("created"), "{}", h.text());
    let created = h
        .runtime()
        .workspaces()
        .list()
        .unwrap()
        .into_iter()
        .find(|workspace| workspace.name == "Deep Research")
        .unwrap();

    h.sink.clear();
    h.run(&format!("/workspace rename {} Shallow", created.id))
        .await;
    assert_eq!(
        h.runtime()
            .workspaces()
            .get(&created.id)
            .unwrap()
            .unwrap()
            .name,
        "Shallow"
    );
}

#[tokio::test]
async fn workspace_new_with_no_name_says_how_to_use_it() {
    let mut h = Harness::bare("cli:1");
    h.run("/workspace new").await;
    assert!(h.text().contains("usage: /workspace new"), "{}", h.text());
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

#[tokio::test]
async fn bare_workspace_falls_back_to_saying_where_sessions_land() {
    let mut h = Harness::bare("cli:1");
    h.run_menu("/workspace", &AnsweringMenu::cancelled()).await;

    assert_eq!(h.workspace_id, None);
    assert!(h.text().contains("new sessions land in default"));
}

#[tokio::test]
async fn bare_workspace_says_the_same_thing_without_a_menu() {
    let mut h = Harness::bare("cli:1");
    h.run("/workspace").await;
    assert!(h.text().contains("new sessions land in default"));
}

// /output

#[tokio::test]
async fn output_lists_every_switch_and_its_state_when_asked_for_nothing() {
    // The bare form is what makes the switches discoverable at all — two
    // separate commands never were.
    let mut h = Harness::bare("cli:1");
    h.run("/output").await;

    assert!(h.text().contains("reasoning  shown"), "{}", h.text());
    // Hidden, which is how a turn's cost arrives now: worth having, and not
    // worth a row under every answer.
    assert!(h.text().contains("stats      hidden"), "{}", h.text());
}

#[tokio::test]
async fn output_flips_a_field_named_with_no_word() {
    let mut h = Harness::bare("cli:1");
    assert!(h.renderer.reasoning_shown());

    h.run("/output reasoning").await;
    assert!(!h.renderer.reasoning_shown());
    assert!(h.text().contains("hidden: reasoning"));

    h.run("/output reasoning").await;
    assert!(h.renderer.reasoning_shown());
    assert!(h.text().contains("shown: reasoning"));
}

#[tokio::test]
async fn output_says_it_outright_with_on_and_off() {
    // For a hand that has lost track of which way the switch is.
    let mut h = Harness::bare("cli:1");

    h.run("/output stats off").await;
    h.run("/output stats off").await;
    assert!(!h.renderer.stats_shown());

    h.run("/output stats on").await;
    h.run("/output stats on").await;
    assert!(h.renderer.stats_shown());
}

#[tokio::test]
async fn output_shows_the_new_state_in_the_listing_afterwards() {
    let mut h = Harness::bare("cli:1");
    h.run("/output stats off").await;
    h.sink.clear();

    h.run("/output").await;

    assert!(h.text().contains("stats      hidden"), "{}", h.text());
    assert!(h.text().contains("reasoning  shown"), "{}", h.text());
}

#[tokio::test]
async fn output_refuses_a_field_it_has_never_heard_of() {
    // Warned rather than propagated, and nothing changes.
    let mut h = Harness::bare("cli:1");
    h.run("/output colours off").await;

    assert!(h.text().contains("colours"));
    assert!(h.renderer.reasoning_shown());
    assert!(!h.renderer.stats_shown());
}

#[tokio::test]
async fn output_treats_a_word_it_does_not_know_as_a_flip() {
    let mut h = Harness::bare("cli:1");
    h.run("/output stats yes-please").await;
    assert!(h.renderer.stats_shown());
}

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
fn memory(h: &Harness, name: &str) {
    let dir = h.runtime().jail().root().join("memory");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.md")),
        format!("---\ndescription: about {name}\nmetadata:\n  type: project\n---\n\nBody.\n"),
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
async fn memory_turns_off_on_the_agent_and_back_on() {
    let mut h = Harness::bare("cli:1");

    h.run("/memory off").await;
    assert_eq!(
        entry(&h.install.config(), "default").tools.get("memory"),
        Some(&ToolPermission::Deny)
    );
    assert!(h.text().contains("no longer remembers"), "{}", h.text());

    h.run("/memory on").await;
    assert_eq!(
        entry(&h.install.config(), "default").tools.get("memory"),
        Some(&ToolPermission::Allow)
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

    h.run("/memory off").await;

    let config = h.install.config();
    let default = entry(&config, "default");
    assert_eq!(default.tools.get("read"), Some(&ToolPermission::Allow));
    assert_eq!(default.tools.get("exec"), Some(&ToolPermission::Deny));
    assert_eq!(default.tools.get("memory"), Some(&ToolPermission::Deny));
    // And nothing else on the entry was dropped either.
    assert_eq!(default.label, "Primary");
}

#[tokio::test]
async fn memory_names_its_verbs_when_given_one_it_does_not_know() {
    let mut h = Harness::bare("cli:1");
    h.run("/memory sideways").await;

    assert!(h.text().contains("/memory on"), "{}", h.text());
    // `compress` was a third verb, and went with the format it folded into.
    assert!(!h.text().contains("compress"));
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
async fn new_attaches_to_a_fresh_conversation() {
    let mut h = Harness::bare("cli:1");
    let outcome = h.run("/new Notes on the build").await;

    let SlashOutcome::Attach(key) = outcome else {
        panic!("expected an attach, got {outcome:?}");
    };
    assert!(key.starts_with("cli-"));
    assert_eq!(
        h.runtime()
            .store()
            .get_session(&key)
            .unwrap()
            .unwrap()
            .title,
        "Notes on the build"
    );
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

    assert_eq!(
        h.runtime()
            .store()
            .get_session(&key)
            .unwrap()
            .unwrap()
            .workspace_id,
        "research"
    );
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
async fn delete_of_the_attached_conversation_lands_somewhere_new() {
    let h = Harness::bare("cli:1");
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    let outcome = h.run("/delete").await;

    let SlashOutcome::Attach(key) = outcome else {
        panic!("expected an attach, got {outcome:?}");
    };
    assert_ne!(key, "cli:1");
    assert!(h.runtime().store().get_session("cli:1").unwrap().is_none());
}

#[tokio::test]
async fn delete_of_a_key_that_names_nothing_is_a_refusal() {
    let mut h = Harness::bare("cli:1");
    h.run("/delete cli:ghost").await;
    assert!(h.text().contains("No session cli:ghost"), "{}", h.text());
}

#[tokio::test]
async fn sessions_says_so_when_there_are_none() {
    let mut h = Harness::bare("cli:1");
    h.run("/sessions").await;
    assert!(h.text().contains("no sessions yet"), "{}", h.text());
}

#[tokio::test]
async fn sessions_lists_them_with_the_current_one_marked() {
    let h = Harness::bare("cli:1");
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    h.run("/sessions").await;

    assert!(h.text().contains("* (unnamed)"), "{}", h.text());
    assert!(h.text().contains("cli:1"));
}

#[tokio::test]
async fn stats_says_so_when_no_turn_has_run() {
    let mut h = Harness::bare("cli:1");
    h.run("/stats").await;
    assert!(h.text().contains("no turns recorded"), "{}", h.text());
}

// /edit and /regenerate

#[tokio::test]
async fn edit_without_a_replacement_says_how_to_use_it() {
    let mut h = Harness::bare("cli:1");
    h.run("/edit 1").await;
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
    let outcome = h.run("/edit -1 what changed here?").await;

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
async fn edit_refuses_a_message_that_is_not_yours() {
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", answered("I said this."), &AppendOptions::default())
        .unwrap();

    let mut h = h;
    h.run("/edit 1 no you did not").await;

    assert!(h.text().contains("not one of yours"), "{}", h.text());
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

#[tokio::test]
async fn workspaces_lists_them_with_the_pending_one_marked() {
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
    h.sink.clear();
    h.run("/workspaces").await;

    assert!(h.text().contains("* research"), "{}", h.text());
    assert!(h.text().contains("  default"), "{}", h.text());
}

// /messages, /sessions, /session, /delete

#[tokio::test]
async fn messages_shows_the_recent_lines_newest_last() {
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .append("cli:1", said("what is here?"), &AppendOptions::default())
        .unwrap();
    store
        .append(
            "cli:1",
            answered("a repository."),
            &AppendOptions::default(),
        )
        .unwrap();

    let mut h = h;
    h.run("/messages").await;

    let text = h.text();
    assert!(text.contains("what is here?"), "{text}");
    assert!(text.contains("a repository."), "{text}");
    assert!(
        text.find("what is here?") < text.find("a repository."),
        "the transcript reads oldest first: {text}"
    );
}

#[tokio::test]
async fn messages_takes_the_count_as_a_page_size() {
    // The same argument `/sessions` takes, so a person and a script are asking
    // the same question of the same rows.
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    for text in ["one", "two", "three"] {
        store
            .append("cli:1", said(text), &AppendOptions::default())
            .unwrap();
    }

    let mut h = h;
    h.run("/messages 1").await;

    let text = h.text();
    assert!(text.contains("three"), "{text}");
    assert!(!text.contains("one"), "{text}");
}

#[tokio::test]
async fn sessions_prints_the_listing_where_there_is_no_menu_to_open() {
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
    h.run("/sessions").await;

    let text = h.text();
    assert!(text.contains("* "), "the current one is marked: {text}");
    assert!(text.contains("cli:1"), "{text}");
    assert!(
        text.contains("(unnamed)"),
        "a session nothing has named says so: {text}"
    );
}

#[tokio::test]
async fn sessions_attaches_to_what_the_picker_answered() {
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
    let outcome = h.run_menu("/sessions", &menu).await;

    assert_eq!(outcome, SlashOutcome::Attach("cli:2".to_owned()));
}

#[tokio::test]
async fn sessions_stays_put_when_the_picker_was_cancelled() {
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
    // Created rather than only named: the prompt is about to write into it.
    assert!(
        h.runtime()
            .store()
            .get_session("cli:other")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn delete_removes_a_named_conversation_and_stays_where_it_is() {
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    for key in ["cli:1", "cli:2"] {
        store.ensure_session(key, CreateSession::default()).unwrap();
    }

    let mut h = h;
    let outcome = h.run("/delete cli:2").await;

    assert_eq!(outcome, SlashOutcome::Continue);
    assert!(h.runtime().store().get_session("cli:2").unwrap().is_none());
    assert!(h.runtime().store().get_session("cli:1").unwrap().is_some());
}

#[tokio::test]
async fn delete_refuses_a_key_the_store_has_never_heard_of() {
    let mut h = Harness::bare("cli:1");
    h.run("/delete cli:nowhere").await;

    assert!(h.text().contains("cli:nowhere"), "{}", h.text());
}

// /workspace rm, move, rename

#[tokio::test]
async fn workspace_rm_detaches_one_nothing_is_left_in() {
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
    h.sink.clear();
    h.run("/workspace rm research").await;

    assert!(h.runtime().workspaces().get("research").unwrap().is_none());
    // The pending workspace pointed at the one just removed, so it is cleared
    // rather than left naming a row that is gone.
    assert_eq!(h.workspace_id, None);
}

#[tokio::test]
async fn workspace_rm_with_no_id_says_how_to_use_it() {
    let mut h = Harness::bare("cli:1");
    h.run("/workspace rm").await;

    assert!(h.text().contains("usage: /workspace rm"), "{}", h.text());
}

#[tokio::test]
async fn workspace_move_reassigns_every_conversation_and_counts_them() {
    let h = Harness::new(Install::bare(), "cli:1");
    h.runtime()
        .workspaces()
        .create(CreateWorkspace {
            name: "Research".to_owned(),
            id: Some("research".to_owned()),
            ..CreateWorkspace::default()
        })
        .unwrap();
    let store = h.runtime().store();
    for key in ["cli:1", "cli:2"] {
        store.ensure_session(key, CreateSession::default()).unwrap();
    }

    let mut h = h;
    h.run("/workspace move default research").await;

    assert!(h.text().contains('2'), "{}", h.text());
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
async fn workspace_move_refuses_a_destination_that_does_not_exist() {
    // Reassigning into a row nobody created would leave every moved
    // conversation pointing at nothing.
    let h = Harness::new(Install::bare(), "cli:1");
    h.runtime()
        .store()
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();

    let mut h = h;
    h.run("/workspace move default nowhere").await;

    assert!(h.text().contains("nowhere"), "{}", h.text());
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

#[tokio::test]
async fn workspace_move_with_one_argument_says_how_to_use_it() {
    let mut h = Harness::bare("cli:1");
    h.run("/workspace move default").await;

    assert!(h.text().contains("usage: /workspace move"), "{}", h.text());
}

#[tokio::test]
async fn workspace_rename_with_no_name_says_how_to_use_it() {
    let mut h = Harness::bare("cli:1");
    h.run("/workspace rename default").await;

    assert!(
        h.text().contains("usage: /workspace rename"),
        "{}",
        h.text()
    );
}

// /stats and /context

/// One recorded turn, with only the fields a stats row shows filled in.
fn turn(id: &str, model: &str, ended_at_ms: i64) -> TurnStatsRecord {
    TurnStatsRecord {
        turn_id: id.to_owned(),
        session_key: "cli:1".to_owned(),
        agent_id: "default".to_owned(),
        workspace_id: "default".to_owned(),
        provider: "local".to_owned(),
        model: model.to_owned(),
        started_at_ms: ended_at_ms - 1000,
        ended_at_ms,
        iterations: 1,
        stop_reason: StopReason::Complete,
        usage: Usage {
            prompt_tokens: 100,
            completion_tokens: 20,
            total_tokens: 120,
            cached_tokens: None,
            reasoning_tokens: None,
        },
        generation_ms: Some(800),
        generation_tokens: Some(20),
        first_token_ms: Some(150),
        error: None,
    }
}

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
async fn stats_shows_the_recorded_turns() {
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .record_turn_stats(&turn("t1", "qwen3:8b", 2000))
        .unwrap();

    let mut h = h;
    h.run("/stats").await;

    assert!(h.text().contains("qwen3:8b"), "{}", h.text());
}

#[tokio::test]
async fn stats_takes_the_count_as_a_page_size() {
    // Newest first, so a limit of one is the turn that just ran rather than the
    // first one of the session.
    let h = Harness::bare("cli:1");
    let store = h.runtime().store();
    store
        .ensure_session("cli:1", CreateSession::default())
        .unwrap();
    store
        .record_turn_stats(&turn("t1", "old-model", 1000))
        .unwrap();
    store
        .record_turn_stats(&turn("t2", "new-model", 5000))
        .unwrap();

    let mut h = h;
    h.run("/stats 1").await;

    assert!(h.text().contains("new-model"), "{}", h.text());
    assert!(!h.text().contains("old-model"), "{}", h.text());
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

#[tokio::test]
async fn tasks_clear_empties_the_list() {
    let h = Harness::bare("cli:1");
    h.runtime()
        .store()
        .set_tasks(
            "cli:1",
            &[TaskItem {
                text: "Add tests".to_owned(),
                status: TaskStatus::Todo,
            }],
        )
        .unwrap();

    let mut h = h;
    h.run("/tasks clear").await;

    assert!(h.text().contains("cleared"), "{}", h.text());
    assert_eq!(h.runtime().store().tasks("cli:1").unwrap(), Vec::new());
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
