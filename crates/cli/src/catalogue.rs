//! Finding the catalogue, and fetching it when it is not here yet.
//!
//! The catalogue is `@ghostwire/presets` — the agent presets and the toolbox
//! definitions some of them run in — and it is published from a repository of
//! its own rather than living in this one. That is the whole reason this module
//! exists: presets shipped inside the binary would make finding them a lookup
//! and nothing else, but they are data arriving separately and on its own
//! cadence, which makes "where is it" and "get it" two real questions.
//!
//! **Four places, nearest first, and the first one wins:**
//!
//!  1. An explicit directory — `--from`, or `GHOSTAI_CATALOGUE`. A checkout of
//!     the presets repository, which is what somebody writing a preset has.
//!  2. `GHOSTAI_PRESETS_DIR` — the same thing for an install that keeps its
//!     catalogue somewhere fixed: a read-only image layer, a shared mount, a
//!     package the distribution laid down. Separate from `GHOSTAI_CATALOGUE`
//!     because the two are set by different people for different reasons, and
//!     collapsing them would make `--from` on a machine that has the fixed one
//!     exported behave differently from `--from` on a machine that does not.
//!  3. `<root>/catalogue/node_modules/@ghostwire/presets` — what [`fetch_catalogue`]
//!     put there.
//!  4. A sibling checkout: a `GhostAI-presets` or `ghostai-presets` directory
//!     beside this one. This is what a contributor with both repositories
//!     checked out has, and finding it is what keeps `ghostai preset list` from
//!     reaching a registry during development.
//!
//! Every one of them answers `None` rather than failing when it is not there. A
//! missing catalogue is an ordinary state — a fresh install has none — and the
//! command above turns it into a sentence with a fix in it.
//!
//! The sibling lookup is the one that *guesses*, so it is the one held to a
//! stricter test: it must hold an `agents/` directory to count. A named
//! directory that turns out to have the wrong layout is worth the sentence
//! [`assert_catalogue_layout`] writes; a *guessed* one that matched something
//! unrelated would produce that sentence about a directory the operator never
//! mentioned.
//!
//! **Fetching is npm's job, not ours.** `npm install --prefix` into
//! `<root>/catalogue` is why the package lands a level down under
//! `node_modules/` instead of being unpacked here directly, and the nesting is
//! worth it: npm already does integrity checking, version resolution and the
//! update case, and the alternative is a registry client and a tarball reader
//! this repository would own and have to keep correct.

use std::path::{Path, PathBuf};
use std::process::Command;

use ghostai_core::{ErrorKind, GhostError, Result};

use crate::i18n::Env;

/// The package the catalogue is published as.
pub const CATALOGUE_PACKAGE: &str = "@ghostwire/presets";

/// The range [`fetch_catalogue`] asks for.
///
/// A range rather than `latest`, so a future breaking change to the layout has
/// to be adopted by editing this line rather than arriving on its own the next
/// time somebody runs `ghostai preset update`.
///
/// This package was briefly published as `@ghostwire/catalogue`, whose 1.x kept
/// its presets under `presets/` rather than `agents/`. The rename is what lets
/// this start at 1.0.0 rather than carrying a major bump to step over that
/// layout: under a new name there is no old layout to skip.
pub const CATALOGUE_RANGE: &str = "^1.0.0";

/// Points [`catalogue_dir`] at a checkout, for somebody writing a preset.
pub const CATALOGUE_ENV_VAR: &str = "GHOSTAI_CATALOGUE";

/// Points [`catalogue_dir`] at a catalogue an install keeps somewhere fixed.
pub const PRESETS_DIR_ENV_VAR: &str = "GHOSTAI_PRESETS_DIR";

/// The directory names a sibling checkout is looked for under.
///
/// Both spellings, because the repository is `GhostAI` and half of everyone
/// clones it lower-cased.
pub const SIBLING_CHECKOUT_NAMES: [&str; 2] = ["GhostAI-presets", "ghostai-presets"];

/// How far above the running binary a sibling checkout is looked for.
///
/// Bounded rather than walked to the filesystem root: a development build sits
/// at `<repo>/target/debug/ghostai`, so the directory holding both checkouts is
/// four levels up, and a search that kept climbing would start matching
/// directories in somebody's home or on the root of the disk.
const SIBLING_SEARCH_DEPTH: usize = 6;

/// Where a catalogue is looked for.
#[derive(Debug, Clone, Default)]
pub struct CatalogueOptions {
    /// `--from`, and the only path that needs no network at all.
    pub from: Option<String>,
    /// `<root>/catalogue` — the npm prefix [`fetch_catalogue`] installs into.
    pub catalogue_dir: Option<PathBuf>,
    /// The environment the two overrides are read from.
    pub env: Env,
    /// Directories to look for a sibling checkout beside.
    ///
    /// Defaults to [`sibling_search_roots`] when empty, which derives them from
    /// the running binary. Named explicitly so a test can point the search at a
    /// temporary tree instead of wherever the test binary happens to live.
    pub near: Vec<PathBuf>,
}

