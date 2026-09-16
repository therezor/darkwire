//! The exec guard: an argv vector in, a validated spawn plan out.
//!
//! What is deliberately absent is a deny-list of shell metacharacters. Patterns
//! for `$(...)`, backticks and `| sh` are the standard shape of this code and
//! they are theatre when the executor is `Command::new(argv[0])` with no shell:
//! there is no string for a metacharacter to be interpreted *in*, so the
//! patterns can only ever reject legitimate arguments — a commit message
//! containing `$HOME`, a grep for a pipe character. They protect nothing and
//! break real commands, which is the worst trade available.
//!
//! What actually constrains the child is here instead:
//!
//!  - `argv[0]` against a deny-list, then an allow-list, matched on the basename
//!    so `/usr/bin/git`, `git` and `git.exe` receive the same verdict.
//!  - A shell binary is refused unless the operator listed it explicitly, and the
//!    `-c` family of flags is refused even then. Handing `bash -c "…"` to a
//!    shell-less spawn re-creates the shell parsing the argv contract removes;
//!    that is the one thing on this list a metacharacter scan would have been
//!    aiming at.
//!  - Every path-shaped argument through the workspace jail.
//!  - An environment allow-list, so the child inherits `PATH` and nothing that
//!    happens to hold a token. The map is supplied by the caller rather than
//!    read from the process, so the guard can be run over an environment it did
//!    not inherit.
//!  - An output budget, enforced while the child writes rather than after it
//!    exits.
//!
//! The guard validates and never rewrites. A path-shaped argument that passes
//! stays exactly as the model wrote it, because the child's working directory is
//! the workspace root and substituting an absolute path would corrupt any
//! argument that only looked like a path — `git log a/b`, a regex, a URL.
//!
//! Two of those rules — the shell refusal and the path refusal — are premised on
//! the child being unconfined, and both are lifted when `sandboxed` says it is
//! not. See [`ExecGuardOptions::sandboxed`]; the short version is that a
//! container mounting only the workspace enforces by construction what they
//! enforce by inspection, and lifting one without the other produces a shell
//! that refuses the redirects it was enabled for.
//!
//! That is also why this is the one place in DarkWire where a path outside the
//! workspace is **refused instead of clamped**. [`WorkspaceJail`] resolves
//! `/etc/passwd` to `<workspace>/etc/passwd`, but clamping is a property of
//! *its* resolution and a spawned child does not honour it: handing `/etc/passwd`
//! to `cat` with the working directory at the workspace root reads the real
//! file. So every argument is classified with [`path_shapes`] — the jail's own
//! lexical rules, reported rather than applied — and a non-empty result stops
//! the command before any filesystem work happens.

use std::collections::HashMap;
use std::path::PathBuf;

use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::ExecToolConfig;
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::jail::{JailCheck, JailRejection, PathShape, WorkspaceJail, path_shapes};

/// Binaries whose whole purpose is to interpret a string as a program.
///
/// Refused unless named in `allowed_binaries`: an operator who wants
/// `bash script.sh` can say so, and gets it without the `-c` family.
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

/// Flags that make a shell — or `env`, `perl`, `python` — take a program string.
const PROGRAM_STRING_FLAGS: &[&str] = &[
    "-c",
    "-lc",
    "-ic",
    "--command",
    "/c",
    "/k",
    "-command",
    "-encodedcommand",
];

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
    /// Whether the command will run in a container that mounts only the workspace.
    ///
    /// Two of the rules exist because a host child process is not confined by
    /// anything: a shell would reintroduce a parser between the argv contract and
    /// the kernel, and an absolute path would reach the real filesystem. Move the
    /// command into a container and both premises fail — the only filesystem it can
    /// address *is* the workspace, and a shell inside it can reach nothing the
    /// command could not reach anyway.
    ///
    /// So both are lifted here, together. Lifting only the shell ban would be worse
    /// than lifting neither: `bash -lc 'nmap … > /tmp/out'` carries its redirect
    /// inside the script string, the path check sees `/tmp/out` and refuses, and the
    /// operator gets a shell that rejects the pipelines they enabled it for.
    ///
    /// What does **not** change: the binary allow- and deny-lists still apply, and
    /// every argv is still recorded. The container is the boundary; the audit log is
    /// still the record.
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
    /// The canonical paths that were validated, for the approval prompt and audit log.
    pub paths: Vec<PathBuf>,
}

