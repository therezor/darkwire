//! `ghostai preset` — the picker, the builds it triggers, and the refusals.
//!
//! Everything that touches the world is injected — the fetcher, the builder,
//! the daemon probe, the prompts — so none of this needs a registry, a daemon
//! or a terminal.
//!
//! The catalogue is a directory this file writes rather than a package it
//! resolves. That is what `--from` is for, and it is also the only way to test
//! the layout: a fixture with three agents, two toolboxes and two containers
//! says more about the ordering rules than eight real ones would, and it does
//! not change when the presets repository does.
//!
//! The prompts are driven by a scripted line reader rather than by a fake
//! `Ask`, so the parsing every answer goes through — a number, a name, `all`,
//! an empty line — is the parsing the product uses. What a test asserts about
//! the *question* it was asked is asserted against the output stream, where the
//! question is actually written.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "clippy's allow-*-in-tests covers `#[test]` bodies only, and the \
              fixtures here are ordinary functions; a fixture that cannot load is \
              a failing test either way, and `?` in one hides which line gave up"
)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ghostai::Streams;
use ghostai::agent::PresetPaths;
use ghostai::ask::{Ask, ScriptedReader};
use ghostai::catalogue::{CATALOGUE_RANGE, fetched_catalogue_dir};
use ghostai::i18n::{Env, Translations};
use ghostai::preset::{PresetOptions, run_preset};
use ghostai::program::{CatalogueArgs, Globals, PresetAction, StoreAction};
use ghostai_core::{ErrorKind, GhostError, Result};
use serde_json::{Value, json};
use tempfile::TempDir;

/// The image id the fake builder pins.
const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A sink that keeps what was written to it, so a test can read it back.
#[derive(Clone, Default)]
struct Sink(std::sync::Arc<Mutex<Vec<u8>>>);

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

/// A temporary install and a temporary catalogue beside it.
struct Harness {
    home: TempDir,
    catalogue: TempDir,
}

impl Harness {
    /// Three agents, two toolboxes and two containers: one agent needing
    /// neither, one needing both, and one delegating to the pair. The `spare`
    /// halves are what nobody named, and so what must never be installed.
    fn new() -> Harness {
        let harness = Harness {
            home: TempDir::new().expect("a temporary home"),
            catalogue: TempDir::new().expect("a temporary catalogue"),
        };
        harness.agent(
            "nano",
            &json!({"label": "Nano", "toolsEnabled": false, "tools": {}}),
        );
        harness.agent(
            "coder",
            &json!({
                "label": "Coder",
                "toolbox": {"name": "coding"},
                "container": {"name": "dev", "network": {"mode": "none", "allow": []}},
            }),
        );
        harness.agent(
            "lead",
            &json!({"label": "Team lead", "subagents": [{"id": "coder"}, {"id": "nano"}]}),
        );
        harness.toolbox("coding");
        harness.toolbox("spare");
        harness.container("dev");
        harness.container("spare");
        harness.skill("code-review", None, &[("checklist.md", "- Read it.\n")]);
        harness.skill("triage", Some("lead"), &[]);
        harness
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn catalogue(&self) -> &Path {
        self.catalogue.path()
    }

    fn globals(&self) -> Globals {
        Globals {
            home: Some(self.home().to_string_lossy().into_owned()),
            color: Some(false),
            ..Globals::default()
        }
    }

    /// The environment, with the catalogue overrides pointed nowhere.
    ///
    /// Every case names `--from` or means to fetch, so a sibling checkout on a
    /// reviewer's machine must never be reachable.
    fn env(&self) -> Env {
        [(
            "GHOSTAI_CATALOGUE",
            self.home()
                .join("no-catalogue-here")
                .to_string_lossy()
                .into_owned(),
        )]
        .into_iter()
        .collect()
    }

    fn agent(&self, id: &str, body: &Value) {
        let dir = self.catalogue().join("agents");
        std::fs::create_dir_all(&dir).expect("an agents directory");
        let mut preset = json!({"schema": "ghostai.agent-preset/1", "id": id});
        if let (Some(target), Some(extra)) = (preset.as_object_mut(), body.as_object()) {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }
        std::fs::write(dir.join(format!("{id}.json")), preset.to_string()).expect("a preset");
    }

    /// A toolbox manifest and the definition it grants, copied verbatim by an
    /// install. Nothing here is built.
    fn toolbox(&self, name: &str) {
        let root = self.catalogue();
        std::fs::create_dir_all(root.join("toolboxes")).expect("a toolboxes directory");
        std::fs::create_dir_all(root.join("tool-definitions")).expect("a definitions directory");
        std::fs::write(
            root.join("toolboxes").join(format!("{name}.json")),
            json!({
                "schema": "ghostai.toolbox/1",
                "name": name,
                "tools": [{"name": "rg", "definition": "rg", "permission": "ask"}],
            })
            .to_string(),
        )
        .expect("a manifest");
        std::fs::write(
            root.join("tool-definitions").join("rg.json"),
            json!({
                "schema": "ghostai.tool/1",
                "description": "Search the workspace.",
                "parameters": {"type": "object", "properties": {}, "additionalProperties": false},
                "implementation": {
                    "kind": "command",
                    "executable": "/usr/bin/rg",
                    "argv": ["--files"],
                },
            })
            .to_string(),
        )
        .expect("a definition");
    }

    /// One container's build context: a `Dockerfile` and the definition whose
    /// image id the build fills in.
    fn container(&self, name: &str) {
        let dir = self.catalogue().join("containers").join(name);
        std::fs::create_dir_all(&dir).expect("a container directory");
        std::fs::write(dir.join("Dockerfile"), "FROM scratch\n").expect("a Dockerfile");
        std::fs::write(
            dir.join("container.json"),
            json!({
                "schema": "ghostai.container/1",
                "name": name,
                // The placeholder the build replaces. A definition that shipped
                // a real image id would be one nobody could have built.
                "image": "__IMAGE_ID__",
            })
            .to_string(),
        )
        .expect("a definition");
    }

    /// A sheet in the catalogue's `skills/`, optionally scoped and with extras.
    fn skill(&self, name: &str, agents: Option<&str>, extras: &[(&str, &str)]) {
        let dir = self.catalogue().join("skills").join(name);
        std::fs::create_dir_all(&dir).expect("a sheet directory");
        let scope = agents.map_or_else(String::new, |value| format!("agents: {value}\n"));
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\ndescription: What {name} does.\n{scope}---\n\nBody of {name}.\n"),
        )
        .expect("a sheet");
        for (file, contents) in extras {
            std::fs::write(dir.join(file), contents).expect("an extra");
        }
    }

