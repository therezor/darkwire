//! `ghostai agent` — install agent presets, and list agents and presets.
//!
//! `install` is a config merge, not a package manager. A preset is a YAML file
//! already on the box — laid down by the catalogue, or written by an operator —
//! and installing it writes one entry into `agents.list` in `config.yaml`.
//! Nothing is fetched, and after the write the entry is ordinary agent config
//! the web UI edits like any other.
//!
//! The argument is either a path — anything with a separator, a `.yaml` suffix,
//! or that exists as a file — or a preset id looked up in the directories
//! [`crate::presets::preset_dirs`] names, operator's before the catalogue's.
//! There is one preset format and one place ids are searched; an agent that
//! needs a container is not a different kind of preset, it is a preset whose
//! `toolbox.name` and `container.name` are set. The two are independent: a
//! preset may name either, both or neither.
//!
//! Two refusals do the real work:
//!
//!  - **A preset naming a toolbox that does not resolve is refused.** An enabled
//!    agent whose toolbox fails to resolve makes the runtime's build fail, so
//!    accepting the entry would write a config the server refuses to boot on.
//!    The refusal happens here, where the message can name the fix.
//!  - **An id that already exists needs `--force`.** The existing entry may
//!    carry an operator's edits, and a re-run of the catalogue's build plus a
//!    reinstall must not destroy them silently.
//!
//! A preset's `subagents` list is a roster of *other* preset agents, filtered
//! at install time to the ones installed and enabled — the model is never
//! offered a specialist that cannot answer. Re-running with `--force` after
//! installing more of them refreshes the snapshot.

use std::io::Write;
use std::path::{Path, PathBuf};

use ghostai_core::{
    ErrorKind, GhostError, GhostPaths, LoadConfigOptions, Result, load_config, save_config,
};
use ghostai_protocol::{
    AgentPreset, Config, DEFAULT_AGENT_ID, DEFAULT_WORKSPACE_ID, NetworkMode, RESERVED_AGENT_IDS,
    SubagentRef, is_agent_id, preset_to_agent_entry,
};
use ghostai_security::{
    PolicyStore, assert_container_network, assert_gateway_compatible, toolbox::invalid,
};

use crate::Streams;
use crate::catalogue::{
    CatalogueOptions, catalogue_agents_dir, catalogue_dir, catalogue_skills_dir,
};
use crate::i18n::Env;
use crate::presets::{find_preset, list_all_presets, preset_dirs, read_preset};
use crate::program::{AgentCommand, Globals};
use crate::runtime::load_options;
use crate::skill_install::{SkillInstallRequest, install_skills, skills_target_dir};

/// The slice of [`GhostPaths`] this command reads, plus nothing else.
///
/// Named rather than taken whole so a test can point the directories wherever
/// it likes without standing up a whole resolved install.
#[derive(Debug, Clone, Default)]
pub struct PresetPaths {
    /// The operator's policy directory: toolboxes, containers and operation
    /// definitions.
    pub policy_dir: PathBuf,
    /// `<root>/presets` — an operator's own drop-in directory.
    pub presets_dir: PathBuf,
    /// The one SQLite file.
    pub db_file: PathBuf,
    /// The catalogue's `agents/`, when there is a catalogue.
    ///
    /// `None` is the ordinary state on a fresh install and narrows the search
    /// to the operator's own presets rather than failing.
    pub catalogue_agents_dir: Option<PathBuf>,
    /// The catalogue's `skills/`, when it has one.
    ///
    /// `None` skips the copy and reports every sheet a preset names as missing
    /// — which is the ordinary state for an operator's own preset on a box with
    /// no catalogue, not an error.
    pub catalogue_skills_dir: Option<PathBuf>,
    /// `<workspace>/skills` for this run. `None` skips the copy.
    pub skills_dir: Option<PathBuf>,
}

/// The preset one argument names: a file, or an id in the search path.
///
/// A separator or a `.yaml` suffix is an explicit path even when the file is
/// missing — resolving `./typo.yaml` to a catalogue preset would install
/// something other than what was named.
pub fn resolve_preset(arg: &str, paths: &PresetPaths) -> Result<AgentPreset> {
    let explicit = arg.contains('/')
        || arg.contains('\\')
        || Path::new(arg)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("yaml"));
    if explicit || Path::new(arg).exists() {
        return read_preset(Path::new(arg));
    }

    let dirs = preset_dirs(&paths.presets_dir, paths.catalogue_agents_dir.as_deref());
    if let Some(found) = find_preset(&dirs, arg) {
        return read_preset(&found);
    }

    let available = list_all_presets(&dirs);
    Err(GhostError::new(
        ErrorKind::InvalidInput,
        format!(
            "No preset is available under \"{arg}\".\n  Pass a preset file, or one of: {}.",
            available.join(", ")
        ),
    )
    .with_detail("name", arg)
    .with_detail("available", available))
}

