//! Reading and writing `config.yaml`.
//!
//! The mirror in `ghostai-protocol` deliberately does no normalisation on
//! parse, so that every field stays representable as JSON Schema for the
//! OpenAPI document and the parsed shape is identical to the serialised one.
//! That leaves one job for load time, and this is it: find the file, turn its
//! absence into defaults rather than an error, and resolve the paths the config
//! names.
//!
//! Three decisions worth stating:
//!
//!  - **A missing config file is the normal first run**, not a failure. Every
//!    field has a default and an empty object parses to a complete tree, so
//!    `ghostai chat --provider ollama --model qwen3` has to work on a machine
//!    that has never written a config. [`LoadedConfig::from_file`] reports which
//!    happened, for the one caller that wants to say "no config found" in a
//!    diagnostic.
//!
//!  - **A malformed config file is a hard failure, and it names the keys.** The
//!    alternative, falling back to defaults on a parse error, silently ignores
//!    everything the operator wrote, and the first sign of it is an agent
//!    talking to the wrong provider. Issues are reported as dotted paths because
//!    `agents.list.default.temperature` is something you can search a file for,
//!    and a list of segments is not.
//!
//!  - **Paths are resolved twice, on purpose.** The config file lives under the
//!    root, so the root has to be resolved before the file can be read, but the
//!    file is what names the workspace. The second pass folds the config's
//!    `workspace` in, with an explicit caller-supplied workspace still winning
//!    over both.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use garde::Validate as _;
use ghostai_protocol::Config;
use serde_json::Value;

use crate::errors::{ErrorKind, GhostError, Result};
use crate::paths::{GhostPaths, ResolveGhostPaths, ensure_dir};

/// Inputs to [`load_config`].
#[derive(Debug, Clone, Default)]
pub struct LoadConfigOptions {
    /// Where the root and, absent a config, the workspace come from.
    pub paths: ResolveGhostPaths,
    /// Overrides `<root>/config.yaml`.
    pub file: Option<PathBuf>,
}

/// What [`load_config`] found.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    /// The settings tree.
    pub config: Config,
    /// With `workspace` folded in from the config, unless the caller named one.
    pub paths: GhostPaths,
    /// The file that was read, or would have been.
    pub file: PathBuf,
    /// `false` when no file existed and the defaults were used.
    pub from_file: bool,
}

/// Parses and validates config text.
///
/// Separate from the file read so that a config arriving over the wire (the
/// settings panel's preview) is validated by exactly the same code that
/// validates the file, rather than by a second implementation that drifts.
pub fn parse_config(text: &str, file: &Path) -> Result<Config> {
    let raw: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).map_err(|error| {
        GhostError::new(
            ErrorKind::Config,
            format!("{} is not valid YAML: {error}", file.display()),
        )
        .with_detail("file", file.to_string_lossy())
        .with_source(error)
    })?;

    // A struct also deserialises from a sequence, positionally, and every
    // field here has a default, so `[]` would read as a complete config. The
    // file is an object or it is malformed.
    if !matches!(raw, serde_yaml_ng::Value::Mapping(_)) {
        let issue = "(root): expected an object".to_owned();
        return Err(invalid_settings(file, vec![issue]));
    }

    let config: Config = match serde_path_to_error::deserialize(raw) {
        Ok(config) => config,
        Err(error) => {
            let mut path = error.path().to_string();
            if path == "." {
                path.clear();
            }
            let message = error.inner().to_string();
            if let Some(field) = message
                .strip_prefix("missing field `")
                .and_then(|rest| rest.strip_suffix('`'))
            {
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(field);
            }
            let label = if path.is_empty() { "(root)" } else { &path };
            let issues = vec![format!("{label}: {message}")];
            return Err(invalid_settings(file, issues).with_source(error));
        }
    };

    let issues = validation_issues(&config);
    if issues.is_empty() {
        Ok(config)
    } else {
        Err(invalid_settings(file, issues))
    }
}

/// One line per validation failure, as `path: message`, or nothing when the
/// tree is acceptable.
///
/// Public because [`save_config`] refuses on the same list and the settings
/// route wants to show it before writing anything.
pub fn validation_issues(config: &Config) -> Vec<String> {
    let mut issues = Vec::new();
    if let Err(report) = config.validate() {
        issues.extend(
            report
                .iter()
                .map(|(path, error)| (path.to_string(), error))
                // The keyed maps are reported per entry below, with the key.
                .filter(|(path, _)| !KEYED_MAPS.contains(&path.as_str()))
                .map(|(path, error)| issue_line(&path, error)),
        );
    }
    keyed_issues("agents.list", &config.agents.list, &mut issues);
    keyed_issues("providers", &config.providers, &mut issues);
    keyed_issues("tools.mcpServers", &config.tools.mcp_servers, &mut issues);
    issues
}

