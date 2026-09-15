//! `ghostai preset` — pick agents from the catalogue, and get the boxes they need.
//!
//! **A toolbox is built because an agent asked for it, never on its own.** The
//! selection is a list of agents; the boxes fall out of `toolbox.name` on the
//! ones chosen. That is why there is no "presets only" flag: choosing only
//! agents that need no container *is* the flag, and it is a checkbox rather
//! than something to remember.
//!
//! **It stops short of approving anything unless somebody says so, and prints
//! the policy before it asks.** Building an image and installing its manifest
//! are reversible, mechanical steps; approving one is a statement that a person
//! read what that container may do — its network ceiling, the capabilities it
//! adds back, the hardening it switches off. A prompt showing only names would
//! make that sentence false, so this run ends up printing a screen of policy
//! before a `y`. That is the right trade for the one action here that re-running
//! cannot undo.
//!
//! Everything that touches the world is injected — the fetcher, the image
//! builder, the daemon probe, the prompts — so the tests need neither a
//! registry nor a daemon nor a terminal.

use std::io::Write;
use std::path::{Path, PathBuf};

use ghostai_core::{
    ErrorKind, GhostError, LoadConfigOptions, LoadedConfig, Result, ensure_dir, load_config,
    save_config,
};
use ghostai_environment::container_pool::{DockerEngineOptions, docker_engine};
use ghostai_i18n::{args, keys};
use ghostai_protocol::{
    AgentEntry, AgentPreset, Config, DEFAULT_WORKSPACE_ID, TOOLBOX_DEFAULT_KEY, ToolPermission,
};
use ghostai_security::{PolicyStore, parse_toolbox};

use crate::Streams;
use crate::agent::{InstallPlan, PresetPaths, plan_install};
use crate::ask::Ask;
use crate::catalogue::{
    CATALOGUE_PACKAGE, CatalogueOptions, FetchCatalogueOptions, Fetcher, assert_catalogue_layout,
    catalogue_container, catalogue_definition, catalogue_dir, catalogue_skills_dir,
    catalogue_toolbox, fetch_catalogue,
};
use crate::i18n::{Env, Translations};
use crate::presets::{find_preset, list_all_presets, preset_dirs, read_preset};
use crate::program::{CatalogueArgs, Globals, PresetAction};
use crate::runtime::load_options;
use crate::skill_install::{
    SkillInstallRequest, SkillInstallResult, WrittenSheet, install_skills, skills_target_dir,
};

/// The placeholder a catalogue's container definition carries where the image
/// id goes.
///
/// The catalogue's own build script writes the same token, so a definition that
/// shipped a real image id would be one nobody could have built.
pub const IMAGE_PLACEHOLDER: &str = "__IMAGE_ID__";

/// Builds one image and answers with its id.
///
/// Injected, so the tests exercise every branch around it without a daemon. The
/// default streams the build rather than buffering it — an image that takes
/// four minutes with no output looks hung — and takes no deadline, because a
/// first build downloads base layers over whatever link the machine has.
pub type ImageBuilder<'a> = &'a (dyn Fn(&Path, &str) -> Result<String> + 'a);

/// Checks that a container runtime is reachable. Injected in tests.
pub type DaemonProbe<'a> = &'a (dyn Fn() -> Result<()> + 'a);

/// Everything one `ghostai preset` run is injected with.
///
/// The four seams below the flags are what let the whole command be driven
/// without a registry, a daemon or a terminal. Each defaults to the real thing
/// when [`run`] builds these.
pub struct PresetOptions<'a> {
    /// What to do.
    pub action: PresetAction,
    /// The three catalogue flags every subcommand carries.
    pub catalogue: CatalogueArgs,
    /// Which workspace a preset's skill sheets are copied into.
    ///
    /// `None` is `default`. A preset is workspace-agnostic — it writes an
    /// `agents.list` entry, which every workspace shares — but a sheet lives in
    /// one, so this is the one thing an install has to be told.
    pub workspace_id: Option<String>,
    /// `--home`, and the colour setting the prompts read.
    pub globals: Globals,
    /// The environment the catalogue overrides are read from.
    pub env: Env,
    /// The bundle the prose comes from.
    pub t: &'a Translations,
    /// The prompts. `None` means nothing here is interactive: a pipe, a
    /// scheduled job, or a run that named its ids on the command line.
    ///
    /// Passed in rather than opened here because a caller may already hold a
    /// reader on the same stdin, and two readers fight over the same bytes.
    pub ask: Option<&'a mut Ask<'a>>,
    /// Builds one image. `None` shells out to a container build.
    pub build: Option<ImageBuilder<'a>>,
    /// Checks the daemon. `None` probes the real one.
    pub probe: Option<DaemonProbe<'a>>,
    /// Fetches the catalogue. `None` runs the real package manager.
    pub fetch: Option<Fetcher<'a>>,
}

impl std::fmt::Debug for PresetOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresetOptions")
            .field("action", &self.action)
            .field("catalogue", &self.catalogue)
            .field("workspace_id", &self.workspace_id)
            .finish_non_exhaustive()
    }
}