/// The rule a settings save applies to an agent id, applied here.
///
/// A preset arrives from disk rather than from the form that enforces them, so
/// the check that the web UI gets for free has to be made explicitly.
fn assert_installable_id(id: &str) -> Result<()> {
    if id == DEFAULT_AGENT_ID {
        return Ok(());
    }
    if is_agent_id(id) && !RESERVED_AGENT_IDS.contains(&id) {
        return Ok(());
    }
    Err(GhostError::new(
        ErrorKind::InvalidInput,
        format!(
            "\"{id}\" cannot be used as an agent id.\n  Ids are lower-case letters, digits and \
             hyphens, up to 40 characters,\n  and cannot be a reserved device name."
        ),
    )
    .with_detail("agentId", id))
}

/// The write one preset would make.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadyInstall {
    /// The preset, as read.
    pub preset: AgentPreset,
    /// The whole settings tree with the entry merged in, ready to save.
    pub config: Config,
    /// Whether an entry of the same id was replaced.
    pub overwrote: bool,
    /// Subagents left out because they are not installed or not enabled.
    pub skipped: Vec<String>,
    /// How many tools the entry grants.
    pub tools: usize,
}

/// What one preset would do to the config, or why it cannot.
///
/// Every rule an install applies lives behind one function, because there are
/// two callers — `ghostai agent install` and the bulk installer — and a rule
/// that existed twice would be a rule that disagrees with itself. The single
/// install turns a [`InstallPlan::Blocked`] into a failure; the bulk one turns
/// it into a line in its report and carries on.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallPlan {
    /// The preset can be written.
    ///
    /// Boxed because the settings tree inside it is large and the other variant
    /// is two strings; an enum sized for the larger one would be moved around
    /// on every blocked preset too.
    Ready(Box<ReadyInstall>),
    /// The preset cannot be written, and why.
    Blocked {
        /// The preset's id.
        id: String,
        /// What to tell the operator. Already a whole sentence.
        reason: String,
    },
}

/// The policy checks the runtime's build applies, run before the write.
///
/// An entry that fails them is a config the server refuses to boot on, so it is
/// better to find out here, where the message can name the fix.
fn check_policy(preset: &AgentPreset, paths: &PresetPaths) -> Result<()> {
    let store = PolicyStore::new(paths.policy_dir.clone());
    if !preset.container.name.is_empty() {
        if preset.toolbox.name.is_empty() {
            return Err(invalid(
                "A container only hosts a toolbox's approved operations; this preset names no toolbox",
            ));
        }
        let container = store.require_container(&preset.container.name)?;
        if preset.container.network.mode == NetworkMode::Allowlist {
            assert_gateway_compatible(&container.definition)?;
        }
    } else if preset.container.network.mode != NetworkMode::None {
        return Err(invalid(
            "This preset asks for a network but names no container; egress is enforced by the container's gateway",
        ));
    }
    assert_container_network(&preset.container.network, &preset.id)?;
    if preset.toolbox.name.is_empty() {
        return Ok(());
    }
    store.require_toolbox(&preset.toolbox.name).map(|_| ())
}

