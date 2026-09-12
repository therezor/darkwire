//! The container runner: argv builders asserted without a daemon, the
//! transcript, and the runner over an injected client.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::path::PathBuf;
use std::sync::Arc;

use ghostai_core::{ErrorKind, GhostError, Result, SystemClock};
use ghostai_protocol::{Toolbox, ToolboxNetworkMode};
use ghostai_security::{EffectiveNetwork, ExecPlan};
use ghostai_tools::{
    BoxFuture, CommandRunner, ContainerCreateOptions, ContainerExecOptions, ContainerRunner,
    ContainerRunnerOptions, KillSignal, OutputStream, OutputTee, RUNS_MOUNT_DIR, RunOutcome,
    RunRequest, TOOLBOX_MOUNT_DIR, ToolboxMount, Transcript, container_create_argv,
    container_exec_argv, container_is_gone, container_kill_argv, container_run_dir,
};
use indexmap::IndexMap;
use parking_lot::Mutex;
use proptest::prelude::*;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const DIGEST: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn toolbox_of(overrides: Value) -> Toolbox {
    let mut base = json!({
        "schema": "ghostai.toolbox/1",
        "name": "kali",
        "image": format!("kalilinux/kali-rolling@{DIGEST}"),
    });
    if let (Value::Object(base), Value::Object(overrides)) = (&mut base, overrides) {
        for (key, value) in overrides {
            base.insert(key, value);
        }
    }
    serde_json::from_value(base).unwrap()
}

fn plan_of() -> ExecPlan {
    let mut env = IndexMap::new();
    env.insert("PATH".to_owned(), "/host/bin".to_owned());
    env.insert("LANG".to_owned(), "en_GB.UTF-8".to_owned());
    env.insert("SECRET".to_owned(), "nope".to_owned());
    ExecPlan {
        file: "nmap".to_owned(),
        args: vec!["-sV".to_owned(), "10.0.0.5".to_owned()],
        cwd: PathBuf::from("/host/workspace"),
        env,
        timeout_ms: 30_000,
        max_output_bytes: 1024,
        paths: Vec::new(),
    }
}

fn none() -> EffectiveNetwork {
    EffectiveNetwork {
        mode: ToolboxNetworkMode::None,
        allow: Vec::new(),
        dns: Vec::new(),
        proxy_allow_hosts: Vec::new(),
    }
}

fn network(mode: ToolboxNetworkMode) -> EffectiveNetwork {
    EffectiveNetwork {
        mode,
        allow: vec!["10.0.0.0/8".to_owned()],
        dns: Vec::new(),
        proxy_allow_hosts: Vec::new(),
    }
}

fn mount() -> ToolboxMount {
    ToolboxMount {
        host_path: "/host/workspace".to_owned(),
        container_path: "/workspace".to_owned(),
    }
}

fn create(overrides: Value, network: EffectiveNetwork) -> Result<Vec<String>> {
    container_create_argv(&ContainerCreateOptions::new(
        toolbox_of(overrides),
        network,
        mount(),
        "ghost-sbx-abc",
    ))
}

fn argv(overrides: Value) -> Vec<String> {
    create(overrides, none()).unwrap()
}

fn has(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|item| item == flag)
}

#[test]
fn drops_all_capabilities_and_blocks_privilege_escalation() {
    let argv = argv(json!({}));
    assert!(has(&argv, "--cap-drop=ALL"));
    assert!(has(&argv, "--security-opt=no-new-privileges"));
    assert!(has(&argv, "--read-only"));
}

#[test]
fn drops_all_even_when_the_toolbox_forgot_to_say_so() {
    assert!(has(
        &argv(json!({"caps": {"drop": [], "add": []}})),
        "--cap-drop=ALL"
    ));
    let explicit = argv(json!({"caps": {"drop": ["all"], "add": []}}));
    assert_eq!(
        explicit.iter().filter(|f| *f == "--cap-drop=ALL").count(),
        1
    );
}