/// The `--force` flag, which only `install` carries.
fn force_of(action: &PresetAction) -> bool {
    match action {
        PresetAction::Install { force, .. } => *force,
        PresetAction::List | PresetAction::Update => false,
    }
}

/// The ids named on the command line, which only `install` carries.
fn ids_of(action: &PresetAction) -> &[String] {
    match action {
        PresetAction::Install { ids, .. } => ids,
        PresetAction::List | PresetAction::Update => &[],
    }
}

/// Builds one image with the container CLI, and answers with its pinned id.
///
/// `--iidfile` rather than reading the build's own output: the latter is racy
/// when two builds run and reports a short id that cannot be pinned. The same
/// argument the catalogue's build script makes, which is the other
/// implementation of this.
fn container_build(context: &Path, tag: &str) -> Result<String> {
    let safe: String = tag
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    let iid_file = std::env::temp_dir().join(format!("ghostai-iid-{safe}"));

    let status = std::process::Command::new("docker")
        .args(["build", "--iidfile"])
        .arg(&iid_file)
        .args(["-t", tag])
        .arg(context)
        .status()
        .map_err(|error| {
            GhostError::new(ErrorKind::Tool, format!("Could not run docker: {error}"))
                .with_source(error)
        })?;
    if !status.success() {
        return Err(GhostError::new(
            ErrorKind::Tool,
            format!("docker build failed for {tag}"),
        ));
    }

    let id = std::fs::read_to_string(&iid_file)
        .map_err(|error| {
            GhostError::new(
                ErrorKind::Tool,
                format!("docker build reported success but wrote no image id for {tag}"),
            )
            .with_source(error)
        })?
        .trim()
        .to_owned();
    if !is_pinned_image(&id) {
        return Err(GhostError::new(
            ErrorKind::Tool,
            format!("Unexpected image id for {tag}: {id}"),
        ));
    }
    Ok(id)
}

/// Whether a build reported a digest rather than a tag.
///
/// A definition is only worth approving if the image it names cannot move, so
/// an id that is not `sha256:` plus sixty-four hex digits is refused rather
/// than pinned into a definition an operator would then approve.
fn is_pinned_image(id: &str) -> bool {
    let Some(hex) = id.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Everything an operator has to weigh before approving a container.
///
fn copy_policy_file(source: &Path, target: &Path) -> Result<()> {
    ensure_dir(target.parent().unwrap_or(target))?;
    std::fs::copy(source, target).map(|_| ()).map_err(|error| {
        GhostError::new(
            ErrorKind::Storage,
            format!(
                "{} could not be copied to {}",
                source.display(),
                target.display()
            ),
        )
        .with_source(error)
    })
}

/// Installs one toolbox and every operation definition it names.
///
/// The definitions travel with it rather than being installed on their own,
/// because the approval hash covers all of them: a toolbox whose definitions
/// were missing would resolve to a refusal rather than to something an operator
/// could review.
fn install_toolbox(name: &str, catalogue: &Path, policy_dir: &Path) -> Result<()> {
    let Some(source) = catalogue_toolbox(catalogue, name) else {
        return Ok(());
    };
    let bytes = std::fs::read(&source).map_err(|error| {
        GhostError::new(
            ErrorKind::Config,
            format!("{} could not be read", source.display()),
        )
        .with_source(error)
    })?;
    let toolbox = parse_toolbox(&bytes)?;
    for grant in &toolbox.tools {
        let Some(definition) = catalogue_definition(catalogue, &grant.definition) else {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "Toolbox \"{name}\" grants \"{}\" from definition \"{}\", which this \
                     catalogue does not carry.\n  Update the catalogue with `ghostai preset \
                     update`.",
                    grant.name, grant.definition
                ),
            )
            .with_detail("definition", grant.definition.clone()));
        };
        copy_policy_file(
            &definition,
            &policy_dir
                .join("tool-definitions")
                .join(format!("{}.yaml", grant.definition)),
        )?;
    }
    copy_policy_file(
        &source,
        &policy_dir.join("toolboxes").join(format!("{name}.yaml")),
    )
}

