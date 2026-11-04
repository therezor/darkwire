//! The exec guard: an argv vector in, a validated spawn plan out.
//!
//! What is deliberately absent is a deny-list of shell metacharacters. Patterns
//! for `$(...)`, backticks and `| sh` are the standard shape of this code and
//! they are theatre when the executor is `Command::new(argv[0])` with no shell:
//! there is no string for a metacharacter to be interpreted *in*, so the
//! patterns can only ever reject legitimate arguments, such as a commit message
//! containing `$HOME` or a grep for a pipe character.
//!
//! What the guard does instead:
//!
//!  - A shell is refused when the agent's `shell` permission is `deny`. A shell
//!    behind a launcher such as `env` or `timeout` counts. Otherwise the
//!    command rules decide, and no wildcard rule lifts a shell call above
//!    `shell`, whose default is `ask`.
//!  - An environment allow-list, so the child inherits `PATH` and nothing that
//!    happens to hold a token. The map is supplied by the caller rather than
//!    read from the process, so the guard can be run over an environment it did
//!    not inherit.
//!  - An output budget, enforced while the child writes rather than after it
//!    exits.
//!
//! Paths are not checked. A child on the host runs as the operator's user and
//! reaches whatever that user can, through a script or a program's own
//! defaults as easily as through an argument, so refusing `/etc/passwd` only
//! refused the honest spelling. The command rules and approvals are the gate.
//! The working directory is the workspace root, so relative arguments resolve
//! there, and an argument is never rewritten.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{ExecToolConfig, ToolPermission};
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::jail::WorkspaceJail;

/// Binaries whose whole purpose is to interpret a string as a program.
///
/// A call to one of these is a shell call: the agent's `shell` permission caps
/// it, and no wildcard command rule can approve it.
pub const SHELL_BINARIES: &[&str] = &[
    "ash",
    "bash",
    "busybox",
    "cmd",
    "csh",
    "dash",
    "fish",
    "ksh",
    "powershell",
    "pwsh",
    "sh",
    "tcsh",
    "zsh",
];

/// Programs that run the program named in their arguments.
///
/// A shell behind one of these is still a shell: `env sh -c "…"` is `sh -c`.
/// Their own options are not parsed. Any argument naming a shell makes the
/// call a shell call, which can only over-refuse.
const LAUNCHERS: &[&str] = &[
    "caffeinate",
    "chrt",
    "doas",
    "env",
    "flock",
    "gtimeout",
    "ionice",
    "nice",
    "nohup",
    "setsid",
    "stdbuf",
    "sudo",
    "taskset",
    "time",
    "timeout",
    "xargs",
];

/// Whether a binary name is a shell, whatever its case.
///
/// Case is folded because macOS and Windows find `Bash` and `bash` as the same
/// file.
pub fn is_shell_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    SHELL_BINARIES.contains(&lower.as_str())
}

/// `env -S` and `--split-string`, the one launcher option that takes a
/// program string itself.
fn is_split_string(argument: &str) -> bool {
    argument.starts_with("--split-string")
        || argument
            .strip_prefix('-')
            .is_some_and(|flags| !flags.starts_with('-') && flags.contains('S'))
}

/// `env` options that take the next argument as their value.
const ENV_VALUE_OPTIONS: &[&str] = &["-u", "-C", "--unset", "--chdir"];