fn denied(message: impl Into<String>, details: Map<String, Value>) -> WireError {
    WireError::new(ErrorKind::PermissionDenied, message).with_details(details)
}

fn detail(key: &str, value: impl Into<Value>) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(key.to_owned(), value.into());
    map
}

/// The name an allow/deny entry is compared against.
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

fn has_drive_letter(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    )
}

fn is_path_shaped(value: &str) -> bool {
    value == "."
        || value == ".."
        || value.starts_with('~')
        || has_drive_letter(value)
        || value.contains('/')
        || value.contains('\\')
}

/// The part of an argument that could be addressing the filesystem.
///
/// Every argument is a candidate, not only the ones that look like paths: `cat
/// notes.txt` where `notes.txt` is a symlink to `/etc/shadow` is precisely the
/// escape the jail exists to catch, and a separator-based heuristic would wave it
/// through. Arguments that are plainly not paths — a flag, a commit message —
/// cost nothing to check, because a relative string with no `..` in it always
/// resolves inside the workspace and passes.
fn path_candidate(argument: &str) -> Option<&str> {
    if argument.starts_with('-') {
        // `--out=../etc/passwd`: the flag is not a path, its value is.
        let value = &argument[argument.find('=')? + 1..];
        return (!value.is_empty()).then_some(value);
    }
    (!argument.is_empty()).then_some(argument)
}

/// Whether a refusal should stop the command.
///
/// Everything except `unverifiable` is an argument reaching for something outside
/// the workspace, and is fatal. `unverifiable` is not, on its own: a 5000-character
/// commit message is a path the filesystem cannot answer for, and refusing to
/// commit because a message was long would be the guard inventing a rule nobody
/// asked for. Such an argument is only fatal if it is *shaped* like a path.
fn is_fatal_rejection(rejection: JailRejection, candidate: &str) -> bool {
    rejection != JailRejection::Unverifiable || is_path_shaped(candidate)
}

fn shapes_value(shapes: &[PathShape]) -> Value {
    Value::Array(shapes.iter().map(|s| Value::from(s.as_str())).collect())
}