    /// Where a workspace's sheets land. The default workspace is `<home>/workspace`.
    fn sheet(&self, name: &str, workspace: Option<&str>) -> PathBuf {
        let base = self.home().join("workspace");
        match workspace {
            Some(id) => base.join(id).join("skills").join(name),
            None => base.join("skills").join(name),
        }
    }

    /// Where a fetch would land under this test's home.
    fn fetched(&self) -> PathBuf {
        fetched_catalogue_dir(&self.home().join("catalogue"))
    }

    /// The agents on disk.
    ///
    /// Empty when there is no config at all, which is a state this command
    /// produces on purpose: the write happens only if something installed, so a
    /// run that installed nothing leaves the file it would have created absent
    /// rather than writing an empty one.
    fn agents(&self) -> Value {
        let Ok(text) = std::fs::read_to_string(self.home().join("config.json")) else {
            return json!({});
        };
        serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|config| config.pointer("/agents/list").cloned())
            .unwrap_or_else(|| json!({}))
    }

    /// Where an install lands what it copied and built.
    fn policy(&self) -> PathBuf {
        self.home().join("policy")
    }

    fn approve(&self, name: &str) {
        let err = Sink::default();
        let mut streams = Streams {
            out: Box::new(Sink::default()),
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

    /// The other half of the decision, approved on its own.
    fn approve_container(&self, name: &str) {
        let err = Sink::default();
        let mut streams = Streams {
            out: Box::new(Sink::default()),
            err: Box::new(err.clone()),
        };
        let code = ghostai::container::run(
            &self.globals(),
            StoreAction::Approve,
            Some(name),
            &self.env(),
            &mut streams,
        )
        .expect("the approval answers with an exit code");
        assert_eq!(code, 0, "{}", err.text());
    }
}

/// What one run produced.
struct Run {
    code: u8,
    output: String,
    errors: String,
    built: Vec<PathBuf>,
    fetched: usize,
}

/// How one run differs from the ordinary one.
struct Spec<'a> {
    action: PresetAction,
    /// `None` uses the fixture catalogue; `Some(None)` passes no `--from`.
    #[allow(
        clippy::option_option,
        reason = "three cases: the fixture catalogue, a named directory, or no flag at all"
    )]
    from: Option<Option<&'a str>>,
    refresh: bool,
    offline: bool,
    workspace_id: Option<&'a str>,
    /// Lines the prompts read. Empty means there is nobody to ask.
    answers: &'a [&'a str],
    /// Makes the builder fail with this message.
    build_error: Option<&'a str>,
    /// Makes the daemon probe fail with this message.
    probe_error: Option<&'a str>,
    /// Creates a catalogue where a fetch would land, rather than refusing.
    fetch_lands: bool,
}

impl Default for Spec<'_> {
    fn default() -> Spec<'static> {
        Spec {
            action: install(&[]),
            from: None,
            refresh: false,
            offline: false,
            workspace_id: None,
            answers: &[],
            build_error: None,
            probe_error: None,
            fetch_lands: false,
        }
    }
}