/// The name of the shell `argv` runs, directly or behind a launcher, if any.
///
/// `env` stands for the shell when `env -S` parses a string into a command.
pub fn shell_call(argv: &[String]) -> Option<String> {
    let program = binary_name(argv.first()?);
    if is_shell_name(&program) {
        return Some(program);
    }
    if !LAUNCHERS.contains(&program.to_lowercase().as_str()) {
        return None;
    }
    // Only `env`'s own options can be `-S`: past them, `-S` belongs to
    // whatever `env` runs.
    let mut env_options = program.eq_ignore_ascii_case("env");
    let mut value_next = false;
    for argument in argv.iter().skip(1) {
        if env_options {
            if value_next {
                value_next = false;
                continue;
            }
            if is_split_string(argument) {
                return Some("env".to_owned());
            }
            if ENV_VALUE_OPTIONS.contains(&argument.as_str()) {
                value_next = true;
                continue;
            }
            if argument.starts_with('-') || argument.contains('=') {
                continue;
            }
            env_options = false;
        } else if argument.contains('=') {
            // `SHELL=/bin/bash` names a shell and runs nothing.
            continue;
        }
        let name = binary_name(argument);
        if is_shell_name(&name) {
            return Some(name);
        }
        if name.eq_ignore_ascii_case("env") {
            env_options = true;
        }
    }
    None
}

/// Stripped before allow/deny matching so a verdict is the same on every platform.
const EXECUTABLE_EXTENSIONS: &[&str] = &[".exe", ".com", ".bat", ".cmd", ".ps1"];

/// What the guard needs beside the argv.
#[derive(Debug, Clone)]
pub struct ExecGuardOptions<'a> {
    /// The workspace the command runs in.
    pub jail: &'a WorkspaceJail,
    /// Defaults to the config's own defaults.
    pub config: Option<&'a ExecToolConfig>,
    /// The environment the allow-list is applied to.
    pub env: &'a HashMap<String, String>,
    /// Whether the command will run in a container that mounts the workspace.
    /// A relative program path is then the container's to resolve.
    pub sandboxed: bool,
}

impl<'a> ExecGuardOptions<'a> {
    /// Host execution with the default config.
    pub fn new(jail: &'a WorkspaceJail, env: &'a HashMap<String, String>) -> ExecGuardOptions<'a> {
        ExecGuardOptions {
            jail,
            config: None,
            env,
            sandboxed: false,
        }
    }
}

/// A validated command, ready to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecPlan {
    /// The program. Never through a shell.
    pub file: String,
    /// The arguments, exactly as written.
    pub args: Vec<String>,
    /// Always the workspace root, which is what makes relative arguments resolve.
    pub cwd: PathBuf,
    /// The allow-listed environment, in allow-list order.
    pub env: IndexMap<String, String>,
    /// `0` means unlimited, matching the config convention.
    pub timeout_ms: u64,
    /// The output budget in bytes.
    pub max_output_bytes: u64,
}

fn denied(message: impl Into<String>, details: Map<String, Value>) -> WireError {
    WireError::new(ErrorKind::PermissionDenied, message).with_details(details)
}

fn detail(key: &str, value: impl Into<Value>) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(key.to_owned(), value.into());
    map
}

/// The name a command rule's first token is compared against.
pub fn binary_name(argv0: &str) -> String {
    let unified = argv0.replace('\\', "/");
    let name = unified.rsplit('/').next().unwrap_or("");
    let lower = name.to_lowercase();
    for extension in EXECUTABLE_EXTENSIONS {
        if lower.ends_with(extension) && lower.len() > extension.len() {
            return name[..name.len() - extension.len()].to_owned();
        }
    }
    name.to_owned()
}

fn build_env(
    config: &ExecToolConfig,
    source: &HashMap<String, String>,
) -> IndexMap<String, String> {
    let mut env = IndexMap::new();
    for name in &config.env_allowlist {
        if let Some(value) = source.get(name) {
            env.insert(name.clone(), value.clone());
        }
    }
    if !config.path_append.is_empty() {
        let path = match env.get("PATH") {
            Some(existing) => format!("{existing}:{}", config.path_append),
            None => config.path_append.clone(),
        };
        env.insert("PATH".to_owned(), path);
    }
    env
}

fn check_shell(argv: &[String], config: &ExecToolConfig) -> Result<()> {
    match shell_call(argv) {
        Some(name) if config.shell == ToolPermission::Deny => Err(denied(
            format!(
                "{name} is a shell, and shells are switched off for this agent. Pass the program and its arguments as argv instead."
            ),
            detail("binary", name),
        )),
        _ => Ok(()),
    }
}