/// Builds one container image and installs its definition. Never approves it.
fn install_container(
    name: &str,
    context: &Path,
    policy_dir: &Path,
    build: ImageBuilder<'_>,
) -> Result<()> {
    let image_id = build(context, &format!("ghostai/{name}:local"))?;

    // The placeholder is replaced rather than the file generated, so the
    // definition an operator reviews in the catalogue is the definition that
    // gets installed apart from one field.
    let source = context.join("container.yaml");
    let definition = std::fs::read_to_string(&source)
        .map_err(|error| {
            GhostError::new(
                ErrorKind::Config,
                format!("{} could not be read", source.display()),
            )
            .with_source(error)
        })?
        .replace(IMAGE_PLACEHOLDER, &image_id);
    let target = policy_dir.join("containers").join(format!("{name}.yaml"));
    ensure_dir(target.parent().unwrap_or(&target))?;
    std::fs::write(&target, definition).map_err(|error| {
        GhostError::new(
            ErrorKind::Storage,
            format!("{} could not be written", target.display()),
        )
        .with_source(error)
    })
}

/// One row of the picker, and everything the report needs about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    /// The preset's id, which is also its filename.
    pub id: String,
    /// The preset, as read.
    pub preset: AgentPreset,
    /// Whether `agents.list` already holds an entry of this id.
    pub installed: bool,
}

/// Every preset on offer, operator's own and the catalogue's, deduplicated.
///
/// Reads each one, unlike `ghostai agent list` which reads only names: this has
/// to show the toolbox and the label, and it is about to install them anyway. A
/// file that does not parse is a line in the report rather than the end of the
/// run — one broken preset in a directory must not hide the other seven.
fn offers(paths: &PresetPaths, config: &Config, warnings: &mut Vec<String>) -> Vec<Offer> {
    let dirs = preset_dirs(&paths.presets_dir, paths.catalogue_agents_dir.as_deref());
    let mut found = Vec::new();
    for id in list_all_presets(&dirs) {
        // Operator's directory first, so a local preset of the same name wins —
        // the same resolution `ghostai agent install <id>` uses, through the
        // same function, because two answers to "which file is `coder`" is a
        // bug waiting for somebody to hit it.
        let Some(path) = find_preset(&dirs, &id) else {
            continue;
        };
        match read_preset(&path) {
            Ok(preset) => found.push(Offer {
                installed: config.agents.list.contains_key(&id),
                id,
                preset,
            }),
            Err(error) => {
                warnings.push(format!(
                    "    skipped    {id} — {}",
                    first_line(&error.message)
                ));
            }
        }
    }
    found
}

/// The first line of a multi-line message, for a report that wants one.
fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or(message)
}

/// `<id> (<label>)  <toolbox>`, which is what the choice actually turns on.
fn label_of(offer: &Offer, t: &Translations) -> String {
    let label = if offer.preset.label.is_empty() {
        &offer.id
    } else {
        &offer.preset.label
    };
    let name = if label == &offer.id {
        String::new()
    } else {
        format!(" ({label})")
    };
    let box_name = if offer.preset.toolbox.name.is_empty() {
        String::new()
    } else {
        format!(
            "  {}{}",
            offer.preset.toolbox.name,
            describe_grant(&offer.preset, t)
        )
    };
    format!("{}{name}{box_name}", offer.id)
}

/// How much of the box this preset asked for, when it did not ask for all of it.
///
/// Worth a few characters in the picker because it is the difference between
/// two agents that name the same toolbox — and because an operator scanning the
/// list has no other way to see that one of them is getting four programs of
/// twenty-four.
fn describe_grant(preset: &AgentPreset, t: &Translations) -> String {
    let overrides = &preset.toolbox.tools;
    if !overrides.contains_key(TOOLBOX_DEFAULT_KEY) {
        return String::new();
    }
    let named = overrides
        .iter()
        .filter(|(name, permission)| {
            name.as_str() != TOOLBOX_DEFAULT_KEY && **permission != ToolPermission::Deny
        })
        .count();
    format!(" ({})", t.tr(keys::preset::TOOLS, args!["count" => named]))
}