/// What installing one preset into `config` would do.
///
/// An unusable id fails outright rather than blocking, because it is a mistake
/// in the preset file rather than a state of this machine: no approval and no
/// re-run will make `CON` into an agent id, so there is nothing for a report to
/// tell the operator to go and do.
pub fn plan_install(
    preset: &AgentPreset,
    config: &Config,
    paths: &PresetPaths,
    force: bool,
) -> Result<InstallPlan> {
    assert_installable_id(&preset.id)?;

    // Unconditionally, not only when a name is set. A preset naming neither a
    // toolbox nor a container can still ask for egress, and skipping the check
    // for it let exactly the config this guard exists to catch reach
    // `config.yaml` and fail the next boot instead.
    if let Err(error) = check_policy(preset, paths) {
        return Ok(InstallPlan::Blocked {
            id: preset.id.clone(),
            reason: error.message,
        });
    }

    let existing = config.agents.list.get(&preset.id);
    if existing.is_some() && !force {
        return Ok(InstallPlan::Blocked {
            id: preset.id.clone(),
            reason: format!(
                "An agent named \"{}\" already exists and may carry your own edits.\n  Re-run with \
                 --force to overwrite it with the preset.",
                preset.id
            ),
        });
    }

    let enabled = |reference: &SubagentRef| {
        config
            .agents
            .list
            .get(&reference.id)
            .is_some_and(|entry| entry.enabled)
    };
    // The roster snapshot: only specialists that are installed and enabled are
    // offered to the model. `--force` re-runs refresh it.
    let roster: Vec<SubagentRef> = preset
        .subagents
        .iter()
        .filter(|reference| enabled(reference))
        .cloned()
        .collect();
    let skipped: Vec<String> = preset
        .subagents
        .iter()
        .filter(|reference| !enabled(reference))
        .map(|reference| reference.id.clone())
        .collect();

    let mut entry = preset_to_agent_entry(preset);
    // The model and the endpoint come from the default agent, not from the
    // preset. A preset ships neither on purpose — one naming a model would
    // break on every machine that lacks it — and with nothing to inherit from,
    // an entry written without them would install an agent that cannot run a
    // turn. Materialising them here is what the browser's Duplicate already
    // does.
    if let Some(seed) = config.agents.list.get(DEFAULT_AGENT_ID) {
        entry.settings.model.clone_from(&seed.settings.model);
        entry.settings.provider.clone_from(&seed.settings.provider);
    }
    entry.subagents = roster;
    let tools = entry.tools.len();

    let mut next = config.clone();
    next.agents.list.insert(preset.id.clone(), entry);

    Ok(InstallPlan::Ready(Box::new(ReadyInstall {
        preset: preset.clone(),
        config: next,
        overwrote: existing.is_some(),
        skipped,
        tools,
    })))
}

/// One line to the answer stream.
fn line(out: &mut dyn Write, text: &str) -> Result<()> {
    writeln!(out, "{text}").map_err(GhostError::from)
}

fn install(
    name: &str,
    force: bool,
    config: &Config,
    file: &Path,
    paths: &PresetPaths,
    streams: &mut Streams,
) -> Result<u8> {
    let preset = resolve_preset(name, paths)?;
    let plan = plan_install(&preset, config, paths, force)?;
    let ready = match plan {
        InstallPlan::Ready(ready) => ready,
        InstallPlan::Blocked { id, reason } => {
            return Err(GhostError::new(ErrorKind::InvalidInput, reason).with_detail("agentId", id));
        }
    };

    save_config(file, &ready.config)?;

    let out = &mut streams.out;
    let overwrote = if ready.overwrote {
        " (overwrote the existing agent)"
    } else {
        ""
    };
    line(out, &format!("Installed {}{overwrote}:", preset.id))?;
    let label = if preset.label.is_empty() {
        &preset.id
    } else {
        &preset.label
    };
    line(out, &format!("    label      {label}"))?;
    let toolbox = if preset.toolbox.name.is_empty() {
        "none — no toolbox selected"
    } else {
        &preset.toolbox.name
    };
    line(out, &format!("    toolbox    {toolbox}"))?;
    line(out, &format!("    tools      {} granted", ready.tools))?;

    let roster: Vec<&str> = ready
        .config
        .agents
        .list
        .get(&preset.id)
        .map(|entry| {
            entry
                .subagents
                .iter()
                .map(|reference| reference.id.as_str())
                .collect()
        })
        .unwrap_or_default();
    if !roster.is_empty() {
        line(out, &format!("    delegates  {}", roster.join(", ")))?;
    }
    for id in &ready.skipped {
        line(
            out,
            &format!(
                "    skipped    subagent \"{id}\" — not installed or disabled. Install it, then \
                 re-run with --force to refresh the roster."
            ),
        )?;
    }

    // After the config write, and reported rather than enforced: a sheet that
    // does not arrive costs the agent one index line, so nothing here is
    // allowed to turn a successful install into a failed one.
    let sheets = install_skills(&SkillInstallRequest {
        preset_id: &preset.id,
        names: &preset.skills,
        catalogue_skills_dir: paths.catalogue_skills_dir.as_deref(),
        target_dir: paths.skills_dir.as_deref(),
        force,
    });
    if !sheets.written.is_empty() {
        let names: Vec<&str> = sheets
            .written
            .iter()
            .map(|sheet| sheet.name.as_str())
            .collect();
        line(out, &format!("    skills     {}", names.join(", ")))?;
    }
    if !sheets.kept.is_empty() {
        line(
            out,
            &format!(
                "    kept       {} — already in the workspace. Re-run with --force to overwrite \
                 them.",
                sheets.kept.join(", ")
            ),
        )?;
    }
    for name in &sheets.missing {
        // The common case here rather than an edge: this command never fetches,
        // so an operator's own preset on a box with no catalogue lands entirely
        // in this branch. The fix is the other command.
        line(
            out,
            &format!(
                "    skipped    skill \"{name}\" — not in this catalogue. Run `ghostai preset \
                 update` to fetch a current one."
            ),
        )?;
    }
    for warning in &sheets.warnings {
        line(out, &format!("    note       {warning}"))?;
    }
    line(out, "")?;
    line(
        out,
        "Edit it any time in the web UI under Agents. A running server picks",
    )?;
    line(out, "this up on its next settings save or restart.")?;
    Ok(0)
}