/// The maps whose entries the protocol validates as one opaque field. Their
/// entries are re-validated here so the issue names the key: an operator
/// searches for `agents.list.reviewer.temperature`, not for `agents.list`.
const KEYED_MAPS: [&str; 3] = ["agents.list", "providers", "tools.mcpServers"];

fn keyed_issues<V: garde::Validate<Context = ()>>(
    prefix: &str,
    map: &indexmap::IndexMap<String, V>,
    issues: &mut Vec<String>,
) {
    for (key, value) in map {
        if let Err(report) = value.validate() {
            issues.extend(report.iter().map(|(path, error)| {
                // `settings` is an agent entry's flattened block: a field in
                // the Rust type, not a level in the file.
                let inner = path.to_string();
                let inner = inner.strip_prefix("settings.").unwrap_or(&inner);
                issue_line(&format!("{prefix}.{key}.{inner}"), error)
            }));
        }
    }
}

fn issue_line(path: &str, error: &garde::Error) -> String {
    let label = if path.is_empty() { "(root)" } else { path };
    format!("{label}: {error}")
}

fn invalid_settings(file: &Path, issues: Vec<String>) -> GhostError {
    let listed: Vec<String> = issues.iter().map(|issue| format!("  {issue}")).collect();
    GhostError::new(
        ErrorKind::Config,
        format!(
            "{} has invalid settings:\n{}",
            file.display(),
            listed.join("\n")
        ),
    )
    .with_detail("file", file.to_string_lossy())
    .with_detail("issues", Value::from(issues))
}

/// The exact bytes [`save_config`] writes: YAML with a trailing newline.
pub fn render_config(config: &Config) -> Result<String> {
    serde_yaml_ng::to_string(config).map_err(|error| {
        GhostError::new(ErrorKind::Internal, "config is not serialisable").with_source(error)
    })
}

/// Writes the settings tree back.
///
/// The other half of [`load_config`], and it lives here for the same reason: a
/// settings save from the UI and a config file written by hand have to be the
/// same file in the same shape, and two implementations of "what a config file
/// looks like" is one more than the format can survive.
///
/// Three properties it holds:
///
///  - **It validates before it writes.** A patch that merged into something the
///    schema rejects is a config the next boot refuses to load, and discovering
///    that at the next restart is discovering it at the worst moment.
///  - **The replacement is atomic.** A crash mid-write leaves the previous file
///    intact rather than a truncated one; a half-written `config.yaml` is an
///    install that will not start.
///  - **Two spaces and a trailing newline**, because this file is edited by
///    hand at least as often as it is written by a program, and a save from the
///    UI should not reformat what an operator wrote.
pub fn save_config(file: &Path, config: &Config) -> Result<()> {
    let issues = validation_issues(config);
    if !issues.is_empty() {
        return Err(GhostError::new(
            ErrorKind::Config,
            format!("Refusing to write invalid settings to {}", file.display()),
        )
        .with_detail("file", file.to_string_lossy())
        .with_detail("issues", Value::from(issues)));
    }

    if let Some(parent) = file.parent() {
        ensure_dir(parent)?;
    }
    let text = render_config(config)?;

    // Same directory, so the rename is a rename and not a cross-device copy,
    // which is not atomic and is exactly what this is avoiding. `0o600`
    // because the file holds provider keys and channel tokens.
    let temporary = temporary_path(file);
    write_private(&temporary, &text)
        .and_then(|()| fs::rename(&temporary, file))
        .map_err(|error| {
            GhostError::new(
                ErrorKind::Config,
                format!("{} could not be written", file.display()),
            )
            .with_detail("file", file.to_string_lossy())
            .with_source(error)
        })
}

fn temporary_path(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)?.write_all(text.as_bytes())
}

/// The settings tree and the paths derived from it.
///
/// Precedence for the workspace is the explicit option, then the config file,
/// then `<root>/workspace`. An explicit workspace is an instruction for one run
/// and must not be overridden by whatever the config happens to say; an empty
/// `workspace` means "unset", so a root moved with `GHOSTAI_HOME` takes its
/// workspace with it.
pub fn load_config(options: LoadConfigOptions) -> Result<LoadedConfig> {
    let base = GhostPaths::resolve(options.paths.clone())?;
    let file = options.file.unwrap_or_else(|| base.config_file.clone());

    let text = match fs::read_to_string(&file) {
        Ok(text) => Some(text),
        // Only these two mean "there is no config file here".
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            None
        }
        Err(error) => {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!("{} could not be read", file.display()),
            )
            .with_detail("file", file.to_string_lossy())
            .with_source(error));
        }
    };

    let config = match &text {
        Some(text) => parse_config(text, &file)?,
        None => Config::default(),
    };

    let mut resolve = options.paths;
    if resolve.workspace.is_none() && !config.workspace.is_empty() {
        resolve.workspace = Some(config.workspace.clone());
    }

    Ok(LoadedConfig {
        paths: GhostPaths::resolve(resolve)?,
        config,
        file,
        from_file: text.is_some(),
    })
}