/// The catalogue this run reads, fetching it first when that is called for.
///
/// Three questions in order, and the order is the whole of it:
///
///  1. **Was a directory named?** Then that is the answer or the refusal, and
///     nothing is fetched either way. Somebody pointing `--from` at a checkout
///     is testing *that checkout*, and quietly using a fetched copy instead is
///     how a preset gets published without ever having been run. `--refresh`
///     and `update` have nothing to do against one, so they say so rather than
///     appearing to work.
///  2. **Is a copy here, and is it good enough?** `--refresh` and
///     `ghostai preset update` are the two ways of saying it is not.
///  3. Otherwise fetch, unless `--offline` forbids it.
fn locate(
    options: &PresetOptions<'_>,
    catalogue_root: &Path,
    out: &mut dyn Write,
) -> Result<PathBuf> {
    let from = options.catalogue.from.as_deref().unwrap_or_default();

    if !from.is_empty() {
        let found = catalogue_dir(&CatalogueOptions {
            from: Some(from.to_owned()),
            env: options.env.clone(),
            ..CatalogueOptions::default()
        });
        let Some(found) = found else {
            return Err(
                GhostError::new(ErrorKind::Config, format!("No catalogue at {from}."))
                    .with_detail("from", from),
            );
        };
        // A directory named on the command line is not something this can
        // update: it is a checkout, and pulling it is the command for that.
        // Reporting so beats an update that prints a path and fetched nothing.
        if matches!(options.action, PresetAction::Update) {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "{from} is a checkout, not a fetched catalogue — there is nothing\n  to \
                     update. Pull it with git, or drop --from to update the fetched one."
                ),
            )
            .with_detail("from", from));
        }
        return Ok(found);
    }

    let found = catalogue_dir(&CatalogueOptions {
        catalogue_dir: Some(catalogue_root.to_path_buf()),
        env: options.env.clone(),
        ..CatalogueOptions::default()
    });
    // `update` *is* a refresh — it is the whole command. Without this it
    // answered with the copy already here and reported success having fetched
    // nothing.
    let wants_fresh = options.catalogue.refresh || matches!(options.action, PresetAction::Update);
    if let Some(found) = found.clone()
        && !wants_fresh
    {
        return Ok(found);
    }

    if options.catalogue.offline {
        return found.ok_or_else(|| {
            GhostError::new(
                ErrorKind::Config,
                "No catalogue on this machine, and --offline forbids fetching one.\n  Drop \
                 --offline, or pass --from with a checkout of the presets repository.",
            )
        });
    }

    writeln!(
        out,
        "{}",
        options.t.tr(
            keys::preset::FETCHING,
            args!["package" => CATALOGUE_PACKAGE]
        )
    )
    .map_err(GhostError::from)?;
    fetch_catalogue(&FetchCatalogueOptions {
        catalogue_dir: catalogue_root,
        range: None,
        fetch: options.fetch,
    })
}

/// One line to the answer stream.
fn line(out: &mut dyn Write, text: &str) -> Result<()> {
    writeln!(out, "{text}").map_err(GhostError::from)
}

/// Runs one `ghostai preset` invocation and answers with its exit code.
pub fn run_preset(options: &mut PresetOptions<'_>, streams: &mut Streams) -> Result<u8> {
    match act(options, streams) {
        Ok(code) => Ok(code),
        Err(error) => {
            // A `GhostError` message is written to be read by the person who
            // caused it — it already names the file and what to do next.
            let _ = writeln!(streams.err, "{}", error.message);
            Ok(1)
        }
    }
}