#[test]
fn passes_init_without_which_signals_never_reach_the_command() {
    assert!(has(&argv(json!({})), "--init"));
}

#[test]
fn adds_back_only_the_capabilities_the_toolbox_names() {
    let argv = argv(json!({"caps": {"drop": ["ALL"], "add": ["NET_RAW"]}}));
    assert!(has(&argv, "--cap-add=NET_RAW"));
    assert_eq!(
        argv.iter().filter(|f| f.starts_with("--cap-add=")).count(),
        1
    );
}

#[test]
fn applies_every_resource_limit() {
    let flags =
        argv(json!({"limits": {"memoryMb": 4096, "cpus": 2, "pidsMax": 512, "shmSizeMb": 1024}}));
    assert!(has(&flags, "--memory=4096m"));
    assert!(has(&flags, "--cpus=2"));
    assert!(has(&flags, "--pids-limit=512"));
    assert!(has(&flags, "--shm-size=1024m"));
    assert!(has(&argv(json!({"limits": {"cpus": 1.5}})), "--cpus=1.5"));
}

#[test]
fn omits_a_limit_set_to_zero_rather_than_passing_an_unlimited_flag() {
    let argv = argv(json!({"limits": {"memoryMb": 0, "cpus": 0, "pidsMax": 0, "shmSizeMb": 0}}));
    assert!(!argv.iter().any(|f| f.starts_with("--memory")));
    assert!(!argv.iter().any(|f| f.starts_with("--pids-limit")));
    assert!(!argv.iter().any(|f| f.starts_with("--cpus")));
    assert!(!argv.iter().any(|f| f.starts_with("--shm-size")));
}

#[test]
fn names_a_runtime_only_when_it_is_not_the_default() {
    assert!(!argv(json!({})).iter().any(|f| f.starts_with("--runtime")));
    assert!(has(&argv(json!({"runtime": "runsc"})), "--runtime=runsc"));
    assert!(has(&argv(json!({"runtime": "kata"})), "--runtime=kata"));
}

#[test]
fn passes_unconfined_seccomp_only_when_the_toolbox_asks_for_it() {
    assert!(!has(&argv(json!({})), "--security-opt=seccomp=unconfined"));
    assert!(has(
        &argv(json!({"security": {"seccomp": "unconfined"}})),
        "--security-opt=seccomp=unconfined"
    ));
}

#[test]
fn mounts_the_workspace_at_the_toolbox_workdir() {
    let argv = argv(json!({}));
    assert!(has(&argv, "--mount"));
    assert!(has(&argv, "type=bind,src=/host/workspace,dst=/workspace"));
    assert!(has(&argv, "--workdir"));
    let tail: Vec<&str> = argv
        .iter()
        .rev()
        .take(5)
        .rev()
        .map(String::as_str)
        .collect();
    assert_eq!(tail[0], "--entrypoint");
    assert_eq!(tail[1], "/bin/sh");
    assert_eq!(tail[3], "-c");
    assert_eq!(tail[4], "exec tail -f /dev/null");
}

#[test]
fn survives_a_colon_in_the_workspace_path() {
    let argv = container_create_argv(&ContainerCreateOptions::new(
        toolbox_of(json!({})),
        none(),
        ToolboxMount {
            host_path: "/Users/me/Notes:2024/ws".to_owned(),
            container_path: "/workspace".to_owned(),
        },
        "c",
    ))
    .unwrap();
    assert!(has(
        &argv,
        "type=bind,src=/Users/me/Notes:2024/ws,dst=/workspace"
    ));
}

#[test]
fn mounts_the_approved_manifest_read_only_outside_the_workspace() {
    let mut options = ContainerCreateOptions::new(toolbox_of(json!({})), none(), mount(), "c");
    options.manifest_path = Some("/home/ghost/profiles/kali/profile.json".to_owned());
    let argv = container_create_argv(&options).unwrap();
    assert!(has(
        &argv,
        "type=bind,src=/home/ghost/profiles/kali,dst=/run/ghost,ro"
    ));
    assert!(!argv.join(" ").contains("/workspace/.ghost/profile.json"));
}