/// Where [`fetch_catalogue`] puts the package inside the prefix it is given.
pub fn fetched_catalogue_dir(catalogue_dir: &Path) -> PathBuf {
    let mut dir = catalogue_dir.join("node_modules");
    for segment in CATALOGUE_PACKAGE.split('/') {
        dir.push(segment);
    }
    dir
}

/// The directories a sibling checkout is looked for beside.
///
/// Every ancestor of the running binary, nearest first, bounded by
/// [`SIBLING_SEARCH_DEPTH`]. An unreadable executable path is not a failure:
/// the lookup simply has nowhere to look, which is the same answer as not
/// finding anything.
pub fn sibling_search_roots() -> Vec<PathBuf> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    exe.ancestors()
        .skip(1)
        .take(SIBLING_SEARCH_DEPTH)
        .map(Path::to_path_buf)
        .collect()
}

/// The catalogue's root directory, or `None` when there is none.
///
/// Resolved on call rather than once, because `--from` differs per invocation
/// and the fetched copy appears while a single `ghostai preset install` is
/// running.
pub fn catalogue_dir(options: &CatalogueOptions) -> Option<PathBuf> {
    let explicit = options
        .from
        .as_deref()
        .filter(|value| !value.is_empty())
        .or_else(|| options.env.non_empty(CATALOGUE_ENV_VAR))
        .or_else(|| options.env.non_empty(PRESETS_DIR_ENV_VAR));

    // An explicit directory is not searched past: pointing `--from` at a typo
    // and silently getting the fetched copy is how somebody ships a preset they
    // never actually tested.
    if let Some(explicit) = explicit {
        let path = PathBuf::from(explicit);
        return path.exists().then_some(path);
    }

    if let Some(prefix) = options.catalogue_dir.as_deref() {
        let fetched = fetched_catalogue_dir(prefix);
        if fetched.exists() {
            return Some(fetched);
        }
    }

    sibling_checkout(options)
}

/// A `GhostAI-presets` checkout beside one of the search roots.
///
/// Held to `agents/` for the reason the module documents: this is the one
/// lookup nobody named, so it has to recognise a catalogue rather than a
/// directory.
fn sibling_checkout(options: &CatalogueOptions) -> Option<PathBuf> {
    let owned;
    let roots = if options.near.is_empty() {
        owned = sibling_search_roots();
        owned.as_slice()
    } else {
        options.near.as_slice()
    };

    roots.iter().find_map(|root| {
        SIBLING_CHECKOUT_NAMES.iter().find_map(|name| {
            let candidate = root.join(name);
            catalogue_agents_dir(&candidate).map(|_| candidate)
        })
    })
}

/// The agent presets, one `<id>.yaml` each, or `None`.
pub fn catalogue_agents_dir(dir: &Path) -> Option<PathBuf> {
    subdir(dir, "agents")
}

/// One `<name>.yaml` per toolbox, naming the operations it grants.
pub fn catalogue_toolboxes_dir(dir: &Path) -> Option<PathBuf> {
    subdir(dir, "toolboxes")
}

/// One directory per container, each with a `Dockerfile` and a definition.
pub fn catalogue_containers_dir(dir: &Path) -> Option<PathBuf> {
    subdir(dir, "containers")
}

/// One `<name>.yaml` per reusable operation definition.
pub fn catalogue_definitions_dir(dir: &Path) -> Option<PathBuf> {
    subdir(dir, "tool-definitions")
}

/// One directory per skill sheet, each with a `SKILL.md`.
///
/// Optional, like `toolboxes/` and unlike `agents/`: a catalogue that ships
/// only agent presets is an ordinary catalogue, so this answers `None` rather
/// than going through [`assert_catalogue_layout`].
pub fn catalogue_skills_dir(dir: &Path) -> Option<PathBuf> {
    subdir(dir, "skills")
}

fn subdir(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(name);
    path.exists().then_some(path)
}

/// One toolbox manifest, or `None` when the catalogue does not carry it.
///
/// A preset can name a toolbox this catalogue has never heard of — an
/// operator's own preset, or one written against a newer catalogue — and that
/// is a sentence to print, not a crash.
pub fn catalogue_toolbox(dir: &Path, name: &str) -> Option<PathBuf> {
    let file = catalogue_toolboxes_dir(dir)?.join(format!("{name}.yaml"));
    file.exists().then_some(file)
}

/// One operation definition, or `None` when the catalogue does not carry it.
pub fn catalogue_definition(dir: &Path, name: &str) -> Option<PathBuf> {
    let file = catalogue_definitions_dir(dir)?.join(format!("{name}.yaml"));
    file.exists().then_some(file)
}

/// The build context for one container, or `None` when the catalogue does not
/// carry it.
///
/// The same shape and the same argument as [`catalogue_toolbox`]. The
/// `container.yaml` has to be there as well as the directory: a name with no
/// definition is a half-checkout, and an image build would be the wrong error
/// to report it with.
pub fn catalogue_container(dir: &Path, name: &str) -> Option<PathBuf> {
    let context = catalogue_containers_dir(dir)?.join(name);
    context.join("container.yaml").exists().then_some(context)
}