fn act(options: &mut PresetOptions<'_>, streams: &mut Streams) -> Result<u8> {
    let loaded = load_config(LoadConfigOptions {
        paths: load_options(&options.globals, None, &options.env),
        file: None,
    })?;

    let dir = locate(options, &loaded.paths.catalogue_dir, &mut streams.out)?;
    if matches!(options.action, PresetAction::Update) {
        line(&mut streams.out, &format!("Catalogue at {}", dir.display()))?;
        return Ok(0);
    }
    // Refuses when the directory exists but holds no `agents/` — the older
    // layout, which would otherwise offer nothing and read as "no presets
    // exist".
    let agents_dir = assert_catalogue_layout(&dir)?;

    let paths = PresetPaths {
        policy_dir: loaded.paths.policy_dir.clone(),
        presets_dir: loaded.paths.presets_dir.clone(),
        db_file: loaded.paths.db_file.clone(),
        catalogue_agents_dir: Some(agents_dir.clone()),
        catalogue_skills_dir: catalogue_skills_dir(&dir),
        // Resolved here rather than inside the copy, so a `-W` naming no
        // workspace fails before anything is built or written.
        skills_dir: Some(skills_target_dir(
            &loaded.paths,
            options
                .workspace_id
                .as_deref()
                .unwrap_or(DEFAULT_WORKSPACE_ID),
        )?),
    };

    let mut warnings = Vec::new();
    let available = offers(&paths, &loaded.config, &mut warnings);
    for warning in &warnings {
        line(&mut streams.out, warning)?;
    }
    if available.is_empty() {
        line(
            &mut streams.out,
            &format!(
                "No presets under {} or {}.",
                agents_dir.display(),
                paths.presets_dir.display()
            ),
        )?;
        return Ok(0);
    }

    if matches!(options.action, PresetAction::List) {
        for offer in &available {
            let mark = if offer.installed {
                format!("  [{}]", options.t.t(keys::preset::INSTALLED))
            } else {
                String::new()
            };
            line(
                &mut streams.out,
                &format!("{}{mark}", label_of(offer, options.t)),
            )?;
        }
        return Ok(0);
    }

    let Some(chosen) = select(options, &available, streams)? else {
        writeln!(
            streams.err,
            "Which presets? Pass ids, or run this in a terminal to pick from a list —\n  see \
             `ghostai preset list`."
        )
        .map_err(GhostError::from)?;
        return Ok(2);
    };
    if chosen.is_empty() {
        line(&mut streams.out, &options.t.t(keys::preset::NOTHING_CHOSEN))?;
        return Ok(0);
    }

    install(options, &loaded, &paths, &dir, &chosen, streams)
}

/// Which presets this run installs.
///
/// `None` for "nobody said, and there is nobody to ask" — distinct from an
/// empty list, which is somebody declining. The first is exit 2 with a usage
/// message; the second is exit 0, because pressing enter on the picker is a
/// valid answer and not an error.
fn select(
    options: &mut PresetOptions<'_>,
    available: &[Offer],
    streams: &mut Streams,
) -> Result<Option<Vec<Offer>>> {
    let named = ids_of(&options.action);
    if !named.is_empty() {
        let missing: Vec<&String> = named
            .iter()
            .filter(|id| !available.iter().any(|offer| &offer.id == *id))
            .collect();
        if !missing.is_empty() {
            let quoted: Vec<String> = missing.iter().map(|id| format!("\"{id}\"")).collect();
            let offered: Vec<&str> = available.iter().map(|offer| offer.id.as_str()).collect();
            return Err(GhostError::new(
                ErrorKind::InvalidInput,
                format!(
                    "No preset is available under {}.\n  Available: {}.",
                    quoted.join(", "),
                    offered.join(", ")
                ),
            )
            .with_detail(
                "missing",
                missing.iter().map(|id| (*id).clone()).collect::<Vec<_>>(),
            ));
        }
        // Deduplicated and back into catalogue order, so `install b a` and
        // `install a b` do the same thing — which matters because the delegator
        // ordering below is computed from this list.
        return Ok(Some(
            available
                .iter()
                .filter(|offer| named.contains(&offer.id))
                .cloned()
                .collect(),
        ));
    }

    let labels: Vec<String> = available
        .iter()
        .map(|offer| label_of(offer, options.t))
        .collect();
    let marks: Vec<String> = available
        .iter()
        .map(|offer| {
            if offer.installed {
                format!("[{}]", options.t.t(keys::preset::INSTALLED))
            } else {
                String::new()
            }
        })
        .collect();
    let question = options.t.t(keys::preset::WHICH);

    let Some(ask) = options.ask.as_mut() else {
        return Ok(None);
    };
    let picked = ask.choose_many(&mut streams.out, &question, &labels, &marks)?;
    Ok(Some(
        picked
            .into_iter()
            .filter_map(|index| available.get(index).cloned())
            .collect(),
    ))
}