#[test]
fn carries_labels_and_tmpfs_and_devices() {
    let mut options = ContainerCreateOptions::new(
        toolbox_of(json!({"security": {"tmpfs": ["/tmp:rw,size=64m"], "devices": ["/dev/fuse"]}})),
        none(),
        mount(),
        "c",
    );
    options
        .labels
        .insert("ghostai.session".to_owned(), "s1".to_owned());
    let argv = container_create_argv(&options).unwrap();
    assert!(has(&argv, "--label"));
    assert!(has(&argv, "ghostai.session=s1"));
    assert!(has(&argv, "--tmpfs=/tmp:rw,size=64m"));
    assert!(has(&argv, "--device=/dev/fuse"));
    assert!(format!("{options:?}").contains("ContainerCreateOptions"));
}

#[test]
fn runs_the_container_as_the_toolbox_user() {
    assert!(has(&argv(json!({"user": "1000:1000"})), "--user=1000:1000"));
    assert!(!argv(json!({})).iter().any(|f| f.starts_with("--user")));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// The assertion that matters most is about what can never appear. A
    /// toolbox is operator-installed, but it is still data, and this is the
    /// line between "a container policy" and "host root".
    #[test]
    fn never_grants_privilege_or_the_daemon_socket_for_any_toolbox(
        drop in prop::collection::vec("[A-Za-z_]{0,12}", 0..4),
        add in prop::collection::vec("[A-Za-z_]{0,12}", 0..4),
        user in "[a-z0-9:]{0,12}",
        workdir in "/[a-z]{1,8}",
        seccomp in prop::sample::select(vec!["default", "unconfined"]),
        tmpfs in prop::collection::vec("/[a-z]{1,6}", 0..3),
        devices in prop::collection::vec("/dev/[a-z]{1,6}", 0..3),
    ) {
        let argv = argv(json!({
            "caps": {"drop": drop, "add": add},
            "user": user,
            "workdir": workdir,
            "security": {"seccomp": seccomp, "tmpfs": tmpfs, "devices": devices},
        }));
        prop_assert!(!has(&argv, "--privileged"));
        prop_assert!(!argv.join(" ").contains("docker.sock"));
        prop_assert!(has(&argv, "--cap-drop=ALL"));
    }
}

#[test]
fn isolates_the_network_entirely_when_the_mode_is_none() {
    assert!(has(&argv(json!({})), "--network=none"));
}

#[test]
fn joins_the_gateway_namespace_when_one_is_supplied() {
    let mut options = ContainerCreateOptions::new(
        toolbox_of(json!({})),
        network(ToolboxNetworkMode::Allowlist),
        mount(),
        "c",
    );
    options.gateway_container = Some("ghost-netgate-1".to_owned());
    let argv = container_create_argv(&options).unwrap();
    assert!(has(&argv, "--network=container:ghost-netgate-1"));
}

#[test]
fn refuses_a_scoped_sandbox_with_no_gateway_rather_than_running_it_wide_open() {
    let error = create(json!({}), network(ToolboxNetworkMode::Allowlist))
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.contains("open egress"));
}

#[test]
fn uses_the_bridge_for_an_open_profile_with_no_gateway() {
    assert!(has(
        &create(json!({}), network(ToolboxNetworkMode::Open)).unwrap(),
        "--network=bridge"
    ));
}

fn exec_argv(plan: &ExecPlan, toolbox: &Toolbox) -> Vec<String> {
    container_exec_argv(&ContainerExecOptions {
        plan,
        toolbox,
        container_name: "c",
        run_id: "r1",
    })
}

fn script_of(argv: &[String]) -> String {
    let at = argv.iter().position(|item| item == "-c").unwrap();
    argv[at + 1].clone()
}