/// One skill sheet's directory, or `None` when the catalogue lacks it.
///
/// The same shape and the same argument as [`catalogue_toolbox`]: a preset can
/// name a sheet this catalogue has never heard of, and the `SKILL.md` has to be
/// there as well as the directory. A directory without one would be copied,
/// reported as installed, and then silently skipped when the sheets are read —
/// the worst of the three outcomes, because nothing anywhere would say why.
pub fn catalogue_skill(dir: &Path, name: &str) -> Option<PathBuf> {
    let sheet = catalogue_skills_dir(dir)?.join(name);
    sheet.join("SKILL.md").exists().then_some(sheet)
}

/// The refusal for a catalogue that resolved but holds no `agents/`.
///
/// Its own sentence rather than an empty list, because the case that produces
/// it is specific and the fix is not guessable: a checkout from before the
/// layout settled keeps its presets under `presets/`, so `--from` at one gives
/// a directory that exists, reads, and offers nothing. "No presets available"
/// would send somebody looking for a preset to write.
pub fn assert_catalogue_layout(dir: &Path) -> Result<PathBuf> {
    catalogue_agents_dir(dir).ok_or_else(|| {
        GhostError::new(
            ErrorKind::Config,
            format!(
                "{} holds no agents/ directory.\n  {CATALOGUE_PACKAGE} {CATALOGUE_RANGE} is \
                 expected, and keeps its presets\n  in agents/. Run `ghostai preset update` to \
                 fetch a current one, or pass\n  --from with a checkout of the presets repository.",
                dir.display()
            ),
        )
        .with_detail("dir", dir.to_string_lossy())
        .with_detail("range", CATALOGUE_RANGE)
    })
}

/// Runs the fetch. Injected, so no test reaches a registry.
///
/// Answers with the exit status rather than failing, because the caller has a
/// better message for a failure than the spawn does — it knows about `--from`.
pub type Fetcher<'a> = &'a (dyn Fn(&[String]) -> Result<i32> + 'a);

/// Runs `npm` with an argv, never a shell line.
///
/// The child inherits this process's streams and gets no deadline, for the same
/// reason a container build does: a first fetch over a slow link with no output
/// looks hung, and a timeout on somebody else's network is a refusal they
/// cannot act on.
fn npm_fetch(args: &[String]) -> Result<i32> {
    let status = Command::new("npm").args(args).status().map_err(|error| {
        GhostError::new(
            ErrorKind::Tool,
            format!(
                "Could not run npm: {error}\n  Install npm, or pass --from with a checkout of the \
                 presets repository."
            ),
        )
        .with_source(error)
    })?;
    // A child killed by a signal reports no code. It did not succeed, and `1`
    // is what the caller's refusal is written against.
    Ok(status.code().unwrap_or(1))
}

/// Where and what to fetch.
pub struct FetchCatalogueOptions<'a> {
    /// `<root>/catalogue`, used as an npm prefix.
    pub catalogue_dir: &'a Path,
    /// Overrides [`CATALOGUE_RANGE`].
    pub range: Option<&'a str>,
    /// Overrides the real fetch, so no test reaches a registry.
    pub fetch: Option<Fetcher<'a>>,
}

impl std::fmt::Debug for FetchCatalogueOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchCatalogueOptions")
            .field("catalogue_dir", &self.catalogue_dir)
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

/// Installs or updates the catalogue, and answers with where it landed.
///
/// `--no-save` and `--no-package-lock` because the prefix is a place to put one
/// package, not a project: npm writes neither a manifest nor a lockfile there,
/// and re-running is how an update happens.
pub fn fetch_catalogue(options: &FetchCatalogueOptions<'_>) -> Result<PathBuf> {
    let range = options.range.unwrap_or(CATALOGUE_RANGE);
    let argv: Vec<String> = vec![
        "install".to_owned(),
        "--prefix".to_owned(),
        options.catalogue_dir.to_string_lossy().into_owned(),
        format!("{CATALOGUE_PACKAGE}@{range}"),
        "--no-save".to_owned(),
        "--no-package-lock".to_owned(),
        "--no-audit".to_owned(),
        "--no-fund".to_owned(),
    ];

    let status = match options.fetch {
        Some(fetch) => fetch(&argv)?,
        None => npm_fetch(&argv)?,
    };
    if status != 0 {
        return Err(GhostError::new(
            ErrorKind::Tool,
            format!(
                "Could not fetch {CATALOGUE_PACKAGE}@{range}.\n  Check the network, or pass --from \
                 with a checkout of the presets\n  repository to install without one."
            ),
        )
        .with_detail("range", range)
        .with_detail("status", i64::from(status)));
    }

    let dir = fetched_catalogue_dir(options.catalogue_dir);
    if !dir.exists() {
        return Err(GhostError::new(
            ErrorKind::Tool,
            format!("npm reported success but {} does not exist.", dir.display()),
        )
        .with_detail("dir", dir.to_string_lossy()));
    }
    Ok(dir)
}