/// One preset that could not be written, and why.
struct Blocked {
    id: String,
    reason: String,
}

fn install(
    options: &mut PresetOptions<'_>,
    loaded: &LoadedConfig,
    paths: &PresetPaths,
    catalogue: &Path,
    chosen: &[Offer],
    streams: &mut Streams,
) -> Result<u8> {
    // A fresh install has no root directory yet, and both the policy store and
    // the settings write go into it. Made once, here, rather than discovered as
    // an unwritable directory by whichever of them ran first.
    ensure_dir(&loaded.paths.root)?;

    // What the *chosen* agents name, in first-mention order and each once. Two
    // agents naming one container is one build.
    let mut toolboxes: Vec<String> = Vec::new();
    let mut containers: Vec<String> = Vec::new();
    for offer in chosen {
        for (name, into) in [
            (&offer.preset.toolbox.name, &mut toolboxes),
            (&offer.preset.container.name, &mut containers),
        ] {
            if !name.is_empty() && !into.contains(name) {
                into.push(name.clone());
            }
        }
    }

    let missing: Vec<String> = toolboxes
        .iter()
        .filter(|name| !is_toolbox_installed(paths, name))
        .filter(|name| catalogue_toolbox(catalogue, name).is_none())
        .chain(
            containers
                .iter()
                .filter(|name| !is_container_installed(paths, name))
                .filter(|name| catalogue_container(catalogue, name).is_none()),
        )
        .cloned()
        .collect();
    if !missing.is_empty() {
        let quoted: Vec<String> = missing.iter().map(|name| format!("\"{name}\"")).collect();
        return Err(GhostError::new(
            ErrorKind::Config,
            format!(
                "This catalogue carries nothing named {}.\n  A preset naming a toolbox or \
                 container the catalogue does not carry cannot be\n  installed. Update the \
                 catalogue with `ghostai preset update`.",
                quoted.join(", ")
            ),
        )
        .with_detail("missing", missing));
    }

    // Already installed and usable? Then there is nothing to do: rebuilding
    // would change the image id and so the definition's digest, restarting
    // every warm instance of it for no reason.
    for name in &toolboxes {
        if !is_toolbox_installed(paths, name) {
            install_toolbox(name, catalogue, &paths.policy_dir)?;
        }
    }
    let to_build: Vec<&String> = containers
        .iter()
        .filter(|name| !is_container_installed(paths, name))
        .collect();
    build_containers(options, paths, catalogue, &to_build, streams)?;

    let mut config = loaded.config.clone();
    let mut installed: Vec<String> = Vec::new();
    let mut blocked: Vec<Blocked> = Vec::new();
    let mut stale: Vec<String> = Vec::new();

    // Delegators last. A preset's roster is snapshotted from the agents that
    // exist when it installs, so a lead installed before its specialists would
    // be handed an empty team — in one run, ordering is the whole fix.
    let mut ordered: Vec<&Offer> = chosen.iter().collect();
    ordered.sort_by_key(|offer| usize::from(!offer.preset.subagents.is_empty()));

    let force = force_of(&options.action);
    for offer in &ordered {
        match plan_install(&offer.preset, &config, paths, force)? {
            InstallPlan::Ready(ready) => {
                config = ready.config;
                installed.push(offer.id.clone());
            }
            InstallPlan::Blocked { id, reason } => {
                // Already installed, and its roster would now name more
                // specialists than it does — which happens whenever toolboxes
                // were approved between two runs. Not overwritten, because the
                // entry may carry edits; named instead, with the command that
                // refreshes it.
                if let Some(current) = config.agents.list.get(&id)
                    && roster_is_stale(&offer.preset, current, &config)
                {
                    stale.push(id.clone());
                }
                blocked.push(Blocked { id, reason });
            }
        }
    }

    if !installed.is_empty() {
        save_config(&loaded.file, &config)?;
    }

    line(
        &mut streams.out,
        &if installed.is_empty() {
            "No agents were installed.".to_owned()
        } else {
            format!(
                "Installed {} agents: {}",
                installed.len(),
                installed.join(", ")
            )
        },
    )?;

    // Only for the presets that actually installed. A blocked one was left
    // alone "in case you have edited them", and overwriting its sheets would be
    // the same edit by another route.
    let sheets = install_sheets(paths, &ordered, &installed, force);

    report(paths, &config, &blocked, &stale, &sheets, force, streams)?;
    Ok(0)
}