#[test]
fn exec_passes_the_command_as_positional_parameters_never_inside_the_script() {
    let result = exec_argv(&plan_of(), &toolbox_of(json!({})));
    let script = script_of(&result);
    assert!(!script.contains("nmap"));
    assert!(!script.contains("10.0.0.5"));
    assert_eq!(&result[result.len() - 3..], ["nmap", "-sV", "10.0.0.5"]);
}

#[test]
fn exec_is_unaffected_by_shell_metacharacters_in_the_arguments() {
    let mut plan = plan_of();
    plan.args = ["$(whoami)", "`id`", "; rm -rf /", "&& curl evil"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let result = exec_argv(&plan, &toolbox_of(json!({})));
    assert_eq!(script_of(&result), r#"echo $$ > "$1"; shift; exec "$@""#);
    assert!(has(&result, "$(whoami)"));
    assert!(has(&result, "; rm -rf /"));
}

#[test]
fn exec_runs_in_the_toolbox_workdir() {
    let result = exec_argv(&plan_of(), &toolbox_of(json!({})));
    let at = result.iter().position(|item| item == "--workdir").unwrap();
    assert_eq!(result[at + 1], "/workspace");
}

#[test]
fn exec_passes_through_only_the_environment_names_the_profile_lists() {
    let result = exec_argv(&plan_of(), &toolbox_of(json!({"env": ["LANG"]})));
    assert!(has(&result, "LANG=en_GB.UTF-8"));
    let joined = result.join(" ");
    assert!(!joined.contains("SECRET"));
    assert!(!joined.contains("PATH=/host/bin"));
}

#[test]
fn exec_omits_a_name_the_profile_lists_but_the_plan_does_not_carry() {
    let result = exec_argv(&plan_of(), &toolbox_of(json!({"env": ["TZ"]})));
    assert!(!result.join(" ").contains("TZ="));
}

#[test]
fn exec_writes_the_pid_to_a_tmpfs_not_the_read_only_transcript_mount() {
    let result = exec_argv(&plan_of(), &toolbox_of(json!({})));
    assert!(has(&result, "/tmp/.ghost-r1.pid"));
    assert!(!result.join(" ").contains("/workspace/.ghost"));
}

#[test]
fn reports_the_transcript_at_its_read_only_mount_outside_the_workspace() {
    assert_eq!(
        container_run_dir("ghost-sbx-1", "x"),
        "/run/ghost-runs/ghost-sbx-1/x"
    );
}

#[test]
fn keeps_the_transcript_mount_a_sibling_of_the_toolbox_mount_never_nested() {
    assert!(!RUNS_MOUNT_DIR.starts_with(&format!("{TOOLBOX_MOUNT_DIR}/")));
}

#[test]
fn mounts_the_transcript_directory_read_only() {
    let mut options = ContainerCreateOptions::new(toolbox_of(json!({})), none(), mount(), "c");
    options.runs_path = Some("/home/ghost/runs".to_owned());
    let argv = container_create_argv(&options).unwrap();
    assert!(has(
        &argv,
        "type=bind,src=/home/ghost/runs,dst=/run/ghost-runs,ro"
    ));
}

#[test]
fn kill_signals_the_recorded_pid_inside_the_container() {
    let argv = container_kill_argv("c", "r1", KillSignal::Term);
    assert!(has(&argv, "/tmp/.ghost-r1.pid"));
    assert!(has(&argv, "TERM"));
    assert_eq!(argv[0], "exec");
    assert_eq!(KillSignal::Kill.as_str(), "KILL");
}

#[test]
fn kill_takes_the_pid_file_as_a_parameter_rather_than_interpolating_it() {
    let argv = container_kill_argv("c", "r1", KillSignal::Kill);
    assert!(!script_of(&argv).contains("r1"));
}

#[test]
fn kill_refuses_to_signal_anything_that_is_not_a_bare_positive_integer() {
    let argv = container_kill_argv("c", "r1", KillSignal::Term);
    assert!(script_of(&argv).contains("*[!0-9]*"));
}

/// Records what it was asked to run and answers without a process.
struct FakeInner {
    calls: Mutex<Vec<RunRequest>>,
    outcome: RunOutcome,
    fail_first: bool,
}

impl FakeInner {
    fn new(outcome: RunOutcome) -> Arc<FakeInner> {
        Arc::new(FakeInner {
            calls: Mutex::new(Vec::new()),
            outcome,
            fail_first: false,
        })
    }

    fn ok() -> RunOutcome {
        RunOutcome {
            stdout: "scan line one\n".to_owned(),
            stderr: "a warning\n".to_owned(),
            code: Some(0),
            ..RunOutcome::default()
        }
    }
}

impl CommandRunner for FakeInner {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        Box::pin(async move {
            if let Some(tee) = &request.tee {
                tee.write(OutputStream::Stdout, b"scan line one\n");
                tee.write(OutputStream::Stderr, b"a warning\n");
            }
            let first = self.calls.lock().is_empty();
            self.calls.lock().push(request);
            if self.fail_first && first {
                return Err(GhostError::new(ErrorKind::Tool, "aborted"));
            }
            Ok(self.outcome.clone())
        })
    }
}

fn runner(inner: &Arc<FakeInner>, runs_root: PathBuf, bin: Option<&str>) -> ContainerRunner {
    ContainerRunner::new(ContainerRunnerOptions {
        toolbox: toolbox_of(json!({})),
        container_name: "c".to_owned(),
        runs_root,
        bin: bin.map(str::to_owned),
        next_run_id: Arc::new(|| "run-1".to_owned()),
        inner: Some(Arc::clone(inner) as Arc<dyn CommandRunner>),
    })
}

fn req(plan: ExecPlan, timeout_ms: u64) -> RunRequest {
    RunRequest {
        plan,
        timeout_ms,
        token: CancellationToken::new(),
        clock: Arc::new(SystemClock),
        tee: None,
    }
}

async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn writes_the_full_transcript_and_reports_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let inner = FakeInner::new(FakeInner::ok());
    let outcome = runner(&inner, dir.path().to_path_buf(), None)
        .run(req(plan_of(), 1_000))
        .await
        .unwrap();
    assert_eq!(
        outcome.transcript_dir.as_deref(),
        Some("/run/ghost-runs/c/run-1")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("c/run-1/stdout.log")).unwrap(),
        "scan line one\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("c/run-1/stderr.log")).unwrap(),
        "a warning\n"
    );
}

