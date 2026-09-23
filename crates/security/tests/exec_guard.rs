//! The exec guard, against `fixtures/exec/plans.json`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use darkwire_core::{ErrorKind, Result};
use darkwire_protocol::ExecToolConfig;
use darkwire_security::exec_guard::{is_shell_name, shell_call};
use darkwire_security::{
    ExecGuardOptions, ExecPlan, JailOptions, OutputCap, SHELL_BINARIES, WorkspaceJail, binary_name,
    guard_exec,
};
use proptest::prelude::*;
use serde_json::{Value, json};

use common::{cases, kind_of, read_fixture, scrub, symlink, temp_base, write};

struct Workspace {
    _dir: tempfile::TempDir,
    base: PathBuf,
    root: PathBuf,
    jail: WorkspaceJail,
}

fn workspace() -> Workspace {
    let (dir, base) = temp_base();
    let root = base.join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    write(&root.join("script.js"), "");
    std::fs::create_dir_all(root.join("src")).unwrap();
    write(&base.join("outside.txt"), "secret");
    let jail = WorkspaceJail::new(JailOptions::new(&root)).unwrap();
    Workspace {
        _dir: dir,
        base,
        root,
        jail,
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn default_env() -> HashMap<String, String> {
    env(&[
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/home/ghost"),
        ("LANG", "en_US.UTF-8"),
        ("AWS_SECRET_ACCESS_KEY", "leak"),
    ])
}

fn config(patch: Value) -> ExecToolConfig {
    serde_json::from_value(patch).unwrap()
}

fn guard(ws: &Workspace, parts: &[&str]) -> Result<ExecPlan> {
    let env = default_env();
    guard_exec(&argv(parts), &ExecGuardOptions::new(&ws.jail, &env))
}

fn guard_with(ws: &Workspace, parts: &[&str], config: &ExecToolConfig) -> Result<ExecPlan> {
    let env = default_env();
    let mut options = ExecGuardOptions::new(&ws.jail, &env);
    options.config = Some(config);
    guard_exec(&argv(parts), &options)
}

fn sandboxed(ws: &Workspace, parts: &[&str], config: Option<&ExecToolConfig>) -> Result<ExecPlan> {
    let env = default_env();
    let mut options = ExecGuardOptions::new(&ws.jail, &env);
    options.sandboxed = true;
    options.config = config;
    guard_exec(&argv(parts), &options)
}

fn plan_value(plan: &ExecPlan, root: &Path, base: &Path) -> Value {
    json!({
        "plan": {
            "file": scrub(&plan.file, root, base),
            "args": plan.args,
            "cwd": scrub(&plan.cwd.to_string_lossy(), root, base),
            "env": plan.env,
            "timeoutMs": plan.timeout_ms,
            "maxOutputBytes": plan.max_output_bytes,
            "paths": plan.paths.iter().map(|p| scrub(&p.to_string_lossy(), root, base)).collect::<Vec<_>>(),
        }
    })
}

#[test]
fn matches_the_plans_fixture() {
    let fixture = read_fixture("exec/plans.json");
    let (dir, base) = temp_base();
    let root = base.join("workspace");
    std::fs::create_dir_all(&root).unwrap();
    write(&root.join("script.js"), "");
    write(&root.join("build.sh"), "");
    std::fs::create_dir_all(root.join("src")).unwrap();
    write(&root.join("src").join("a.ts"), "");
    write(&base.join("outside.txt"), "secret");
    symlink(&base, &root.join("escape"));
    symlink(&base.join("outside.txt"), &root.join("notes.txt"));
    let jail = WorkspaceJail::new(JailOptions::new(&root)).unwrap();

    let mut failures = Vec::new();
    for case in cases(&fixture) {
        let input = &case["input"];
        let argv: Vec<String> = serde_json::from_value(input["argv"].clone()).unwrap();
        let config: ExecToolConfig = serde_json::from_value(input["config"].clone()).unwrap();
        let env: HashMap<String, String> = serde_json::from_value(input["env"].clone()).unwrap();
        let options = ExecGuardOptions {
            jail: &jail,
            config: Some(&config),
            env: &env,
            sandboxed: input["sandboxed"].as_bool().unwrap(),
        };
        let actual = match guard_exec(&argv, &options) {
            Ok(plan) => plan_value(&plan, &root, &base),
            Err(error) => json!({"error": {
                "kind": error.kind.as_str(),
                "message": scrub(&error.message, &root, &base),
            }}),
        };
        if actual != case["output"] {
            failures.push(format!(
                "{}\n  expected {}\n  actual   {}",
                case["name"], case["output"], actual
            ));
        }
    }
    drop(dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(cases(&fixture).len(), 57);
}

#[test]
fn reduces_a_program_to_its_binary_name() {
    let expectations = [
        ("git", "git"),
        ("/usr/bin/git", "git"),
        ("./tools/git", "git"),
        ("git.exe", "git"),
        ("GIT.EXE", "GIT"),
        ("C:\\Program Files\\Git\\git.exe", "git"),
        ("node.cmd", "node"),
        ("script.js", "script.js"),
        (".exe", ".exe"),
        ("", ""),
    ];
    for (input, expected) in expectations {
        assert_eq!(binary_name(input), expected, "{input}");
    }
}

#[test]
fn the_plan_carries_argv_the_root_and_the_caps() {
    let ws = workspace();
    let plan = guard(&ws, &["node", "script.js", "--flag"]).unwrap();
    assert_eq!(plan.file, "node");
    assert_eq!(plan.args, argv(&["script.js", "--flag"]));
    assert_eq!(plan.cwd, ws.root);
    assert_eq!(plan.timeout_ms, 0);
    assert_eq!(plan.max_output_bytes, 1_048_576);

    let capped = guard_with(
        &ws,
        &["git"],
        &config(json!({"timeoutMs": 5000, "maxOutputBytes": 4096})),
    )
    .unwrap();
    assert_eq!(capped.timeout_ms, 5000);
    assert_eq!(capped.max_output_bytes, 4096);

    write(&ws.root.join("build.sh"), "");
    assert_eq!(
        guard(&ws, &["./build.sh"]).unwrap().file,
        ws.root.join("build.sh").to_string_lossy()
    );
    let system = guard(&ws, &["/usr/bin/git", "status"]).unwrap();
    assert_eq!(system.file, "/usr/bin/git");
    assert!(system.paths.is_empty());
}

#[test]
fn the_environment_is_allow_listed_in_order() {
    let ws = workspace();
    let plan = guard(&ws, &["git"]).unwrap();
    assert_eq!(
        plan.env.keys().cloned().collect::<Vec<_>>(),
        ["PATH", "HOME", "LANG"]
    );
    assert!(!plan.env.contains_key("AWS_SECRET_ACCESS_KEY"));

    let narrowed = env(&[("PATH", "/usr/bin"), ("LANG", "C")]);
    let cfg = config(json!({"envAllowlist": ["LANG"]}));
    let mut options = ExecGuardOptions::new(&ws.jail, &narrowed);
    options.config = Some(&cfg);
    let plan = guard_exec(&argv(&["git"]), &options).unwrap();
    assert_eq!(plan.env.get("LANG").unwrap(), "C");
    assert_eq!(plan.env.len(), 1);

    let appended = config(json!({"pathAppend": "/opt/tools/bin"}));
    let with_path = env(&[("PATH", "/usr/bin")]);
    let mut options = ExecGuardOptions::new(&ws.jail, &with_path);
    options.config = Some(&appended);
    assert_eq!(
        guard_exec(&argv(&["git"]), &options).unwrap().env["PATH"],
        "/usr/bin:/opt/tools/bin"
    );
    let empty = HashMap::new();
    let mut options = ExecGuardOptions::new(&ws.jail, &empty);
    options.config = Some(&appended);
    assert_eq!(
        guard_exec(&argv(&["git"]), &options).unwrap().env["PATH"],
        "/opt/tools/bin"
    );
}

#[test]
fn refuses_disabled_empty_and_nul() {
    let ws = workspace();
    assert_eq!(kind_of(&guard(&ws, &[])), "invalid_input");
    assert_eq!(kind_of(&guard(&ws, &[""])), "invalid_input");
    assert_eq!(
        kind_of(&guard(&ws, &["git", "log\0--all"])),
        "permission_denied"
    );
    assert_eq!(kind_of(&guard(&ws, &["git\0", "log"])), "permission_denied");
}

#[test]
fn runs_shells_on_the_host_without_program_strings() {
    let ws = workspace();
    for shell in SHELL_BINARIES {
        assert_eq!(guard(&ws, &[shell, "script.js"]).unwrap().file, *shell);
    }
    // `/C` is cmd's. To bash it is a script path.
    for (shell, flag) in [
        ("bash", "-c"),
        ("bash", "-lc"),
        ("bash", "--command"),
        ("cmd", "/C"),
        ("bash", "-Command"),
        ("bash", "-EncodedCommand"),
    ] {
        let error = guard(&ws, &[shell, flag, "rm -rf / | sh"]).unwrap_err();
        assert_eq!(error.kind, ErrorKind::PermissionDenied, "{flag}");
        assert_eq!(error.details["flag"], json!(flag));
    }
    // Metacharacters are inert without a shell, so they are not scanned for.
    let plan = guard(&ws, &["git", "commit", "-m", "fix $(HOME) && `date` | sh"]).unwrap();
    assert!(plan.args.contains(&"fix $(HOME) && `date` | sh".to_owned()));
}

#[test]
fn refuses_every_shell_when_shells_are_switched_off() {
    let ws = workspace();
    let off = config(json!({"shell": "deny"}));
    for program in ["bash", "/bin/bash", "bash.exe", "pwsh"] {
        let error = guard_with(&ws, &[program, "script.js"], &off).unwrap_err();
        assert_eq!(error.kind, ErrorKind::PermissionDenied, "{program}");
        assert!(error.message.contains("switched off"), "{program}");
    }
    assert!(
        sandboxed(&ws, &["bash", "-lc", "x"], Some(&off))
            .unwrap_err()
            .message
            .contains("switched off")
    );
    // Only shells: every other program is the command rules' to decide.
    assert_eq!(
        guard_with(&ws, &["git", "status"], &off).unwrap().file,
        "git"
    );
}

#[test]
fn refuses_path_shaped_arguments_that_point_outside() {
    let ws = workspace();
    for argument in [
        "../outside.txt",
        "src/../../outside.txt",
        "/etc/passwd",
        "~/.ssh/id_ed25519",
        "\\\\server\\share",
        "C:\\Windows\\System32",
        "--output=../outside.txt",
        "--output=/etc/passwd",
    ] {
        let error = guard(&ws, &["cat", argument]).unwrap_err();
        assert_eq!(error.kind, ErrorKind::JailEscape, "{argument}");
        assert!(error.details["shapes"].is_array());
    }
    symlink(&ws.base, &ws.root.join("escape"));
    let escaped = guard(&ws, &["cat", "escape/outside.txt"]).unwrap_err();
    assert_eq!(escaped.kind, ErrorKind::JailEscape);
    assert_eq!(escaped.details["rejection"], json!("outside_root"));
    symlink(&ws.base.join("outside.txt"), &ws.root.join("notes.txt"));
    assert_eq!(kind_of(&guard(&ws, &["cat", "notes.txt"])), "jail_escape");
    // The jail clamps what the guard refuses: the one place the two differ.
    assert!(ws.jail.check("/etc/passwd").is_ok());
}

#[test]
fn refuses_program_paths_that_point_outside() {
    let ws = workspace();
    for argv0 in ["../evil.sh", "~/bin/evil"] {
        assert_eq!(kind_of(&guard(&ws, &[argv0])), "jail_escape", "{argv0}");
    }
    let missing = guard(&ws, &["./missing/tool"]).unwrap();
    assert_eq!(
        missing.file,
        ws.root.join("missing").join("tool").to_string_lossy()
    );
    symlink(&ws.base, &ws.root.join("escape"));
    let error = guard(&ws, &["./escape/tool"]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::JailEscape);
    assert!(
        error
            .message
            .contains("Program path is not inside the workspace")
    );

    let drive = guard(&ws, &["C:\\Program Files\\Git\\git.exe"]).unwrap();
    assert_eq!(drive.file, "C:\\Program Files\\Git\\git.exe");
}

#[test]
fn accepts_ordinary_arguments_and_records_paths() {
    let ws = workspace();
    assert_eq!(
        guard(&ws, &["git", "log", "", "--oneline"]).unwrap().args,
        argv(&["log", "", "--oneline"])
    );
    for argument in [
        "--all",
        "-m",
        "fix the thing",
        "--format=json",
        "--output=",
        "src/index.ts",
        "./script.js",
        "42",
    ] {
        assert_eq!(
            guard(&ws, &["git", argument]).unwrap().args,
            argv(&[argument])
        );
    }
    write(&ws.root.join("src").join("a.ts"), "");
    let plan = guard(&ws, &["node", "script.js", "src/a.ts", "--out=src/b.ts"]).unwrap();
    assert_eq!(
        plan.paths,
        [
            ws.root.join("src").join("a.ts"),
            ws.root.join("src").join("b.ts")
        ]
    );
    let url = guard(&ws, &["curl", "https://example.com/a/b"]).unwrap();
    assert!(url.paths.is_empty());

    let message = "x".repeat(5000);
    assert!(
        guard(&ws, &["git", "commit", "-m", &message])
            .unwrap()
            .args
            .contains(&message)
    );
    assert_eq!(
        kind_of(&guard(&ws, &["cat", &format!("src/{}", "x".repeat(5000))])),
        "jail_escape"
    );
}

#[test]
fn sandboxed_lifts_the_shell_and_path_rules_together() {
    let ws = workspace();
    let plan = sandboxed(
        &ws,
        &["bash", "-lc", "nmap -sV 10.0.0.5 | tee scan.txt"],
        None,
    )
    .unwrap();
    assert_eq!(plan.file, "bash");
    assert!(sandboxed(&ws, &["cat", "/etc/os-release"], None).is_ok());
    assert!(sandboxed(&ws, &["nmap", "-oN", "/tmp/scan.txt", "10.0.0.5"], None).is_ok());
    assert!(
        sandboxed(
            &ws,
            &["sh", "-c", "nuclei -u http://t > /workspace/out.txt"],
            None
        )
        .is_ok()
    );
    // A relative program path is not resolved either: the container resolves it.
    assert_eq!(
        sandboxed(&ws, &["./build.sh"], None).unwrap().file,
        "./build.sh"
    );

    assert!(
        sandboxed(&ws, &["nmap", "a\0b"], None)
            .unwrap_err()
            .message
            .contains("NUL")
    );
    assert!(
        guard(&ws, &["bash", "-lc", "x"])
            .unwrap_err()
            .message
            .contains("shell")
    );
    assert!(
        guard(&ws, &["cat", "/etc/passwd"])
            .unwrap_err()
            .message
            .contains("outside")
    );
}

#[test]
fn options_and_plans_can_be_printed() {
    let ws = workspace();
    let env = default_env();
    let options = ExecGuardOptions::new(&ws.jail, &env);
    assert!(format!("{options:?}").contains("ExecGuardOptions"));
    let plan = guard(&ws, &["git"]).unwrap();
    assert_eq!(plan, plan.clone());
}

// Output cap

#[test]
fn keeps_output_that_fits() {
    let mut cap = OutputCap::new(16);
    assert!(cap.push(b"hello "));
    assert!(cap.push(b"world"));
    let result = cap.done();
    assert_eq!(result.text, "hello world");
    assert!(!result.truncated);
    assert_eq!(result.bytes, 11);
}

#[test]
fn fills_exactly_to_the_budget_without_truncation() {
    let mut cap = OutputCap::new(5);
    assert!(cap.push(b"12345"));
    let result = cap.done();
    assert_eq!(
        (result.text.as_str(), result.truncated, result.bytes),
        ("12345", false, 5)
    );
}

#[test]
fn truncates_mid_chunk_and_refuses_afterwards() {
    let mut cap = OutputCap::new(8);
    assert!(cap.push(b"12345"));
    assert!(!cap.push(b"67890"));
    assert!(!cap.push(b"x"));
    let result = cap.done();
    assert_eq!(
        (result.text.as_str(), result.truncated, result.bytes),
        ("12345678", true, 8)
    );

    let mut spent = OutputCap::new(2);
    assert!(spent.push(b"ab"));
    assert!(!spent.push(b"c"));
    assert!(!spent.push(b"d"));
    assert_eq!(spent.done().text, "ab");
}

#[test]
fn zero_is_unlimited() {
    let mut cap = OutputCap::new(0);
    for _ in 0..100 {
        assert!(cap.push(&[b'x'; 100]));
    }
    let result = cap.done();
    assert!(!result.truncated);
    assert_eq!(result.bytes, 10_000);
}

#[test]
fn decodes_once_at_the_end_and_counts_bytes() {
    let emoji = "🐕".as_bytes();
    let mut cap = OutputCap::new(16);
    cap.push(&emoji[..2]);
    cap.push(&emoji[2..]);
    assert_eq!(cap.done().text, "🐕");

    let mut tight = OutputCap::new(4);
    assert!(!tight.push("🐕🐕".as_bytes()));
    let result = tight.done();
    assert_eq!(
        (result.text.as_str(), result.truncated, result.bytes),
        ("🐕", true, 4)
    );
    assert!(format!("{result:?}").contains("OutputCapResult"));
}

// Properties

/// The pre-chroot syntactic rule, written out again as an independent oracle.
fn refused_before(input: &str) -> bool {
    if input.is_empty() || input.contains('\0') {
        return true;
    }
    let segments: Vec<&str> = input.split(['/', '\\']).collect();
    if segments.first().is_some_and(|s| s.starts_with('~')) {
        return true;
    }
    if input.starts_with("\\\\") || input.starts_with("//") {
        return true;
    }
    let drive = {
        let mut chars = input.chars();
        matches!((chars.next(), chars.next()), (Some(l), Some(':')) if l.is_ascii_alphabetic())
    };
    if input.starts_with('/') || input.starts_with('\\') || drive {
        return true;
    }
    segments.contains(&"..")
}

fn candidate_of(argument: &str) -> String {
    if !argument.starts_with('-') {
        return argument.to_owned();
    }
    argument
        .find('=')
        .map_or_else(String::new, |i| argument[i + 1..].to_owned())
}

fn shape_fragments() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "..", "../", "..\\", "/", "\\", "//", "~", "~/", "C:", "c:", ".", "./", "src", "a.ts",
        "plain", "--out=", "-m",
    ])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn accepts_exactly_when_the_pre_chroot_rule_would_have(parts in prop::collection::vec(shape_fragments(), 1..=6)) {
        let ws = workspace();
        let argument = parts.concat();
        let candidate = candidate_of(&argument);
        let expected = !candidate.is_empty() && refused_before(&candidate);
        prop_assert_eq!(guard(&ws, &["git", &argument]).is_err(), expected, "{}", argument);
    }

    #[test]
    fn every_accepted_path_is_contained_and_argv_is_unchanged(parts in prop::collection::vec(
        prop::sample::select(vec!["..", "../", "/", "\\", "~", "C:", "src", "a.ts", ".", "--out=", "-m", "plain", "\0"]),
        1..=6,
    )) {
        let ws = workspace();
        let argument = parts.concat();
        if let Ok(plan) = guard(&ws, &["git", &argument]) {
            for path in &plan.paths {
                prop_assert!(ws.jail.contains(path));
            }
            prop_assert_eq!(plan.args, vec![argument]);
        }
    }
}