/// Builds and installs the definitions for the containers that need one.
///
/// The daemon is probed once, before the first build: five failed builds is a
/// worse way to learn it is down than one sentence.
fn build_containers(
    options: &PresetOptions<'_>,
    paths: &PresetPaths,
    catalogue: &Path,
    to_build: &[&String],
    streams: &mut Streams,
) -> Result<()> {
    if to_build.is_empty() {
        return Ok(());
    }
    match options.probe {
        Some(probe) => probe()?,
        None => docker_engine(DockerEngineOptions::default()).probe()?,
    }

    let build = options.build.unwrap_or(&container_build);
    for name in to_build {
        let Some(context) = catalogue_container(catalogue, name) else {
            continue;
        };
        line(&mut streams.out, &format!("==> building {name}"))?;
        install_container(name, &context, &paths.policy_dir, build)?;
        line(
            &mut streams.out,
            &format!(
                "    installed {}",
                paths
                    .policy_dir
                    .join("containers")
                    .join(format!("{name}.yaml"))
                    .display()
            ),
        )?;
    }
    line(&mut streams.out, "")
}

/// Every chosen preset's sheets, copied and folded into one result.
fn install_sheets(
    paths: &PresetPaths,
    ordered: &[&Offer],
    installed: &[String],
    force: bool,
) -> SkillInstallResult {
    let mut written: Vec<WrittenSheet> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for offer in ordered {
        if !installed.contains(&offer.id) {
            continue;
        }
        let result = install_skills(&SkillInstallRequest {
            preset_id: &offer.id,
            names: &offer.preset.skills,
            catalogue_skills_dir: paths.catalogue_skills_dir.as_deref(),
            target_dir: paths.skills_dir.as_deref(),
            force,
        });
        // Two presets can name one sheet. The second copy is a no-op the first
        // already did, so the report should not say it twice either.
        for sheet in result.written {
            if !written.iter().any(|seen| seen.name == sheet.name) {
                written.push(sheet);
            }
        }
        for name in result.kept {
            if !kept.contains(&name) {
                kept.push(name);
            }
        }
        for name in result.missing {
            if !missing.contains(&name) {
                missing.push(name);
            }
        }
        warnings.extend(result.warnings);
    }

    SkillInstallResult {
        // A sheet two presets both name is written by the first and then found
        // already there by the second. Reporting it as both written and kept
        // would be true of neither.
        kept: kept
            .into_iter()
            .filter(|name| !written.iter().any(|sheet| &sheet.name == name))
            .collect(),
        written,
        missing,
        warnings,
    }
}

/// What the sheet copy did, and what it left for the operator.
///
/// Split from the rest of the report because the two halves answer different
/// questions — one is about files in a workspace, the other about approvals and
/// agents — and the only thing they share is the stream they write to.
fn report_sheets(
    paths: &PresetPaths,
    sheets: &SkillInstallResult,
    out: &mut dyn Write,
) -> Result<()> {
    if !sheets.written.is_empty() {
        line(out, "")?;
        let target = paths
            .skills_dir
            .as_deref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        line(out, &format!("Skill sheets written to {target}:"))?;
        for sheet in &sheets.written {
            let plural = if sheet.files == 1 { "" } else { "s" };
            line(
                out,
                &format!("    {}  ({} file{plural})", sheet.name, sheet.files),
            )?;
        }
    }

    if !sheets.kept.is_empty() {
        line(out, "")?;
        line(
            out,
            "Already in the workspace, and left alone in case you have edited",
        )?;
        line(out, "them:")?;
        for name in &sheets.kept {
            line(out, &format!("    {name}"))?;
        }
        line(out, "")?;
        line(out, "Re-run with --force to overwrite them.")?;
    }

    if !sheets.missing.is_empty() {
        line(out, "")?;
        line(out, "Named by a preset but not in this catalogue:")?;
        for name in &sheets.missing {
            line(out, &format!("    {name}"))?;
        }
    }

    for warning in &sheets.warnings {
        line(out, "")?;
        line(out, warning)?;
    }
    Ok(())
}