fn shapes_list(shapes: &[PathShape]) -> String {
    shapes
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Refuses a string whose shape addresses something outside the workspace.
///
/// Checked before the jail, which would clamp it and answer accepted — and
/// cheaper, since it touches no filesystem.
fn assert_inside_by_shape(
    candidate: &str,
    what: &str,
    mut details: Map<String, Value>,
) -> Result<()> {
    let shapes = path_shapes(candidate);
    if shapes.is_empty() {
        return Ok(());
    }
    details.insert("path".to_owned(), Value::from(candidate));
    details.insert("shapes".to_owned(), shapes_value(&shapes));
    Err(WireError::new(
        ErrorKind::JailEscape,
        format!(
            "{what} points outside the workspace ({}): {candidate}",
            shapes_list(&shapes)
        ),
    )
    .with_details(details))
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

fn check_binary(
    name: &str,
    argv: &[String],
    config: &ExecToolConfig,
    sandboxed: bool,
) -> Result<()> {
    if config.denied_binaries.iter().any(|d| d == name) {
        return Err(denied(
            format!("Binary is denied by configuration: {name}"),
            detail("binary", name),
        ));
    }
    let allowed = &config.allowed_binaries;
    if !allowed.is_empty() && !allowed.iter().any(|a| a == name) {
        let mut details = detail("binary", name);
        details.insert(
            "allowed".to_owned(),
            Value::Array(allowed.iter().map(|a| Value::from(a.as_str())).collect()),
        );
        return Err(denied(
            format!("Binary is not in the allow-list: {name}"),
            details,
        ));
    }
    if SHELL_BINARIES.contains(&name) && !sandboxed {
        if !allowed.iter().any(|a| a == name) {
            return Err(denied(
                format!(
                    "{name} is a shell. Pass the program and its arguments as argv instead, or add \"{name}\" to allowedBinaries."
                ),
                detail("binary", name),
            ));
        }
        if let Some(flag) = argv
            .iter()
            .skip(1)
            .find(|argument| PROGRAM_STRING_FLAGS.contains(&argument.to_lowercase().as_str()))
        {
            let mut details = detail("binary", name);
            details.insert("flag".to_owned(), Value::from(flag.as_str()));
            return Err(denied(
                format!(
                    "{name} {flag} re-introduces shell parsing. Pass the program and its arguments as argv instead."
                ),
                details,
            ));
        }
    }
    Ok(())
}

/// A program given as a path must be inside the workspace; a bare name is
/// resolved from `PATH` by the OS, and an absolute path is a system binary the
/// allow/deny lists have already ruled on.
fn check_program(argv0: &str, jail: &WorkspaceJail, paths: &mut Vec<PathBuf>) -> Result<String> {
    if !is_path_shaped(argv0) || argv0.starts_with('/') || has_drive_letter(argv0) {
        return Ok(argv0.to_owned());
    }
    assert_inside_by_shape(argv0, "Program path", Map::new())?;
    match jail.check(argv0) {
        JailCheck::Accept(accept) => {
            paths.push(accept.path.clone());
            Ok(accept.path.to_string_lossy().into_owned())
        }
        JailCheck::Reject { rejection, message } => Err(WireError::new(
            ErrorKind::JailEscape,
            format!("Program path is not inside the workspace: {message}"),
        )
        .with_detail("path", argv0)
        .with_detail("rejection", rejection.as_str())),
    }
}

fn check_argument(argument: &str, jail: &WorkspaceJail, paths: &mut Vec<PathBuf>) -> Result<()> {
    let Some(candidate) = path_candidate(argument) else {
        return Ok(());
    };
    assert_inside_by_shape(candidate, "Argument", detail("argument", argument))?;
    match jail.check(candidate) {
        JailCheck::Accept(accept) => {
            // Recorded for the approval prompt and the audit log, so only arguments
            // the caller actually wrote as paths are listed. A URL is excluded: it
            // resolves harmlessly inside the workspace, but reporting it as a file
            // it touches would be a lie.
            if is_path_shaped(candidate) && !candidate.contains("://") {
                paths.push(accept.path);
            }
            Ok(())
        }
        JailCheck::Reject { rejection, message } => {
            if !is_fatal_rejection(rejection, candidate) {
                return Ok(());
            }
            Err(WireError::new(
                ErrorKind::JailEscape,
                format!("Argument is not inside the workspace: {message}"),
            )
            .with_detail("argument", argument)
            .with_detail("path", candidate)
            .with_detail("rejection", rejection.as_str()))
        }
    }
}

/// Validates an argv vector and returns the plan to spawn it.
///
/// Errors rather than returning a verdict: there is no partially-acceptable
/// command, and a caller that forgot to check a boolean would spawn it anyway.
pub fn guard_exec(argv: &[String], options: &ExecGuardOptions<'_>) -> Result<ExecPlan> {
    let default_config = ExecToolConfig::default();
    let config = options.config.unwrap_or(&default_config);
    let jail = options.jail;

    if !config.enable {
        return Err(denied(
            "The exec tool is disabled by configuration",
            Map::new(),
        ));
    }

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

    let name = binary_name(argv0);
    check_binary(&name, argv, config, options.sandboxed)?;

    let mut paths: Vec<PathBuf> = Vec::new();
    let file = if options.sandboxed {
        argv0.to_owned()
    } else {
        check_program(argv0, jail, &mut paths)?
    };
    if !options.sandboxed {
        for argument in argv.iter().skip(1) {
            check_argument(argument, jail, &mut paths)?;
        }
    }

    Ok(ExecPlan {
        file,
        args: argv[1..].to_vec(),
        cwd: jail.root().to_path_buf(),
        env: build_env(config, options.env),
        timeout_ms: config.timeout_ms,
        max_output_bytes: config.max_output_bytes,
        paths,
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