#[tokio::test]
async fn runs_the_daemon_client_rather_than_the_guarded_program() {
    let dir = tempfile::tempdir().unwrap();
    let inner = FakeInner::new(FakeInner::ok());
    let runner = runner(&inner, dir.path().to_path_buf(), Some("podman"));
    assert!(format!("{runner:?}").contains("podman"));
    runner.run(req(plan_of(), 0)).await.unwrap();
    let calls = inner.calls.lock();
    assert_eq!(calls[0].plan.file, "podman");
    assert_eq!(calls[0].plan.args[0], "exec");
    assert_eq!(calls[0].plan.max_output_bytes, 1024);
    assert!(calls[0].tee.is_some());
}

#[tokio::test]
async fn does_not_forward_the_host_environment_to_the_client_beyond_path() {
    let dir = tempfile::tempdir().unwrap();
    let inner = FakeInner::new(FakeInner::ok());
    runner(&inner, dir.path().to_path_buf(), None)
        .run(req(plan_of(), 0))
        .await
        .unwrap();
    let calls = inner.calls.lock();
    let keys: Vec<&String> = calls[0].plan.env.keys().collect();
    assert_eq!(keys, vec!["PATH"]);
}

#[tokio::test]
async fn signals_the_container_when_the_command_timed_out() {
    let dir = tempfile::tempdir().unwrap();
    let inner = FakeInner::new(RunOutcome {
        timed_out: true,
        ..FakeInner::ok()
    });
    runner(&inner, dir.path().to_path_buf(), None)
        .run(req(plan_of(), 5))
        .await
        .unwrap();
    settle().await;
    let calls = inner.calls.lock();
    assert!(
        calls
            .iter()
            .any(|call| call.plan.args.iter().any(|a| a == "KILL"))
    );
}