/// An `install` action with the ids named and everything else at its default.
fn install(ids: &[&str]) -> PresetAction {
    PresetAction::Install {
        ids: ids.iter().map(|id| (*id).to_owned()).collect(),
        force: false,
        approve: None,
        workspace_id: None,
    }
}

/// The same, with `--force`.
fn install_forced(ids: &[&str]) -> PresetAction {
    match install(ids) {
        PresetAction::Install {
            ids,
            approve,
            workspace_id,
            ..
        } => PresetAction::Install {
            ids,
            force: true,
            approve,
            workspace_id,
        },
        other => other,
    }
}

/// The same, with the approval answered on the command line.
fn install_approving(ids: &[&str], approve: bool) -> PresetAction {
    match install(ids) {
        PresetAction::Install {
            ids,
            force,
            workspace_id,
            ..
        } => PresetAction::Install {
            ids,
            force,
            approve: Some(approve),
            workspace_id,
        },
        other => other,
    }
}

fn run(harness: &Harness, spec: Spec<'_>) -> Run {
    let out = Sink::default();
    let err = Sink::default();
    let mut streams = Streams {
        out: Box::new(out.clone()),
        err: Box::new(err.clone()),
    };

    let built: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    let fetched: Mutex<usize> = Mutex::new(0);

    let make_image = |context: &Path, _tag: &str| -> Result<String> {
        if let Some(message) = spec.build_error {
            return Err(GhostError::new(ErrorKind::Tool, message));
        }
        built
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(context.to_path_buf());
        Ok(DIGEST.to_owned())
    };
    let probe = || -> Result<()> {
        match spec.probe_error {
            Some(message) => Err(GhostError::new(ErrorKind::Tool, message)),
            None => Ok(()),
        }
    };
    let landing = harness.fetched();
    let fetch = |_argv: &[String]| -> Result<i32> {
        *fetched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        if !spec.fetch_lands {
            return Err(GhostError::new(
                ErrorKind::Internal,
                "the tests must never reach a registry",
            ));
        }
        std::fs::create_dir_all(landing.join("agents")).expect("a fetched catalogue");
        Ok(0)
    };

    let from = match spec.from {
        None => Some(harness.catalogue().to_string_lossy().into_owned()),
        Some(explicit) => explicit.map(str::to_owned),
    };

    let translations = Translations::default();
    let mut reader = ScriptedReader::new(spec.answers.iter().copied());
    let mut ask = Ask::new(&mut reader, Some(false), &translations);
    let env = harness.env();
    let globals = harness.globals();

    let mut options = PresetOptions {
        action: spec.action,
        catalogue: CatalogueArgs {
            from,
            refresh: spec.refresh,
            offline: spec.offline,
        },
        workspace_id: spec.workspace_id.map(str::to_owned),
        globals,
        env,
        t: &translations,
        ask: if spec.answers.is_empty() {
            None
        } else {
            Some(&mut ask)
        },
        build: Some(&make_image),
        probe: Some(&probe),
        fetch: Some(&fetch),
    };

    let code = run_preset(&mut options, &mut streams)
        .expect("the command answers with an exit code rather than failing");

    Run {
        code,
        output: out.text(),
        errors: err.text(),
        built: built
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        fetched: fetched
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    }
}

/// The question the approval prompt asks about the toolbox and the container
/// an agent needs — two decisions, so two pending approvals.
const APPROVE_BOTH: &str = "Approve all 2, so agents may use them?";

// list

#[test]
fn shows_every_preset_and_which_are_already_installed() {
    let harness = Harness::new();
    run(
        &harness,
        Spec {
            action: install(&["nano"]),
            ..Spec::default()
        },
    );

    let listed = run(
        &harness,
        Spec {
            action: PresetAction::List,
            ..Spec::default()
        },
    );

    assert_eq!(listed.code, 0, "{}", listed.errors);
    assert!(
        listed.output.contains("nano (Nano)  [installed]"),
        "{}",
        listed.output
    );
    assert!(
        listed.output.contains("coder (Coder)  coding"),
        "{}",
        listed.output
    );
    assert!(
        !listed.output.contains("coder (Coder)  coding  [installed]"),
        "{}",
        listed.output
    );
}

#[test]
fn shows_how_much_of_a_box_a_preset_asked_for() {
    // Two agents naming one toolbox differ only here, so it has to be on the
    // row an operator picks from.
    let harness = Harness::new();
    harness.agent(
        "scout",
        &json!({
            "toolbox": {"name": "coding", "tools": {"*": "deny", "rg": "allow"}},
        }),
    );

    let listed = run(
        &harness,
        Spec {
            action: PresetAction::List,
            ..Spec::default()
        },
    );

    // Through the plural forms, so `1` reads as one tool rather than `1 tools`.
    assert!(
        listed.output.contains("scout  coding (1 tool)"),
        "{}",
        listed.output
    );
}

// install