fn list(config: &Config, paths: &PresetPaths, streams: &mut Streams) -> Result<u8> {
    let out = &mut streams.out;

    if config.agents.list.is_empty() {
        line(
            out,
            "No agents are configured; sessions run as the built-in default.",
        )?;
    }
    for (id, entry) in &config.agents.list {
        let state = if entry.enabled { "enabled" } else { "disabled" };
        line(out, &format!("{id}  [{state}]"))?;
        if !entry.label.is_empty() {
            line(out, &format!("    label      {}", entry.label))?;
        }
        if !entry.toolbox.name.is_empty() {
            line(out, &format!("    toolbox    {}", entry.toolbox.name))?;
        }
        if !entry.subagents.is_empty() {
            let ids: Vec<&str> = entry
                .subagents
                .iter()
                .map(|reference| reference.id.as_str())
                .collect();
            line(out, &format!("    delegates  {}", ids.join(", ")))?;
        }
        line(out, "")?;
    }

    // The filename is the agent id, so an id already in `agents.list` is one
    // already installed — no file has to be opened to know that.
    let dirs = preset_dirs(&paths.presets_dir, paths.catalogue_agents_dir.as_deref());
    let available: Vec<String> = list_all_presets(&dirs)
        .into_iter()
        .filter(|id| !config.agents.list.contains_key(id))
        .collect();
    if !available.is_empty() {
        line(
            out,
            &format!("Presets not yet installed: {}", available.join(", ")),
        )?;
        // One line rather than one per id, and it names the *other* command,
        // because that is the one that also builds the toolbox an agent needs.
        // `ghostai agent install <id>` still works and is what a script wants;
        // a person picking from a list wants the picker.
        line(out, "    ghostai preset install")?;
    }
    Ok(0)
}

/// [`PresetPaths`] for this invocation, with the catalogue looked up once.
pub fn preset_paths_of(
    paths: &GhostPaths,
    workspace_id: Option<&str>,
    env: &Env,
) -> Result<PresetPaths> {
    let dir = catalogue_dir(&CatalogueOptions {
        catalogue_dir: Some(paths.catalogue_dir.clone()),
        env: env.clone(),
        ..CatalogueOptions::default()
    });
    Ok(PresetPaths {
        policy_dir: paths.policy_dir.clone(),
        presets_dir: paths.presets_dir.clone(),
        db_file: paths.db_file.clone(),
        catalogue_agents_dir: dir.as_deref().and_then(catalogue_agents_dir),
        catalogue_skills_dir: dir.as_deref().and_then(catalogue_skills_dir),
        skills_dir: Some(skills_target_dir(
            paths,
            workspace_id.unwrap_or(DEFAULT_WORKSPACE_ID),
        )?),
    })
}

/// Runs one `ghostai agent` invocation and answers with its exit code.
pub fn run(
    globals: &Globals,
    command: &AgentCommand,
    env: &Env,
    streams: &mut Streams,
) -> Result<u8> {
    match act(globals, command, env, streams) {
        Ok(code) => Ok(code),
        Err(error) => {
            // A `GhostError` message is written to be read by the person who
            // caused it — it already names the file and what to do next.
            let _ = writeln!(streams.err, "{}", error.message);
            Ok(1)
        }
    }
}

fn act(globals: &Globals, command: &AgentCommand, env: &Env, streams: &mut Streams) -> Result<u8> {
    let workspace_id = match command {
        AgentCommand::Install { workspace_id, .. } => workspace_id.as_deref(),
        AgentCommand::List => None,
    };

    let loaded = load_config(LoadConfigOptions {
        paths: load_options(globals, None, env),
        file: None,
    })?;
    let paths = preset_paths_of(&loaded.paths, workspace_id, env)?;

    match command {
        AgentCommand::List => list(&loaded.config, &paths, streams),
        AgentCommand::Install { name, force, .. } => {
            if name.is_empty() {
                writeln!(
                    streams.err,
                    "Which preset? Pass a preset id or a file — see `ghostai agent list`."
                )
                .map_err(GhostError::from)?;
                return Ok(2);
            }
            install(name, *force, &loaded.config, &loaded.file, &paths, streams)
        }
    }
}
