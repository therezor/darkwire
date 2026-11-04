//! The exec guard, against `fixtures/exec/plans.json`.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use crate::common;

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
    assert_eq!(cases(&fixture).len(), 39);
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
        ws.root.join("./build.sh").to_string_lossy()
    );
    assert_eq!(
        guard(&ws, &["/usr/bin/git", "status"]).unwrap().file,
        "/usr/bin/git"
    );
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
fn runs_shells_and_their_program_strings_on_the_host() {
    let ws = workspace();
    for shell in SHELL_BINARIES {
        assert_eq!(guard(&ws, &[shell, "script.js"]).unwrap().file, *shell);
    }
    let plan = guard(&ws, &["bash", "-lc", "ls | wc -l > count.txt"]).unwrap();
    assert_eq!(plan.args, argv(&["-lc", "ls | wc -l > count.txt"]));
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
fn passes_paths_outside_the_workspace_unchanged() {
    let ws = workspace();
    symlink(&ws.base, &ws.root.join("escape"));
    for argument in [
        "../outside.txt",
        "/etc/passwd",
        "~/.ssh/id_ed25519",
        "--output=/etc/passwd",
        "escape/outside.txt",
    ] {
        assert_eq!(
            guard(&ws, &["cat", argument]).unwrap().args,
            argv(&[argument])
        );
    }
}

#[test]
fn anchors_a_relative_program_at_the_workspace_root() {
    let ws = workspace();
    for argv0 in ["./missing/tool", "../tool", "src/run"] {
        assert_eq!(
            guard(&ws, &[argv0]).unwrap().file,
            ws.root.join(argv0).to_string_lossy()
        );
    }
    // A bare name goes to `PATH`, and only `/` separates on this platform.
    for argv0 in ["git", "C:\\Program Files\\Git\\git.exe"] {
        assert_eq!(guard(&ws, &[argv0]).unwrap().file, argv0);
    }
    // In a container the relative path is the container's to resolve.
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

proptest! {
    #[test]
    fn arguments_pass_through_unchanged(argument in "[^\0]{0,40}") {
        let ws = workspace();
        prop_assert_eq!(guard(&ws, &["git", &argument]).unwrap().args, vec![argument]);
    }
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
}

#[test]
fn sees_a_shell_behind_a_launcher() {
    let ws = workspace();
    let off = config(json!({"shell": "deny"}));
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
        let error = guard_with(&ws, call, &off).unwrap_err();
        assert_eq!(error.kind, ErrorKind::PermissionDenied, "{call:?}");
        assert!(guard(&ws, call).is_ok(), "{call:?}");
    }
    // Past `env`'s own options, `-S` belongs to the program it runs.
    assert!(guard_with(&ws, &["env", "cc", "-DSOME"], &off).is_ok());
    assert_eq!(
        shell_call(&argv(&["env", "FOO=1", "sh", "x"])),
        Some("sh".to_owned())
    );
    assert_eq!(
        shell_call(&argv(&["env", "-S", "sh -c id"])),
        Some("env".to_owned())
    );
    assert!(shell_call(&argv(&["timeout", "5", "git", "status"])).is_none());
    assert!(shell_call(&argv(&["env", "CONFIG_SHELL=/bin/sh", "./configure"])).is_none());
    assert!(shell_call(&argv(&["env", "SHELL=/bin/bash", "make"])).is_none());
    assert!(shell_call(&argv(&["nice", "make", "SHELL=/bin/sh"])).is_none());
    assert!(shell_call(&argv(&["git", "sh"])).is_none());
}