#[test]
fn builds_only_the_boxes_the_chosen_agents_asked_for() {
    // The whole reason the picker exists. `spare` is in the catalogue and
    // nobody named it, so neither half of it is built or installed.
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install(&["coder"]),
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    assert_eq!(
        installed.built,
        vec![harness.catalogue().join("containers").join("dev")]
    );
    assert!(
        harness
            .policy()
            .join("toolboxes")
            .join("coding.json")
            .exists()
    );
    // The definition the manifest grants travels with it: the approval hash
    // covers both, so a toolbox installed without one could never be reviewed.
    assert!(
        harness
            .policy()
            .join("tool-definitions")
            .join("rg.json")
            .exists()
    );
    assert!(
        !harness
            .policy()
            .join("toolboxes")
            .join("spare.json")
            .exists()
    );
    assert!(
        !harness
            .policy()
            .join("containers")
            .join("spare.json")
            .exists()
    );
}

#[test]
fn runs_no_builder_at_all_when_nothing_chosen_needs_a_box() {
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install(&["nano"]),
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    assert!(installed.built.is_empty());
    assert!(harness.agents().get("nano").is_some());
}

#[test]
fn pins_the_built_image_id_into_the_installed_definition() {
    let harness = Harness::new();
    run(
        &harness,
        Spec {
            action: install(&["coder"]),
            ..Spec::default()
        },
    );

    let definition = std::fs::read_to_string(harness.policy().join("containers").join("dev.json"))
        .expect("a definition was installed");
    assert!(definition.contains(DIGEST), "{definition}");
    assert!(!definition.contains("__IMAGE_ID__"), "{definition}");
}

#[test]
fn approves_nothing_when_there_is_nobody_to_ask() {
    // A pipe or a scheduled job. Answering "yes" by default would approve
    // container policy nobody read, which is the failure the gate exists to
    // stop.
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install(&["coder"]),
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    assert!(
        installed.output.contains("ghostai toolbox approve coding"),
        "{}",
        installed.output
    );
    assert!(harness.agents().get("coder").is_none());
}

#[test]
fn prints_each_policy_before_asking_so_a_yes_is_an_informed_one() {
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install(&["coder"]),
            answers: &["n"],
            ..Spec::default()
        },
    );

    let shown = &installed.output;
    let question = shown.find(APPROVE_BOTH).expect("the question was asked");
    // What the operator has seen by the time the question arrives: both halves
    // named, and the container's policy spelled out rather than summarised.
    for expected in [
        "toolbox coding",
        "container dev",
        "image      sha256:",
        "limits     ",
    ] {
        let seen = shown.find(expected).unwrap_or(usize::MAX);
        assert!(
            seen < question,
            "{expected} was not shown before the question"
        );
    }
    assert_eq!(shown.matches(APPROVE_BOTH).count(), 1, "{shown}");
}

#[test]
fn approves_and_installs_in_one_run_when_the_answer_is_yes() {
    // The point of asking here rather than after: approving is what unblocks
    // the agents, so the same run finishes the job.
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install(&["coder"]),
            answers: &["y"],
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    assert!(harness.agents().get("coder").is_some());
    assert!(
        installed.output.contains("Approved coding"),
        "{}",
        installed.output
    );
    assert!(
        installed.output.contains("Approved dev"),
        "{}",
        installed.output
    );
}

#[test]
fn approve_does_it_without_asking() {
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install_approving(&["coder"], true),
            answers: &["n"],
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    assert!(
        !installed.output.contains(APPROVE_BOTH),
        "{}",
        installed.output
    );
    assert!(harness.agents().get("coder").is_some());
}

#[test]
fn no_approve_neither_asks_nor_prints_the_policies() {
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install_approving(&["coder"], false),
            answers: &["y"],
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    assert!(
        !installed.output.contains(APPROVE_BOTH),
        "{}",
        installed.output
    );
    assert!(harness.agents().get("coder").is_none());
    assert!(
        !installed.output.contains("review this"),
        "{}",
        installed.output
    );
    assert!(
        installed.output.contains("ghostai toolbox approve coding"),
        "{}",
        installed.output
    );
}

#[test]
fn holds_back_an_agent_whose_box_is_not_approved_yet() {
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install(&["nano", "coder"]),
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    let agents = harness.agents();
    assert!(agents.get("nano").is_some());
    // An enabled agent naming an unapproved toolbox is a config the server
    // refuses to boot on, so the entry is not written at all.
    assert!(agents.get("coder").is_none());
    assert!(
        installed.output.contains("Waiting on those approvals"),
        "{}",
        installed.output
    );
}

#[test]
fn installs_a_held_back_agent_once_its_box_is_approved() {
    let harness = Harness::new();
    run(
        &harness,
        Spec {
            action: install(&["coder"]),
            ..Spec::default()
        },
    );
    harness.approve("coding");
    harness.approve_container("dev");

    let second = run(
        &harness,
        Spec {
            action: install(&["coder"]),
            ..Spec::default()
        },
    );

    assert_eq!(second.code, 0, "{}", second.errors);
    assert!(harness.agents().get("coder").is_some());
}