/// The program to spawn. On the host a relative path with a separator is
/// anchored at the workspace root, since how a relative program meets a
/// changed working directory differs by platform.
fn program(argv0: &str, jail: &WorkspaceJail, sandboxed: bool) -> String {
    let relative = Path::new(argv0).is_relative() && argv0.contains('/');
    if sandboxed || !relative {
        return argv0.to_owned();
    }
    jail.root().join(argv0).to_string_lossy().into_owned()
}

/// Validates an argv vector and returns the plan to spawn it.
///
/// Errors rather than returning a verdict: there is no partially-acceptable
/// command, and a caller that forgot to check a boolean would spawn it anyway.
pub fn guard_exec(argv: &[String], options: &ExecGuardOptions<'_>) -> Result<ExecPlan> {
    let default_config = ExecToolConfig::default();
    let config = options.config.unwrap_or(&default_config);

    let argv0 = match argv.first() {
        Some(program) if !program.is_empty() => program.as_str(),
        _ => {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                "argv must start with a program to run",
            ));
        }
    };
    if argv.iter().any(|argument| argument.contains('\0')) {
        return Err(denied(
            "Arguments must not contain NUL bytes",
            detail(
                "argv",
                Value::Array(argv.iter().map(|a| Value::from(a.as_str())).collect()),
            ),
        ));
    }
    check_shell(argv, config)?;

    Ok(ExecPlan {
        file: program(argv0, options.jail, options.sandboxed),
        args: argv[1..].to_vec(),
        cwd: options.jail.root().to_path_buf(),
        env: build_env(config, options.env),
        timeout_ms: config.timeout_ms,
        max_output_bytes: config.max_output_bytes,
    })
}

/// What an [`OutputCap`] collected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputCapResult {
    /// The kept bytes, decoded once.
    pub text: String,
    /// Whether the budget was exceeded.
    pub truncated: bool,
    /// Bytes actually kept, which is the cap when `truncated`.
    pub bytes: u64,
}

/// Accumulates child output against a byte budget.
///
/// Bytes, not characters, because the budget exists to bound memory and a
/// character is between one and four of them. Decoding happens once at the end,
/// so a chunk boundary landing mid-codepoint cannot produce a replacement
/// character in the middle of otherwise intact output.
#[derive(Debug, Clone)]
pub struct OutputCap {
    max_bytes: u64,
    kept: Vec<u8>,
    truncated: bool,
}

impl OutputCap {
    /// A cap of `max_bytes`; `0` is unlimited, matching the config convention.
    pub fn new(max_bytes: u64) -> OutputCap {
        OutputCap {
            max_bytes,
            kept: Vec::new(),
            truncated: false,
        }
    }

    /// Adds a chunk. Returns `false` once the budget is spent, so the caller can
    /// stop reading.
    pub fn push(&mut self, chunk: &[u8]) -> bool {
        if self.max_bytes == 0 {
            self.kept.extend_from_slice(chunk);
            return true;
        }
        let kept = u64::try_from(self.kept.len()).unwrap_or(u64::MAX);
        let remaining = self.max_bytes.saturating_sub(kept);
        if remaining == 0 {
            self.truncated = true;
            return false;
        }
        let chunk_len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
        if chunk_len <= remaining {
            self.kept.extend_from_slice(chunk);
            return true;
        }
        let take = usize::try_from(remaining).unwrap_or(usize::MAX);
        self.kept.extend_from_slice(&chunk[..take]);
        self.truncated = true;
        false
    }

    /// Decodes what was kept.
    pub fn done(self) -> OutputCapResult {
        OutputCapResult {
            bytes: u64::try_from(self.kept.len()).unwrap_or(u64::MAX),
            text: String::from_utf8_lossy(&self.kept).into_owned(),
            truncated: self.truncated,
        }
    }
}