/// A bare name goes to `PATH` and an absolute one is a system binary; only the
/// workspace-relative shape is the jail's business.
fn workspace_relative_program() -> impl Strategy<Value = String> {
    prop::collection::vec(shape_fragments(), 1..=4)
        .prop_map(|parts| parts.concat())
        .prop_filter("a workspace-relative program path", |argv0| {
            let drive = {
                let mut chars = argv0.chars();
                matches!((chars.next(), chars.next()), (Some(l), Some(':')) if l.is_ascii_alphabetic())
            };
            !argv0.is_empty()
                && !argv0.starts_with('/')
                && !drive
                && (argv0.contains('/') || argv0.contains('\\') || argv0.starts_with('~'))
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn program_paths_follow_the_same_rule(argv0 in workspace_relative_program()) {
        let ws = workspace();
        let refused = matches!(guard(&ws, &[&argv0]), Err(error) if error.kind == ErrorKind::JailEscape);
        prop_assert_eq!(refused, refused_before(&argv0), "{}", argv0);
    }
}

proptest! {
    #[test]
    fn never_accepts_a_traversal_segment(
        segments in prop::collection::vec(prop::sample::select(vec!["a", "b", ".."]), 1..=5),
        prefix in prop::sample::select(vec!["", "--out="]),
    ) {
        prop_assume!(segments.contains(&".."));
        let ws = workspace();
        let argument = format!("{prefix}{}", segments.join("/"));
        prop_assert!(guard(&ws, &["git", &argument]).is_err());
    }
}

#[test]
fn refuses_a_program_string_hidden_in_a_cluster_of_short_flags() {
    let ws = workspace();
    for flags in ["-ec", "-xc", "-cx", "-eux"] {
        let call = ["bash", flags, "rm -rf ~"];
        let refused = guard(&ws, &call);
        if flags == "-eux" {
            assert!(refused.is_ok(), "{flags}");
            continue;
        }
        let error = refused.unwrap_err();
        assert_eq!(error.kind, ErrorKind::PermissionDenied, "{flags}");
        assert_eq!(error.details["flag"], json!(flags));
    }
    assert!(guard(&ws, &["bash", "-e", "script.sh"]).is_ok());
    assert!(guard(&ws, &["fish", "-C", "echo hi"]).is_err());
    assert!(guard(&ws, &["cmd", "/r", "dir"]).is_err());
}

#[test]
fn reads_powershell_prefixes_without_refusing_its_ordinary_options() {
    let ws = workspace();
    for flag in ["-e", "-ec", "-enc", "-com", "-C"] {
        assert!(guard(&ws, &["pwsh", flag, "x"]).is_err(), "{flag}");
    }
    assert!(guard(&ws, &["pwsh", "-NonInteractive", "-File", "x.ps1"]).is_ok());
    assert!(guard(&ws, &["pwsh", "-ExecutionPolicy", "Bypass", "x.ps1"]).is_ok());
}

#[test]
fn knows_a_shell_whatever_its_case() {
    let ws = workspace();
    let off = config(json!({"shell": "deny"}));
    for program in ["Bash", "SH", "/bin/ZSH", "PWSH.EXE"] {
        assert!(is_shell_name(&binary_name(program)), "{program}");
        let error = guard_with(&ws, &[program, "script.sh"], &off).unwrap_err();
        assert_eq!(error.kind, ErrorKind::PermissionDenied, "{program}");
    }
    assert!(guard(&ws, &["BASH", "-c", "id"]).is_err());
}

#[test]
fn sees_a_shell_behind_a_launcher() {
    let ws = workspace();
    for call in [
        &["env", "sh", "-c", "id"][..],
        &["env", "-i", "FOO=1", "bash", "-ec", "id"],
        &["nice", "-n", "5", "sh", "-c", "id"],
        &["timeout", "5", "/bin/sh", "-c", "id"],
        &["xargs", "-0", "sh", "-c", "id"],
        &["nohup", "env", "zsh", "-c", "id"],
        &["env", "-S", "sh -c id"],
        &["env", "--split-string=sh -c id"],
    ] {
        let error = guard(&ws, call).unwrap_err();
        assert_eq!(error.kind, ErrorKind::PermissionDenied, "{call:?}");
    }
    let off = config(json!({"shell": "deny"}));
    assert!(guard_with(&ws, &["timeout", "5", "bash", "script.sh"], &off).is_err());
    assert!(guard(&ws, &["timeout", "5", "bash", "script.sh"]).is_ok());
    // Past `env`'s own options, `-S` belongs to the program it runs.
    assert!(guard(&ws, &["env", "cc", "-DSOME"]).is_ok());
    assert_eq!(
        shell_call(&argv(&["env", "FOO=1", "sh", "x"])).map(|call| call.name),
        Some("sh".to_owned())
    );
    assert!(shell_call(&argv(&["timeout", "5", "git", "status"])).is_none());
    assert!(shell_call(&argv(&["env", "CONFIG_SHELL=/bin/sh", "./configure"])).is_none());
    assert!(shell_call(&argv(&["env", "SHELL=/bin/bash", "make"])).is_none());
    assert!(shell_call(&argv(&["nice", "make", "SHELL=/bin/sh"])).is_none());
    assert!(shell_call(&argv(&["git", "sh"])).is_none());
}

#[test]
fn reads_shell_options_only_before_the_first_operand() {
    let ws = workspace();
    // `-clean` is an argument to the script, not an option to bash.
    assert!(guard(&ws, &["bash", "build.sh", "-clean"]).is_ok());
    assert!(guard(&ws, &["bash", "--", "-c"]).is_ok());
    for call in [
        &["bash", "-o", "pipefail", "-c", "x"][..],
        &["bash", "+O", "extglob", "-ec", "x"],
        &["bash", "-eo", "pipefail", "-c", "x"],
        &["bash", "--rcfile", "rc", "-c", "x"],
        &["busybox", "sh", "-c", "x"],
    ] {
        let error = guard(&ws, call).unwrap_err();
        assert_eq!(error.kind, ErrorKind::PermissionDenied, "{call:?}");
        assert_eq!(error.details["flag"], json!(call[call.len() - 2]));
    }
    // The value of `-o` is not read as an option.
    assert!(guard(&ws, &["bash", "-o", "-c", "script.sh"]).is_ok());
}