#[test]
fn does_not_rebuild_a_box_that_is_already_approved() {
    // Rebuilding changes the image id, changes the manifest, and revokes the
    // approval the operator gave — a re-run must not be a silent downgrade.
    let harness = Harness::new();
    run(
        &harness,
        Spec {
            action: install(&["coder"]),
            ..Spec::default()
        },
    );
    harness.approve("coding");
    harness.approve_container("dev");

    let second = run(
        &harness,
        Spec {
            action: install(&["coder"]),
            ..Spec::default()
        },
    );

    assert!(second.built.is_empty(), "{:?}", second.built);
}

#[test]
fn leaves_an_already_installed_agent_alone_rather_than_overwriting_it() {
    let harness = Harness::new();
    run(
        &harness,
        Spec {
            action: install(&["nano"]),
            ..Spec::default()
        },
    );
    let before = harness.agents();

    let second = run(
        &harness,
        Spec {
            action: install(&["nano"]),
            ..Spec::default()
        },
    );

    assert_eq!(second.code, 0, "{}", second.errors);
    assert_eq!(harness.agents(), before);
    assert!(
        second.output.contains("Re-run with --force"),
        "{}",
        second.output
    );
}

#[test]
fn force_overwrites_it() {
    let harness = Harness::new();
    run(
        &harness,
        Spec {
            action: install(&["nano"]),
            ..Spec::default()
        },
    );
    harness.agent(
        "nano",
        &json!({"label": "Renamed", "toolsEnabled": false, "tools": {}}),
    );

    let second = run(
        &harness,
        Spec {
            action: install_forced(&["nano"]),
            ..Spec::default()
        },
    );

    assert_eq!(second.code, 0, "{}", second.errors);
    assert_eq!(harness.agents()["nano"]["label"], json!("Renamed"));
}

#[test]
fn installs_delegators_last_so_their_roster_is_not_born_empty() {
    // A roster is snapshotted from the agents that exist at install time, so a
    // lead installed before its specialists would be handed an empty team. In
    // one run, ordering is the whole fix — and the ids are given in the order
    // that would break it.
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install(&["lead", "nano", "coder"]),
            answers: &["y"],
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    let agents = harness.agents();
    let roster: Vec<&str> = agents["lead"]["subagents"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry["id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(roster, vec!["coder", "nano"]);
}

#[test]
fn names_a_stale_roster_rather_than_silently_overwriting_it() {
    // The lead installs first with no specialist reachable; the second run
    // could now offer it two, but the entry may carry the operator's own edits.
    let harness = Harness::new();
    run(
        &harness,
        Spec {
            action: install(&["lead"]),
            ..Spec::default()
        },
    );
    assert_eq!(harness.agents()["lead"]["subagents"], json!([]));

    run(
        &harness,
        Spec {
            action: install(&["nano", "coder"]),
            answers: &["y"],
            ..Spec::default()
        },
    );

    let third = run(
        &harness,
        Spec {
            action: install(&["lead"]),
            ..Spec::default()
        },
    );

    assert_eq!(third.code, 0, "{}", third.errors);
    assert_eq!(harness.agents()["lead"]["subagents"], json!([]));
    assert!(
        third.output.contains("ghostai agent install lead --force"),
        "{}",
        third.output
    );
}

#[test]
fn stops_before_the_first_build_when_the_daemon_is_unreachable() {
    // One sentence up front beats five failed builds.
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install(&["coder"]),
            probe_error: Some("Cannot connect to the Docker daemon"),
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 1);
    assert!(installed.built.is_empty());
    assert!(
        installed.errors.contains("Docker daemon"),
        "{}",
        installed.errors
    );
}

#[test]
fn reports_a_failed_build_without_writing_a_half_pinned_manifest() {
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install(&["coder"]),
            build_error: Some("docker build failed for ghostai/coding:local"),
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 1);
    assert!(
        installed.errors.contains("docker build failed"),
        "{}",
        installed.errors
    );
    assert!(
        !harness
            .policy()
            .join("containers")
            .join("dev.json")
            .exists()
    );
}

#[test]
fn refuses_a_preset_naming_a_box_the_catalogue_does_not_carry() {
    let harness = Harness::new();
    harness.agent("orphan", &json!({"toolbox": {"name": "nowhere"}}));

    let installed = run(
        &harness,
        Spec {
            action: install(&["orphan"]),
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 1);
    assert!(
        installed
            .errors
            .contains("carries nothing named \"nowhere\""),
        "{}",
        installed.errors
    );
    assert!(installed.built.is_empty());
}

#[test]
fn refuses_an_id_that_is_not_on_offer_naming_what_is() {
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install(&["ghost"]),
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 1);
    assert!(
        installed.errors.contains("coder, lead, nano"),
        "{}",
        installed.errors
    );
}

