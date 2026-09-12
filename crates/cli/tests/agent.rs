//! `ghostai agent`, the preset installer.
//!
//! What is asserted here is the *merge*: that a preset lands in `agents.list`
//! exactly once, that the refusals fire before the write, and that the roster
//! snapshot offers only specialists that can answer. The preset shape itself is
//! `ghostai-protocol`'s and the toolbox gate is `ghostai-security`'s — both
//! already tested where they live.
//!
//! Every run points `GHOSTAI_CATALOGUE` somewhere this file controls, including
//! at a path that does not exist. Without that the sibling-checkout lookup would
//! find a real presets repository on a reviewer's machine and not on CI, which
//! is the one flake this command can produce.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "clippy's allow-*-in-tests covers `#[test]` bodies only, and the \
              fixtures here are ordinary functions; a fixture that cannot load is \
              a failing test either way, and `?` in one hides which line gave up"
)]

use std::path::Path;

use ghostai::Streams;
use ghostai::i18n::Env;
use ghostai::program::{AgentCommand, Globals, StoreAction};
use serde_json::{Value, json};
use tempfile::TempDir;

/// A sink that keeps what was written to it, so a test can read it back.
#[derive(Clone, Default)]
struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl Sink {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
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

/// What one run produced.
struct Run {
    code: u8,
    output: String,
    errors: String,
}

/// One temporary install, with the catalogue pinned to a path of its own.
struct Home {
    dir: TempDir,
    catalogue: Option<String>,
}

impl Home {
    fn new() -> Home {
        Home {
            dir: TempDir::new().expect("a temporary home"),
            catalogue: None,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn globals(&self) -> Globals {
        Globals {
            home: Some(self.path().to_string_lossy().into_owned()),
            color: Some(false),
            ..Globals::default()
        }
    }

    /// The environment, with the catalogue pinned wherever this test put it.
    ///
    /// A path that does not exist when no catalogue was written, which is what
    /// stops the sibling lookup from reaching a real checkout.
    fn env(&self) -> Env {
        let catalogue = self.catalogue.clone().unwrap_or_else(|| {
            self.path()
                .join("no-catalogue-here")
                .to_string_lossy()
                .into_owned()
        });
        [("GHOSTAI_CATALOGUE", catalogue)].into_iter().collect()
    }

    /// A catalogue holding one skill sheet.
    fn with_sheet(&mut self, name: &str, agents: Option<&str>) {
        let dir = self.path().join("fixture-catalogue");
        std::fs::create_dir_all(dir.join("agents")).expect("an agents directory");
        let sheet = dir.join("skills").join(name);
        std::fs::create_dir_all(&sheet).expect("a sheet directory");
        let scope = agents.map_or_else(String::new, |value| format!("agents: {value}\n"));
        std::fs::write(
            sheet.join("SKILL.md"),
            format!("---\ndescription: What {name} does.\n{scope}---\n\nBody of {name}.\n"),
        )
        .expect("a sheet");
        self.catalogue = Some(dir.to_string_lossy().into_owned());
    }

    /// A toolbox manifest on disk. Presets never live here — see [`Home::preset`].
    fn toolbox(&self, name: &str) {
        let dir = self.path().join("toolboxes").join(name);
        std::fs::create_dir_all(&dir).expect("a toolbox directory");
        std::fs::write(
            dir.join("toolbox.json"),
            json!({
                "schema": "ghostai.toolbox/1",
                "name": name,
                "image": format!("sha256:{}", "d".repeat(64)),
                "tools": [{"name": "rg", "use": "Search."}],
            })
            .to_string(),
        )
        .expect("a manifest");
    }

    fn approve(&self, name: &str) {
        let out = Sink::default();
        let err = Sink::default();
        let mut streams = Streams {
            out: Box::new(out),
            err: Box::new(err.clone()),
        };
        let code = ghostai::toolbox::run(
            &self.globals(),
            StoreAction::Approve,
            Some(name),
            &self.env(),
            &mut streams,
        )
        .expect("the approval answers with an exit code");
        assert_eq!(code, 0, "{}", err.text());
    }

    /// A preset in `<root>/presets`, the operator's own directory.
    fn preset(&self, name: &str, preset: &Value) {
        let dir = self.path().join("presets");
        std::fs::create_dir_all(&dir).expect("a presets directory");
        std::fs::write(dir.join(format!("{name}.json")), preset.to_string()).expect("a preset");
    }

    /// The settings tree on disk, or `None` when nothing was written.
    fn config(&self) -> Option<Value> {
        let text = std::fs::read_to_string(self.path().join("config.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// One agent's entry, which must exist.
    fn agent(&self, id: &str) -> Value {
        self.config()
            .expect("a config was written")
            .pointer(&format!("/agents/list/{id}"))
            .cloned()
            .unwrap_or_else(|| panic!("no {id} in the saved config"))
    }

    fn run(&self, command: &AgentCommand) -> Run {
        let out = Sink::default();
        let err = Sink::default();
        let mut streams = Streams {
            out: Box::new(out.clone()),
            err: Box::new(err.clone()),
        };
        let code = ghostai::agent::run(&self.globals(), command, &self.env(), &mut streams)
            .expect("the command answers with an exit code rather than failing");
        Run {
            code,
            output: out.text(),
            errors: err.text(),
        }
    }

    fn install(&self, name: &str) -> Run {
        self.install_with(name, false, None)
    }

    fn install_forced(&self, name: &str) -> Run {
        self.install_with(name, true, None)
    }

    fn install_with(&self, name: &str, force: bool, workspace_id: Option<&str>) -> Run {
        self.run(&AgentCommand::Install {
            name: name.to_owned(),
            force,
            workspace_id: workspace_id.map(str::to_owned),
        })
    }

    fn list(&self) -> Run {
        self.run(&AgentCommand::List)
    }
}

/// A preset body with `schema` and `id` filled in.
fn preset_for(id: &str, overrides: &Value) -> Value {
    let mut body = json!({"schema": "ghostai.agent-preset/1", "id": id});
    if let (Some(target), Some(extra)) = (body.as_object_mut(), overrides.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    body
}

// install

#[test]
fn installs_a_preset_from_an_explicit_path() {
    let home = Home::new();
    let file = home.path().join("my-agent.json");
    std::fs::write(
        &file,
        preset_for("scribe", &json!({"label": "Scribe"})).to_string(),
    )
    .unwrap();

    let run = home.install(&file.to_string_lossy());

    assert_eq!(run.code, 0, "{}", run.errors);
    assert_eq!(home.agent("scribe")["label"], json!("Scribe"));
    assert_eq!(home.agent("scribe")["enabled"], json!(true));
}

#[test]
fn installs_a_preset_by_its_id_whether_or_not_it_names_a_toolbox() {
    // One lookup for both kinds. The preset is found by its own id — never
    // beside the manifest of the box it happens to name.
    let home = Home::new();
    home.toolbox("research");
    home.approve("research");
    home.preset(
        "scout",
        &preset_for("scout", &json!({"toolbox": {"name": "research"}})),
    );

    let run = home.install("scout");

    assert_eq!(run.code, 0, "{}", run.errors);
    assert_eq!(home.agent("scout")["toolbox"]["name"], json!("research"));
}

#[test]
fn refuses_a_preset_whose_toolbox_was_never_approved() {
    // An enabled agent naming an unapproved toolbox is a config the server
    // refuses to boot on, so the refusal happens here, before the write.
    let home = Home::new();
    home.toolbox("research");
    home.preset(
        "scout",
        &preset_for("scout", &json!({"toolbox": {"name": "research"}})),
    );

    let run = home.install("scout");

    assert_eq!(run.code, 1);
    assert!(
        run.errors.contains("ghostai toolbox approve research"),
        "{}",
        run.errors
    );
    assert!(home.config().is_none(), "nothing was written");
}

#[test]
fn refuses_a_network_request_above_the_toolbox_ceiling() {
    // The runtime would refuse the same pair at build; failing at install is
    // the same rule at the moment the operator can still fix the preset.
    let home = Home::new();
    home.toolbox("research");
    home.approve("research");
    home.preset(
        "scout",
        &preset_for(
            "scout",
            &json!({"toolbox": {"name": "research", "network": {"mode": "open"}}}),
        ),
    );

    let run = home.install("scout");

    assert_eq!(run.code, 1);
    let lowered = run.errors.to_lowercase();
    assert!(
        lowered.contains("network") || lowered.contains("open"),
        "{}",
        run.errors
    );
}

#[test]
fn refuses_to_overwrite_an_existing_agent_without_force() {
    // The existing entry may carry the operator's own edits.
    let home = Home::new();
    let file = home.path().join("scribe.json");
    std::fs::write(&file, preset_for("scribe", &json!({})).to_string()).unwrap();
    let path = file.to_string_lossy().into_owned();

    assert_eq!(home.install(&path).code, 0);

    let again = home.install(&path);
    assert_eq!(again.code, 1);
    assert!(again.errors.contains("--force"), "{}", again.errors);

    assert_eq!(home.install_forced(&path).code, 0);
}

#[test]
fn refuses_an_id_nothing_downstream_could_use() {
    let home = Home::new();
    let file = home.path().join("bad.json");
    std::fs::write(&file, preset_for("CON", &json!({})).to_string()).unwrap();

    let run = home.install(&file.to_string_lossy());

    assert_eq!(run.code, 1);
    assert!(run.errors.contains("agent id"), "{}", run.errors);
}

#[test]
fn names_the_candidates_when_nothing_matches() {
    let home = Home::new();
    home.preset("team-lead", &preset_for("team-lead", &json!({})));
    home.preset("nano", &preset_for("nano", &json!({})));

    let run = home.install("nope");

    assert_eq!(run.code, 1);
    assert!(run.errors.contains("team-lead"), "{}", run.errors);
    assert!(run.errors.contains("nano"), "{}", run.errors);
}

#[test]
fn names_the_argument_when_nothing_matches() {
    // The contract the top-level parser's own suite asserts: `ghostai agent
    // install nope` on an install with nothing in it exits 1 and says which
    // name it could not find.
    let home = Home::new();

    let run = home.install("nope");

    assert_eq!(run.code, 1);
    assert!(
        run.errors.contains("No preset is available under \"nope\""),
        "{}",
        run.errors
    );
}

#[test]
fn treats_a_path_shaped_argument_as_a_path_even_when_the_file_is_missing() {
    // `./typo.json` must not fall through to an installable preset and install
    // something other than what was named.
    let home = Home::new();
    home.preset("typo", &preset_for("typo", &json!({})));

    let run = home.install("./typo.json");

    assert_eq!(run.code, 1);
    assert!(run.errors.contains("could not be read"), "{}", run.errors);
}

#[test]
fn installs_a_preset_an_operator_dropped_into_the_presets_directory() {
    // The drop-in directory, and the reason the loader takes a directory rather
    // than a hard-coded list: adding a preset is adding a file.
    let home = Home::new();
    home.preset("scribe", &preset_for("scribe", &json!({"label": "Scribe"})));

    assert_eq!(home.install("scribe").code, 0);
    assert_eq!(home.agent("scribe")["label"], json!("Scribe"));
}

#[test]
fn refuses_a_preset_file_that_is_not_valid_json_naming_it() {
    let home = Home::new();
    std::fs::create_dir_all(home.path().join("presets")).unwrap();
    std::fs::write(home.path().join("presets").join("broken.json"), "{").unwrap();

    let run = home.install("broken");

    assert_eq!(run.code, 1);
    assert!(run.errors.contains("not valid JSON"), "{}", run.errors);
}

#[test]
fn installs_an_agent_with_tools_off_and_the_live_sections_deleted() {
    let home = Home::new();
    home.preset(
        "nano",
        &preset_for(
            "nano",
            &json!({
                "toolsEnabled": false,
                "tools": {},
                "livePrompt": " ",
                "wrapUpPrompt": " ",
            }),
        ),
    );

    assert_eq!(home.install("nano").code, 0);

    let entry = home.agent("nano");
    assert_eq!(entry["toolsEnabled"], json!(false));
    assert_eq!(entry["tools"], json!({}));
    // The single space is the three-state spelling for "remove the section".
    assert_eq!(entry["livePrompt"], json!(" "));
    assert_eq!(entry["wrapUpPrompt"], json!(" "));
}

#[test]
fn takes_the_model_and_the_endpoint_from_the_default_agent() {
    // A preset ships neither on purpose — one naming a model would break on
    // every machine that lacks it — so an entry written without them would
    // install an agent that cannot run a turn.
    let home = Home::new();
    home.preset("scribe", &preset_for("scribe", &json!({})));
    assert_eq!(home.install("scribe").code, 0);

    let saved = home.config().expect("a config was written");
    let seed = &saved["agents"]["list"]["default"];
    assert_eq!(home.agent("scribe")["model"], seed["model"]);
    assert_eq!(home.agent("scribe")["provider"], seed["provider"]);
}

// the roster snapshot

/// The delegator whose roster is under test, plus a toolbox-free stand-in for
/// each specialist.
///
/// Every real specialist needs an approved toolbox, which this file has no
/// daemon to build — and the roster rule under test is about *which agents
/// exist*, not about what they run in.
fn with_lead() -> Home {
    let home = Home::new();
    home.preset(
        "team-lead",
        &preset_for(
            "team-lead",
            &json!({"subagents": [{"id": "researcher"}, {"id": "coder"}]}),
        ),
    );
    home
}

fn install_specialist(home: &Home, id: &str) {
    home.preset(id, &preset_for(id, &json!({})));
    assert_eq!(home.install(id).code, 0);
}

fn roster(home: &Home, id: &str) -> Vec<String> {
    home.agent(id)["subagents"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn offers_only_specialists_that_are_installed_and_enabled() {
    let home = with_lead();
    install_specialist(&home, "coder");

    let run = home.install("team-lead");

    assert_eq!(run.code, 0, "{}", run.errors);
    assert_eq!(roster(&home, "team-lead"), vec!["coder".to_owned()]);
    // The missing specialists are named, with the way to add them later.
    assert!(run.output.contains("researcher"), "{}", run.output);
    assert!(run.output.contains("--force"), "{}", run.output);
}

#[test]
fn never_offers_an_agent_the_preset_did_not_name() {
    // A delegator handing "think about this" to a no-tools agent is doing the
    // work itself with a round trip added, so the roster is the preset's list
    // filtered — never every agent that happens to exist.
    let home = with_lead();
    install_specialist(&home, "nano");

    home.install("team-lead");

    assert!(roster(&home, "team-lead").is_empty());
}

#[test]
fn refreshes_the_snapshot_on_a_forced_re_run() {
    let home = with_lead();
    home.install("team-lead");
    assert!(roster(&home, "team-lead").is_empty());

    install_specialist(&home, "coder");
    assert_eq!(home.install_forced("team-lead").code, 0);

    assert_eq!(roster(&home, "team-lead"), vec!["coder".to_owned()]);
}

#[test]
fn skips_a_specialist_that_is_installed_but_disabled() {
    let home = with_lead();
    install_specialist(&home, "coder");

    let mut saved = home.config().expect("a config was written");
    saved["agents"]["list"]["coder"]["enabled"] = json!(false);
    std::fs::write(home.path().join("config.json"), saved.to_string()).unwrap();

    home.install("team-lead");

    assert!(roster(&home, "team-lead").is_empty());
}

// skill sheets

#[test]
fn installs_the_agent_and_names_the_sheets_when_there_is_no_catalogue() {
    // The common case for this command rather than an edge: it never fetches,
    // so an operator's own preset on a box that has never fetched a catalogue
    // lands entirely here. The agent still installs.
    let home = Home::new();
    home.preset(
        "scribe",
        &preset_for("scribe", &json!({"skills": ["code-review"]})),
    );

    let run = home.install("scribe");

    assert_eq!(run.code, 0, "{}", run.errors);
    assert!(home.config().is_some());
    assert!(
        run.output.contains("skill \"code-review\" — not in this"),
        "{}",
        run.output
    );
    assert!(
        run.output.contains("ghostai preset update"),
        "{}",
        run.output
    );
}

#[test]
fn copies_the_sheets_when_a_catalogue_is_reachable() {
    let mut home = Home::new();
    home.preset(
        "scribe",
        &preset_for("scribe", &json!({"skills": ["code-review"]})),
    );
    home.with_sheet("code-review", None);

    let run = home.install("scribe");

    assert_eq!(run.code, 0, "{}", run.errors);
    let sheet = home
        .path()
        .join("workspace")
        .join("skills")
        .join("code-review")
        .join("SKILL.md");
    let text = std::fs::read_to_string(&sheet).expect("the sheet was copied");
    assert!(text.contains("Body of code-review."), "{text}");
    assert!(
        run.output.contains("skills     code-review"),
        "{}",
        run.output
    );
}

#[test]
fn notes_a_sheet_scoped_away_from_the_agent_that_brought_it() {
    let mut home = Home::new();
    home.preset(
        "scribe",
        &preset_for("scribe", &json!({"skills": ["triage"]})),
    );
    home.with_sheet("triage", Some("lead"));

    let run = home.install("scribe");

    assert_eq!(run.code, 0, "{}", run.errors);
    assert!(
        run.output
            .contains("skill \"triage\" is scoped to lead, so the scribe agent will not see it"),
        "{}",
        run.output
    );
}

#[test]
fn refuses_a_named_workspace_that_is_not_there() {
    // The shape of an id is checked by the path helper and the registry lives
    // in SQLite, so without this a typo would make a tree no UI ever lists.
    let mut home = Home::new();
    home.preset(
        "scribe",
        &preset_for("scribe", &json!({"skills": ["code-review"]})),
    );
    home.with_sheet("code-review", None);

    let run = home.install_with("scribe", false, Some("typo"));

    assert_eq!(run.code, 1);
    assert!(run.errors.contains("no typo workspace"), "{}", run.errors);
}

// list

#[test]
fn shows_the_installed_agents_and_the_presets_still_available() {
    let home = Home::new();
    home.preset("nano", &preset_for("nano", &json!({})));
    home.preset("team-lead", &preset_for("team-lead", &json!({})));
    home.install("nano");

    let run = home.list();

    assert_eq!(run.code, 0, "{}", run.errors);
    assert!(run.output.contains("nano  [enabled]"), "{}", run.output);
    assert!(run.output.contains("team-lead"), "{}", run.output);
    // Installed, so it is no longer on offer.
    let offered = run
        .output
        .lines()
        .find(|line| line.starts_with("Presets not yet installed:"))
        .unwrap_or_default();
    assert!(!offered.contains("nano"), "{offered}");
}

#[test]
fn lists_every_available_operator_preset() {
    // One listing, because there is one kind of preset and one search.
    let home = Home::new();
    home.preset("scribe", &preset_for("scribe", &json!({})));
    home.preset("researcher", &preset_for("researcher", &json!({})));

    let run = home.list();

    assert!(
        run.output
            .contains("Presets not yet installed: researcher, scribe"),
        "{}",
        run.output
    );
    // One command, not one per id, and it names the picker rather than this
    // command — that is the one that also builds the toolbox an agent needs.
    assert!(
        run.output.contains("ghostai preset install"),
        "{}",
        run.output
    );
}

#[test]
fn names_an_available_preset_once() {
    let home = Home::new();
    home.preset("nano", &preset_for("nano", &json!({})));

    let run = home.list();

    let mentions = run
        .output
        .lines()
        .filter(|line| line.contains("nano"))
        .count();
    assert_eq!(mentions, 1, "{}", run.output);
}

#[test]
fn says_so_when_an_install_has_no_agents_of_its_own() {
    let home = Home::new();

    let run = home.list();

    assert_eq!(run.code, 0, "{}", run.errors);
    // A fresh install always holds the built-in default, so the listing is
    // never empty — what it must not do is offer nothing and say nothing.
    assert!(run.output.contains("default"), "{}", run.output);
}