/// Everything the operator still has to do, each with the command that does it.
fn report(
    paths: &PresetPaths,
    config: &Config,
    blocked: &[Blocked],
    stale: &[String],
    sheets: &SkillInstallResult,
    force: bool,
    streams: &mut Streams,
) -> Result<()> {
    let out = &mut streams.out;
    report_sheets(paths, sheets, out)?;

    let waiting: Vec<&Blocked> = blocked
        .iter()
        .filter(|entry| !config.agents.list.contains_key(&entry.id))
        .collect();
    if !waiting.is_empty() {
        line(out, "")?;
        for entry in &waiting {
            line(out, &format!("Could not install {}:", entry.id))?;
            for text in entry.reason.lines() {
                line(out, &format!("  {text}"))?;
            }
        }
    }

    let kept: Vec<&Blocked> = blocked
        .iter()
        .filter(|entry| config.agents.list.contains_key(&entry.id) && !stale.contains(&entry.id))
        .collect();
    if !kept.is_empty() && !force {
        line(out, "")?;
        line(
            out,
            "Already installed, and left alone in case you have edited them:",
        )?;
        for entry in &kept {
            line(out, &format!("    {}", entry.id))?;
        }
        line(out, "")?;
        line(
            out,
            "Re-run with --force to overwrite them with the preset.",
        )?;
    }

    if !stale.is_empty() {
        line(out, "")?;
        line(
            out,
            "These agents delegate, and can now reach specialists they were not",
        )?;
        line(
            out,
            "given when they were installed. Refresh each roster when you want",
        )?;
        line(
            out,
            "it — this never overwrites an agent you may have edited:",
        )?;
        for id in stale {
            line(out, &format!("    ghostai agent install {id} --force"))?;
        }
    }
    Ok(())
}

/// Whether a preset's delegation roster would now name specialists the
/// installed entry does not.
///
/// Only ever *grows*: an entry naming someone the preset does not is an
/// operator's own edit, and reporting that as stale would be telling them their
/// customisation is a mistake.
fn roster_is_stale(preset: &AgentPreset, entry: &AgentEntry, config: &Config) -> bool {
    preset.subagents.iter().any(|reference| {
        !entry.subagents.iter().any(|held| held.id == reference.id)
            && config
                .agents
                .list
                .get(&reference.id)
                .is_some_and(|other| other.enabled)
    })
}

/// The definitions over this run's paths.
fn open_store(paths: &PresetPaths) -> PolicyStore {
    PolicyStore::new(paths.policy_dir.clone())
}

/// Whether this toolbox is installed and usable as it stands.
fn is_toolbox_installed(paths: &PresetPaths, name: &str) -> bool {
    open_store(paths).require_toolbox(name).is_ok()
}

/// Whether this container is installed and usable as it stands.
fn is_container_installed(paths: &PresetPaths, name: &str) -> bool {
    open_store(paths).require_container(name).is_ok()
}

/// Runs one `ghostai preset` invocation, with the real world wired in.
///
/// Nothing here awaits today — every step is a file read, a process, or a
/// question at a terminal. It is `async` because the dispatcher awaits every
/// subcommand alike, and a sibling that is sometimes a future and sometimes not
/// would put the shape of one command into the code that chooses between them.
#[expect(
    clippy::unused_async,
    reason = "the dispatcher awaits every subcommand alike; see above"
)]
pub async fn run(
    globals: &Globals,
    catalogue: &CatalogueArgs,
    action: PresetAction,
    env: &Env,
    streams: &mut Streams,
) -> Result<u8> {
    let t = Translations::for_env(env, None);
    let workspace_id = match &action {
        PresetAction::Install { workspace_id, .. } => workspace_id.clone(),
        PresetAction::List | PresetAction::Update => None,
    };

    // Only when there is somebody to answer *and* nothing was named on the
    // command line. A pipe gets the safe default rather than a question it
    // would answer with end-of-input.
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin())
        && ids_of(&action).is_empty()
        && matches!(action, PresetAction::Install { .. });

    let mut reader = crate::ask::StdinReader::new();
    let mut ask = Ask::new(&mut reader, globals.color, &t);

    let mut options = PresetOptions {
        action,
        catalogue: catalogue.clone(),
        workspace_id,
        globals: globals.clone(),
        env: env.clone(),
        t: &t,
        ask: if interactive { Some(&mut ask) } else { None },
        build: None,
        probe: None,
        fetch: None,
    };
    run_preset(&mut options, streams)
}