// skill sheets

#[test]
fn copies_the_sheets_a_preset_names_into_the_default_workspace() {
    let harness = Harness::new();
    harness.agent("scribe", &json!({"skills": ["code-review"]}));

    let installed = run(
        &harness,
        Spec {
            action: install(&["scribe"]),
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    let sheet = harness.sheet("code-review", None);
    let page = std::fs::read_to_string(sheet.join("SKILL.md")).expect("the page was copied");
    assert!(page.contains("Body of code-review."), "{page}");
    // The directory, not just the page — an attachment is the reason a sheet is
    // a folder.
    let extra = std::fs::read_to_string(sheet.join("checklist.md")).expect("the extra was copied");
    assert_eq!(extra, "- Read it.\n");
    assert!(
        installed.output.contains("code-review  (2 files)"),
        "{}",
        installed.output
    );
}

#[test]
fn copies_nothing_for_a_preset_that_names_no_sheets() {
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            action: install(&["nano"]),
            ..Spec::default()
        },
    );

    assert!(!harness.home().join("workspace").join("skills").exists());
    assert!(
        !installed.output.contains("Skill sheets written"),
        "{}",
        installed.output
    );
}

#[test]
fn leaves_a_sheet_already_in_the_workspace_alone_and_says_so() {
    // Two presets naming one sheet, installed in separate runs. The second
    // agent is new — so it installs — and finds the sheet already there.
    let harness = Harness::new();
    harness.agent("scribe", &json!({"skills": ["code-review"]}));
    harness.agent("editor", &json!({"skills": ["code-review"]}));
    run(
        &harness,
        Spec {
            action: install(&["scribe"]),
            ..Spec::default()
        },
    );
    let page = harness.sheet("code-review", None).join("SKILL.md");
    std::fs::write(&page, "Mine.\n").unwrap();

    let second = run(
        &harness,
        Spec {
            action: install(&["editor"]),
            ..Spec::default()
        },
    );

    assert_eq!(second.code, 0, "{}", second.errors);
    assert_eq!(std::fs::read_to_string(&page).unwrap(), "Mine.\n");
    assert!(
        second
            .output
            .contains("Re-run with --force to overwrite them."),
        "{}",
        second.output
    );
}

#[test]
fn overwrites_the_sheets_with_force_alongside_the_agent() {
    // One flag, because an operator asking for the preset back means the whole
    // preset — the entry and the sheets it brought.
    let harness = Harness::new();
    harness.agent("scribe", &json!({"skills": ["code-review"]}));
    run(
        &harness,
        Spec {
            action: install(&["scribe"]),
            ..Spec::default()
        },
    );
    let page = harness.sheet("code-review", None).join("SKILL.md");
    std::fs::write(&page, "Mine.\n").unwrap();

    let second = run(
        &harness,
        Spec {
            action: install_forced(&["scribe"]),
            ..Spec::default()
        },
    );

    assert_eq!(second.code, 0, "{}", second.errors);
    assert!(
        std::fs::read_to_string(&page)
            .unwrap()
            .contains("Body of code-review.")
    );
}