#[tokio::test]
async fn signals_the_container_when_the_run_failed() {
    let dir = tempfile::tempdir().unwrap();
    let inner = Arc::new(FakeInner {
        calls: Mutex::new(Vec::new()),
        outcome: FakeInner::ok(),
        fail_first: true,
    });
    let error = runner(&inner, dir.path().to_path_buf(), None)
        .run(req(plan_of(), 0))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Tool);
    settle().await;
    let calls = inner.calls.lock();
    assert!(
        calls
            .iter()
            .any(|call| call.plan.args.iter().any(|a| a == "TERM"))
    );
}

#[test]
fn a_transcript_is_created_on_the_host_before_the_container_writes() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = Transcript::open(dir.path(), "ghost-sbx-1", "r9").unwrap();
    transcript.write(OutputStream::Stdout, b"hello\n");
    transcript.close();
    assert_eq!(
        transcript.host_dir(),
        dir.path().join("ghost-sbx-1").join("r9")
    );
    assert_eq!(transcript.container_dir(), "/run/ghost-runs/ghost-sbx-1/r9");
    assert_eq!(
        std::fs::read_to_string(transcript.host_dir().join("stdout.log")).unwrap(),
        "hello\n"
    );
    assert!(format!("{transcript:?}").contains("r9"));
}

#[test]
fn a_transcript_survives_a_workspace_that_disappears_mid_run() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = Transcript::open(dir.path(), "c", "gone").unwrap();
    drop(dir);
    transcript.write(OutputStream::Stdout, b"into the void\n");
    transcript.write(OutputStream::Stderr, b"and more\n");
    transcript.close();
    transcript.close();
}

#[test]
fn a_transcript_root_that_cannot_be_created_is_a_storage_error() {
    let error = Transcript::open(std::path::Path::new("/dev/null/nope"), "c", "r")
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Storage);
}

fn gone(stderr: &str, code: Option<i32>) -> bool {
    container_is_gone(&RunOutcome {
        stderr: stderr.to_owned(),
        code,
        ..RunOutcome::default()
    })
}

#[test]
fn recognises_what_each_engine_says_when_the_container_is_not_there() {
    assert!(gone(
        "Error response from daemon: No such container: ghost-sbx-1\n",
        Some(1)
    ));
    assert!(gone(
        "Error response from daemon: Container ghost-sbx-1 is not running\n",
        Some(1)
    ));
    assert!(gone("Error: No such container: ghost-sbx-1\n", Some(1)));
    assert!(gone(
        "Error: no container with name or ID \"ghost-sbx-1\" found\n",
        Some(125)
    ));
    assert!(gone(
        "Error: can only create exec sessions on running containers\n",
        Some(125)
    ));
}

#[test]
fn leaves_a_command_that_merely_printed_one_of_those_strings_alone() {
    assert!(!gone(
        "grep: No such container: not found in any file\n",
        Some(1)
    ));
    assert!(!gone(
        "a warning\nError response from daemon: No such container: x\n",
        Some(1)
    ));
}

#[test]
fn says_nothing_about_a_command_that_succeeded_or_was_killed() {
    assert!(!gone(
        "Error response from daemon: No such container: x\n",
        Some(0)
    ));
    assert!(!container_is_gone(&RunOutcome {
        code: None,
        signal: Some("SIGKILL".to_owned()),
        timed_out: true,
        ..RunOutcome::default()
    }));
}
