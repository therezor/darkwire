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
//!  - A shell binary is refused when the agent's `shell` permission is `deny`,
//!    and on the host the `-c` family of flags is refused whatever it says. A
//!    shell behind a launcher such as `env` or `timeout` counts.
//!    Handing `bash -c "…"` to a shell-less spawn re-creates the shell parsing
//!    the argv contract removes; that is the one thing on this list a
//!    metacharacter scan would have been aiming at.
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
use darkwire_protocol::{ExecToolConfig, ToolPermission};
use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::jail::{JailCheck, JailRejection, PathShape, WorkspaceJail, path_shapes};

/// Binaries whose whole purpose is to interpret a string as a program.
///
/// A call to one of these is a shell call: the agent's `shell` permission caps
/// it, and no wildcard command rule can approve it. On the host it runs without
/// the `-c` family, so `bash script.sh` works and `bash -c "…"` does not.
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
    "/r",
    "-command",
    "-encodedcommand",
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

/// The shell an argv runs, directly or behind a launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCall {
    /// The shell's binary name, or the launcher's when `env -S` is the shell.
    pub name: String,
    /// Where the shell's own arguments start.
    pub args_from: usize,
    /// `env -S` or `--split-string`, which parses a string into a command.
    pub split_string: Option<String>,
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

/// The shell `argv` runs, if any.
pub fn shell_call(argv: &[String]) -> Option<ShellCall> {
    let program = binary_name(argv.first()?);
    if is_shell_name(&program) {
        return Some(ShellCall {
            name: program,
            args_from: 1,
            split_string: None,
        });
    }
    if !LAUNCHERS.contains(&program.to_lowercase().as_str()) {
        return None;
    }
    // Only `env`'s own options can be `-S`: past them, `-S` belongs to
    // whatever `env` runs.
    let mut env_options = program.eq_ignore_ascii_case("env");
    let mut value_next = false;
    for (index, argument) in argv.iter().enumerate().skip(1) {
        if env_options {
            if value_next {
                value_next = false;
                continue;
            }
            if is_split_string(argument) {
                return Some(ShellCall {
                    name: "env".to_owned(),
                    args_from: index + 1,
                    split_string: Some(argument.clone()),
                });
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
            return Some(ShellCall {
                name,
                args_from: index + 1,
                split_string: None,
            });
        }
        if name.eq_ignore_ascii_case("env") {
            env_options = true;
        }
    }
    None
}

/// Options of a POSIX shell that take the next argument as their value.
const SHELL_VALUE_OPTIONS: &[&str] = &[
    "--rcfile",
    "--init-file",
    "-d",
    "--debug",
    "--debug-output",
    "--profile",
    "--profile-startup",
    "-f",
    "--features",
];

/// Whether a shell option consumes the argument after it: `-o pipefail`,
/// `+O extglob`, or a cluster ending in either, such as `-eo pipefail`.
fn takes_value(argument: &str) -> bool {
    if SHELL_VALUE_OPTIONS.contains(&argument) {
        return true;
    }
    argument
        .strip_prefix('-')
        .or_else(|| argument.strip_prefix('+'))
        .is_some_and(|cluster| {
            !cluster.starts_with('-')
                && cluster.bytes().all(|c| c.is_ascii_alphabetic())
                && cluster.contains(['o', 'O'])
        })
}

/// The argument that makes the shell `name` take a program string, if any.
///
/// A POSIX shell reads options only before its first operand, so
/// `bash build.sh -clean` hands `-clean` to the script. The scan stops at the
/// first argument not starting with `-` or `+`, and at `--`. `busybox`'s first
/// argument is the applet, so its options start after it. PowerShell and cmd
/// are scanned whole.
fn program_string_flag<'a>(name: &str, args: &'a [String]) -> Option<&'a String> {
    let lower = name.to_lowercase();
    if matches!(lower.as_str(), "cmd" | "powershell" | "pwsh") {
        return args
            .iter()
            .find(|argument| is_program_string_flag(name, argument));
    }
    let mut args = args.iter().peekable();
    if lower == "busybox" {
        args.next();
    }
    // A first argument spelled like another shell's flag, such as `/C`, is
    // refused as that flag rather than run as a script path.
    if let Some(first) = args.peek()
        && PROGRAM_STRING_FLAGS.contains(&first.to_lowercase().as_str())
    {
        return Some(first);
    }
    while let Some(argument) = args.next() {
        if argument == "--" || !(argument.starts_with('-') || argument.starts_with('+')) {
            return None;
        }
        if is_program_string_flag(name, argument) {
            return Some(argument);
        }
        if takes_value(argument) {
            args.next();
        }
    }
    None
}

/// Whether one argument makes the shell `name` take a program string.
///
/// POSIX shells take `-c` in any cluster of short flags, so `-ec` and `-xc`
/// count. PowerShell spells long options with one dash and accepts any
/// unambiguous prefix, so `-e` and `-enc` are `-EncodedCommand`, and so is its
/// alias `-ec`. A cluster rule there would refuse `-NonInteractive`.
fn is_program_string_flag(name: &str, argument: &str) -> bool {
    let lower = argument.to_lowercase();
    if PROGRAM_STRING_FLAGS.contains(&lower.as_str()) {
        return true;
    }
    match name.to_lowercase().as_str() {
        "cmd" => ["/c", "/k", "/r"]
            .iter()
            .any(|flag| lower.starts_with(flag)),
        "powershell" | "pwsh" => {
            lower == "-ec"
                || (lower.len() >= 2
                    && lower.starts_with('-')
                    && ["-command", "-encodedcommand"]
                        .iter()
                        .any(|full| full.starts_with(lower.as_str())))
        }
        shell => {
            // fish's `-C` runs its init command, which is a program string too.
            let fish = shell == "fish";
            (fish && lower.starts_with("--init-command"))
                || argument.strip_prefix('-').is_some_and(|cluster| {
                    !cluster.is_empty()
                        && cluster.bytes().all(|c| c.is_ascii_alphabetic())
                        && (cluster.contains('c') || (fish && cluster.contains('C')))
                })
        }
    }
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
    /// What does **not** change: a `shell: deny` still refuses a shell, and
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

fn check_binary(argv: &[String], config: &ExecToolConfig, sandboxed: bool) -> Result<()> {
    let Some(call) = shell_call(argv) else {
        return Ok(());
    };
    let name = call.name.as_str();
    if config.shell == ToolPermission::Deny {
        return Err(denied(
            format!(
                "{name} is a shell, and shells are switched off for this agent. Pass the program and its arguments as argv instead."
            ),
            detail("binary", name),
        ));
    }
    let shell_args = argv.get(call.args_from..).unwrap_or_default();
    let flag = call
        .split_string
        .as_ref()
        .or_else(|| program_string_flag(name, shell_args));
    if !sandboxed && let Some(flag) = flag {
        let mut details = detail("binary", name);
        details.insert("flag".to_owned(), Value::from(flag.as_str()));
        return Err(denied(
            format!(
                "{name} {flag} re-introduces shell parsing. Pass the program and its arguments as argv instead."
            ),
            details,
        ));
    }
    Ok(())
}

/// A program given as a path must be inside the workspace; a bare name is
/// resolved from `PATH` by the OS, and an absolute path is a system binary the
/// agent's command rules have already ruled on.
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

    check_binary(argv, config, options.sandboxed)?;

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