#[test]
fn installs_the_agent_even_when_a_sheet_is_not_in_the_catalogue() {
    // Unlike a missing toolbox, which refuses: an agent with one fewer index
    // line runs, and an agent with no toolbox cannot.
    let harness = Harness::new();
    harness.agent("scribe", &json!({"skills": ["ghost-ops"]}));

    let installed = run(
        &harness,
        Spec {
            action: install(&["scribe"]),
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    assert!(harness.agents().get("scribe").is_some());
    assert!(
        installed
            .output
            .contains("Named by a preset but not in this catalogue:"),
        "{}",
        installed.output
    );
    assert!(
        installed.output.contains("ghost-ops"),
        "{}",
        installed.output
    );
}

#[test]
fn warns_when_a_sheet_is_scoped_away_from_the_agent_that_brought_it() {
    let harness = Harness::new();
    harness.agent("scribe", &json!({"skills": ["triage"]}));

    let installed = run(
        &harness,
        Spec {
            action: install(&["scribe"]),
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    assert!(
        installed
            .output
            .contains("skill \"triage\" is scoped to lead, so the scribe agent will not see it"),
        "{}",
        installed.output
    );
}

#[test]
fn copies_no_sheets_for_a_preset_that_was_blocked() {
    // Blocked presets are "left alone in case you have edited them", and
    // overwriting their sheets would be the same edit by another route.
    let harness = Harness::new();
    harness.agent("scribe", &json!({"skills": ["code-review"]}));
    run(
        &harness,
        Spec {
            action: install(&["scribe"]),
            ..Spec::default()
        },
    );
    std::fs::remove_dir_all(harness.home().join("workspace").join("skills")).unwrap();

    // Second run without `--force`: the agent already exists, so it blocks.
    let second = run(
        &harness,
        Spec {
            action: install(&["scribe"]),
            ..Spec::default()
        },
    );

    assert_eq!(second.code, 0, "{}", second.errors);
    assert!(!harness.sheet("code-review", None).exists());
}

#[test]
fn writes_into_a_named_workspace_and_refuses_one_that_is_absent() {
    let harness = Harness::new();
    harness.agent("scribe", &json!({"skills": ["code-review"]}));
    std::fs::create_dir_all(harness.home().join("workspace").join("acme")).unwrap();

    let named = run(
        &harness,
        Spec {
            action: install(&["scribe"]),
            workspace_id: Some("acme"),
            ..Spec::default()
        },
    );

    assert_eq!(named.code, 0, "{}", named.errors);
    assert!(harness.sheet("code-review", Some("acme")).exists());
    assert!(!harness.sheet("code-review", None).exists());

    // The path helper validates the *shape* of an id and joins; the registry is
    // in SQLite and it never asks. Without the check a typo would create a tree
    // no UI ever lists.
    let typo = run(
        &harness,
        Spec {
            action: install(&["scribe"]),
            workspace_id: Some("typo"),
            ..Spec::default()
        },
    );

    assert_eq!(typo.code, 1);
    assert!(typo.errors.contains("no typo workspace"), "{}", typo.errors);
}

// the picker

#[test]
fn installs_what_was_ticked_and_nothing_else() {
    // The listing is sorted: coder, lead, nano. `3` is nano.
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            answers: &["3"],
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    let agents = harness.agents();
    assert!(agents.get("nano").is_some());
    assert!(agents.get("coder").is_none());
    assert!(installed.built.is_empty());
}

#[test]
fn an_empty_answer_installs_nothing_and_is_not_an_error() {
    // Pressing enter is a valid way to decline, distinct from having nobody to
    // ask — which is the exit-2 case below.
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            answers: &[""],
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    assert!(
        installed.output.contains("Nothing chosen"),
        "{}",
        installed.output
    );
    assert!(!harness.home().join("config.json").exists());
}

#[test]
fn refuses_with_a_usage_message_when_there_is_nobody_to_ask() {
    let harness = Harness::new();

    let installed = run(&harness, Spec::default());

    assert_eq!(installed.code, 2);
    assert!(
        installed.errors.contains("ghostai preset list"),
        "{}",
        installed.errors
    );
}

#[test]
fn takes_a_name_as_well_as_a_number_at_the_picker() {
    // A terminal is a place people type from habit, and typing the id is one.
    let harness = Harness::new();

    let installed = run(
        &harness,
        Spec {
            answers: &["nano"],
            ..Spec::default()
        },
    );

    assert_eq!(installed.code, 0, "{}", installed.errors);
    assert!(harness.agents().get("nano").is_some());
}

// finding the catalogue

#[test]
fn refuses_a_from_that_is_not_there() {
    let harness = Harness::new();
    let missing = harness.catalogue().join("nope");

    let listed = run(
        &harness,
        Spec {
            action: PresetAction::List,
            from: Some(Some(&missing.to_string_lossy())),
            ..Spec::default()
        },
    );

    assert_eq!(listed.code, 1);
    assert!(
        listed
            .errors
            .contains(&format!("No catalogue at {}", missing.display())),
        "{}",
        listed.errors
    );
}

#[test]
fn names_the_version_when_the_layout_is_the_old_one() {
    // A checkout from before the layout settled keeps its presets under
    // `presets/` rather than `agents/`, so this is a directory that exists,
    // reads, and offers nothing. "No presets available" would send somebody
    // looking for a preset to write.
    let harness = Harness::new();
    let old = TempDir::new().unwrap();
    std::fs::create_dir_all(old.path().join("presets")).unwrap();

    let listed = run(
        &harness,
        Spec {
            action: PresetAction::List,
            from: Some(Some(&old.path().to_string_lossy())),
            ..Spec::default()
        },
    );

    assert_eq!(listed.code, 1);
    assert!(
        listed.errors.contains("holds no agents/ directory"),
        "{}",
        listed.errors
    );
    // The constant, not a copy of it: a range that moves should not need this
    // line edited to keep passing.
    assert!(listed.errors.contains(CATALOGUE_RANGE), "{}", listed.errors);
}

#[test]
fn refuses_to_fetch_under_offline_naming_the_way_out() {
    let harness = Harness::new();

    let listed = run(
        &harness,
        Spec {
            action: PresetAction::List,
            from: Some(None),
            offline: true,
            ..Spec::default()
        },
    );

    assert_eq!(listed.code, 1);
    assert!(
        listed.errors.contains("--offline forbids fetching"),
        "{}",
        listed.errors
    );
    assert_eq!(listed.fetched, 0);
}

#[test]
fn takes_from_even_with_refresh_and_does_not_fetch_over_it() {
    // `--refresh` is about the *fetched* copy. Against a checkout there is
    // nothing to refresh, and the run must not fail claiming the directory it
    // is looking at is absent.
    let harness = Harness::new();

    let listed = run(
        &harness,
        Spec {
            action: PresetAction::List,
            refresh: true,
            ..Spec::default()
        },
    );

    assert_eq!(listed.code, 0, "{}", listed.errors);
    assert_eq!(listed.errors, "");
    assert_eq!(listed.fetched, 0);
    assert!(listed.output.contains("coder"), "{}", listed.output);
}

// update

#[test]
fn update_always_fetches_even_when_a_copy_is_already_here() {
    // The bug this pins: `update` used to find the copy under the prefix,
    // answer with it, and report success having fetched nothing at all.
    let harness = Harness::new();

    let updated = run(
        &harness,
        Spec {
            action: PresetAction::Update,
            // No `--from`, so it goes looking under the home for a fetched copy.
            from: Some(None),
            fetch_lands: true,
            ..Spec::default()
        },
    );

    assert_eq!(updated.code, 0, "{}", updated.errors);
    assert_eq!(updated.fetched, 1);
    assert!(
        updated.output.contains("Catalogue at"),
        "{}",
        updated.output
    );
}

#[test]
fn update_refuses_to_update_a_checkout() {
    let harness = Harness::new();

    let updated = run(
        &harness,
        Spec {
            action: PresetAction::Update,
            ..Spec::default()
        },
    );

    assert_eq!(updated.code, 1);
    assert!(updated.errors.contains("nothing"), "{}", updated.errors);
    assert!(updated.errors.contains("git"), "{}", updated.errors);
    assert_eq!(updated.fetched, 0);
}

// the shape the paths take

#[test]
fn preset_paths_default_to_nothing_rather_than_to_a_guess() {
    // The type every install path shares. Absent directories mean "look only at
    // what the operator has", which is the state a fresh install is in.
    let paths = PresetPaths::default();
    assert!(paths.catalogue_agents_dir.is_none());
    assert!(paths.catalogue_skills_dir.is_none());
    assert!(paths.skills_dir.is_none());
}

// the process entry point

#[tokio::test]
async fn the_process_entry_point_lists_against_the_catalogue_it_was_pointed_at() {
    // The wiring above `run_preset`: it decides whether there is somebody to
    // ask, opens the real stdin only to answer that, and hands everything else
    // over. Under a test runner there is no terminal, so the prompts are
    // absent and the run has to be complete without them.
    let harness = Harness::new();
    let out = Sink::default();
    let err = Sink::default();
    let mut streams = Streams {
        out: Box::new(out.clone()),
        err: Box::new(err.clone()),
    };

    let code = ghostai::preset::run(
        &harness.globals(),
        &CatalogueArgs {
            from: Some(harness.catalogue().to_string_lossy().into_owned()),
            refresh: false,
            offline: false,
        },
        PresetAction::List,
        &harness.env(),
        &mut streams,
    )
    .await
    .expect("the command answers with an exit code rather than failing");

    assert_eq!(code, 0, "{}", err.text());
    assert!(out.text().contains("coder"), "{}", out.text());
}

#[tokio::test]
async fn the_process_entry_point_refuses_offline_with_no_catalogue_to_read() {
    // `--offline` means "do not fetch", and with nothing on disk there is
    // nothing to install from — a refusal naming the way out rather than a
    // silent no-op.
    let harness = Harness::new();
    let out = Sink::default();
    let err = Sink::default();
    let mut streams = Streams {
        out: Box::new(out.clone()),
        err: Box::new(err.clone()),
    };

    let code = ghostai::preset::run(
        &harness.globals(),
        &CatalogueArgs {
            from: None,
            refresh: false,
            offline: true,
        },
        PresetAction::List,
        &harness.env(),
        &mut streams,
    )
    .await
    .expect("the command answers with an exit code rather than failing");

    assert_eq!(code, 1);
    assert!(err.text().contains("--from"), "{}", err.text());
}

#[test]
fn the_options_keep_the_injected_hooks_out_of_the_debug_output() {
    // A builder, a probe and a fetcher are closures with no useful rendering;
    // what a log line wants is which action ran against which catalogue.
    let harness = Harness::new();
    let translations = Translations::default();
    let rendered = format!(
        "{:?}",
        PresetOptions {
            action: install(&["coder"]),
            catalogue: CatalogueArgs {
                from: Some("/tmp/checkout".to_owned()),
                refresh: false,
                offline: false,
            },
            workspace_id: Some("default".to_owned()),
            globals: harness.globals(),
            env: harness.env(),
            t: &translations,
            ask: None,
            build: None,
            probe: None,
            fetch: None,
        }
    );

    assert!(rendered.contains("PresetOptions"), "{rendered}");
    assert!(rendered.contains("/tmp/checkout"), "{rendered}");
    assert!(rendered.contains("default"), "{rendered}");
}
