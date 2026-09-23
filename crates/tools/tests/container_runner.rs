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

use darkwire_core::{ErrorKind, Result, SystemClock, WireError};
use darkwire_protocol::environment::EnvironmentDefinition;
use darkwire_protocol::{EnvironmentNetwork, NetworkMode};
use darkwire_security::ExecPlan;
use darkwire_tools::{
    BoxFuture, CommandRunner, ContainerCreateOptions, ContainerExecOptions, ContainerRunner,
    ContainerRunnerOptions, KillSignal, OutputStream, OutputTee, RUNS_MOUNT_DIR, RunOutcome,
    RunRequest, TRANSCRIPT_MAX_BYTES, Transcript, WorkspaceMount, container_create_argv,
    container_exec_argv, container_exec_env, container_is_gone, container_kill_argv,
    container_run_dir,
};
use indexmap::IndexMap;
use parking_lot::Mutex;
use proptest::prelude::*;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const DIGEST: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn container_of(overrides: Value) -> EnvironmentDefinition {
    let mut base = json!({
        "schema": "darkwire.environment/1",
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

fn none() -> EnvironmentNetwork {
    EnvironmentNetwork::default()
}

fn network(mode: NetworkMode) -> EnvironmentNetwork {
    EnvironmentNetwork {
        mode,
        allow: vec!["10.0.0.0/8".to_owned()],
    }
}

fn mount() -> WorkspaceMount {
    WorkspaceMount {
        host_path: "/host/workspace".to_owned(),
        environment_path: "/workspace".to_owned(),
    }
}

fn create(overrides: Value, network: EnvironmentNetwork) -> Result<Vec<String>> {
    container_create_argv(&ContainerCreateOptions::new(
        container_of(overrides),
        network,
        mount(),
        "dw-sbx-abc",
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
fn drops_all_even_when_the_definition_forgot_to_say_so() {
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
fn adds_back_only_the_capabilities_the_definition_names() {
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
fn omits_the_hardening_a_definition_explicitly_turned_off() {
    let argv = argv(json!({"security": {"noNewPrivileges": false, "readOnlyRoot": false}}));
    assert!(!has(&argv, "--security-opt=no-new-privileges"));
    assert!(!has(&argv, "--read-only"));
    // The floor that is not the definition's to lower stays where it is.
    assert!(has(&argv, "--cap-drop=ALL"));
}

#[test]
fn passes_unconfined_seccomp_only_when_the_definition_asks_for_it() {
    assert!(!has(&argv(json!({})), "--security-opt=seccomp=unconfined"));
    assert!(has(
        &argv(json!({"security": {"seccomp": "unconfined"}})),
        "--security-opt=seccomp=unconfined"
    ));
}

#[test]
fn mounts_the_workspace_at_the_container_workdir() {
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
        container_of(json!({})),
        none(),
        WorkspaceMount {
            host_path: "/Users/me/Notes:2024/ws".to_owned(),
            environment_path: "/workspace".to_owned(),
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
fn carries_labels_and_tmpfs_and_devices() {
    let mut options = ContainerCreateOptions::new(
        container_of(
            json!({"security": {"tmpfs": ["/tmp:rw,size=64m"], "devices": ["/dev/fuse"]}}),
        ),
        none(),
        mount(),
        "c",
    );
    options
        .labels
        .insert("darkwire.session".to_owned(), "s1".to_owned());
    let argv = container_create_argv(&options).unwrap();
    assert!(has(&argv, "--label"));
    assert!(has(&argv, "darkwire.session=s1"));
    assert!(has(&argv, "--tmpfs=/tmp:rw,size=64m"));
    assert!(has(&argv, "--device=/dev/fuse"));
    assert!(format!("{options:?}").contains("ContainerCreateOptions"));
}

#[test]
fn runs_the_container_as_the_definitions_user() {
    assert!(has(&argv(json!({"user": "1000:1000"})), "--user=1000:1000"));
    assert!(
        !argv(json!({"user": ""}))
            .iter()
            .any(|f| f.starts_with("--user"))
    );
}

#[test]
fn masks_the_host_identifying_corners_of_sysfs_when_the_engine_honours_it() {
    let mut options = ContainerCreateOptions::new(container_of(json!({})), none(), mount(), "c");
    options.mask_sysfs = true;
    let argv = container_create_argv(&options).unwrap();
    for path in ["/sys/firmware", "/sys/class/block"] {
        assert!(
            has(
                &argv,
                &format!("--tmpfs={path}:ro,nosuid,nodev,noexec,size=4k")
            ),
            "{path} is not masked"
        );
    }
    // Every masked path must exist on every architecture: a `--tmpfs` over one
    // that does not makes runc try to create the mountpoint inside a read-only
    // `/sys`, and the container never starts. The DMI directories are x86-only
    // and broke every sandboxed turn on arm64 while they were in the list.
    for x86_only in ["/sys/class/dmi", "/sys/devices/virtual/dmi"] {
        assert!(
            !argv.iter().any(|flag| flag.contains(x86_only)),
            "{x86_only} does not exist on every architecture and must not be masked"
        );
    }
}

#[test]
fn leaves_sysfs_alone_for_an_engine_that_would_have_to_copy_it_up() {
    // Podman's copy-up needs a capability the container does not hold, so the
    // masks are simply not requested there.
    let argv = argv(json!({}));
    assert!(!argv.iter().any(|flag| flag.contains("/sys/")));
    assert!(!argv.iter().any(|flag| flag.contains("/sys/firmware")));
}

#[test]
fn masking_sysfs_leaves_the_definitions_own_tmpfs_specs_intact() {
    let mut options = ContainerCreateOptions::new(
        container_of(json!({"security": {"tmpfs": ["/tmp:rw,size=64m"]}})),
        none(),
        mount(),
        "c",
    );
    options.mask_sysfs = true;
    let argv = container_create_argv(&options).unwrap();
    assert!(has(&argv, "--tmpfs=/tmp:rw,size=64m"));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// The assertion that matters most is about what can never appear. A
    /// container definition is operator-installed, but it is still data, and
    /// this is the line between "a container policy" and "host root".
    #[test]
    fn never_grants_privilege_or_the_daemon_socket_for_any_definition(
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
        container_of(json!({})),
        network(NetworkMode::Allowlist),
        mount(),
        "c",
    );
    options.gateway_container = Some("ghost-netgate-1".to_owned());
    let argv = container_create_argv(&options).unwrap();
    assert!(has(&argv, "--network=container:ghost-netgate-1"));
}

#[test]
fn refuses_a_scoped_sandbox_with_no_gateway_rather_than_running_it_wide_open() {
    let error = create(json!({}), network(NetworkMode::Allowlist))
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.contains("open egress"));
}

#[test]
fn uses_the_bridge_for_an_open_request_with_no_gateway() {
    assert!(has(
        &create(json!({}), network(NetworkMode::Open)).unwrap(),
        "--network=bridge"
    ));
}

/// Every allow-list, not only one naming hosts. The proxy is the only thing
/// that resolves a name, so a container pointed nowhere would find every name
/// unreachable even though its list names one.
#[test]
fn points_ordinary_clients_at_the_proxy_for_every_allow_list() {
    for allow in [
        vec!["10.0.0.0/8".to_owned()],
        vec!["example.test".to_owned()],
        vec!["10.0.0.0/8".to_owned(), ".example.test".to_owned()],
    ] {
        let asked = EnvironmentNetwork {
            mode: NetworkMode::Allowlist,
            allow: allow.clone(),
        };
        let mut options = ContainerCreateOptions::new(container_of(json!({})), asked, mount(), "c");
        options.gateway_container = Some("ghost-netgate-1".to_owned());
        let argv = container_create_argv(&options).unwrap();
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            assert!(
                has(&argv, &format!("{key}=http://127.0.0.1:3128")),
                "{allow:?}"
            );
        }
    }
}

/// Keyed on the mode, not on the list being non-empty. The validator that
/// refuses entries under `none` and `open` runs elsewhere, and this must not
/// depend on it having run.
#[test]
fn sets_no_proxy_variables_outside_an_allow_list() {
    assert!(!argv(json!({})).iter().any(|flag| flag.contains("PROXY")));
    for mode in [NetworkMode::None, NetworkMode::Open] {
        let asked = EnvironmentNetwork {
            mode,
            allow: vec!["example.test".to_owned()],
        };
        let created = create(json!({}), asked);
        if let Ok(argv) = created {
            assert!(
                !argv
                    .iter()
                    .any(|flag| flag.to_lowercase().contains("proxy")),
                "{mode:?}"
            );
        }
    }
}

fn exec_argv(plan: &ExecPlan, container: &EnvironmentDefinition) -> Vec<String> {
    container_exec_argv(&ContainerExecOptions {
        plan,
        container,
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
    let result = exec_argv(&plan_of(), &container_of(json!({})));
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
    let result = exec_argv(&plan, &container_of(json!({})));
    assert_eq!(script_of(&result), r#"echo $$ > "$1"; shift; exec "$@""#);
    assert!(has(&result, "$(whoami)"));
    assert!(has(&result, "; rm -rf /"));
}

#[test]
fn exec_leads_its_own_process_group_so_a_timeout_reaches_the_whole_tree() {
    let result = exec_argv(&plan_of(), &container_of(json!({})));
    let at = result.iter().position(|item| item == "setsid").unwrap();
    assert_eq!(result[at + 1], "/bin/sh");
    assert_eq!(result[at - 1], "c");
}

#[test]
fn exec_runs_in_the_container_workdir() {
    let result = exec_argv(&plan_of(), &container_of(json!({})));
    let at = result.iter().position(|item| item == "--workdir").unwrap();
    assert_eq!(result[at + 1], "/workspace");
}

#[test]
fn exec_passes_through_only_the_environment_names_the_definition_lists() {
    let result = exec_argv(&plan_of(), &container_of(json!({"env": ["LANG"]})));
    let at = result.iter().position(|item| item == "--env").unwrap();
    assert_eq!(result[at + 1], "LANG");
    let joined = result.join(" ");
    assert!(!joined.contains("SECRET"));
    assert!(!joined.contains("PATH=/host/bin"));
}

/// A value on the client's command line is readable by anyone running `ps`.
#[test]
fn exec_never_writes_a_passed_through_value_on_the_command_line() {
    let mut plan = plan_of();
    plan.env
        .insert("API_TOKEN".to_owned(), "hunter2".to_owned());
    let container = container_of(json!({"env": ["API_TOKEN", "TZ"]}));
    let result = exec_argv(&plan, &container);
    assert!(!result.join(" ").contains("hunter2"));
    assert!(has(&result, "API_TOKEN"));
    let env = container_exec_env(&ContainerExecOptions {
        plan: &plan,
        container: &container,
        container_name: "c",
        run_id: "r1",
    });
    assert_eq!(env.get("API_TOKEN").map(String::as_str), Some("hunter2"));
    assert!(!env.contains_key("TZ"));
}

#[test]
fn exec_keeps_a_value_the_client_reads_for_itself_on_the_command_line() {
    let result = exec_argv(&plan_of(), &container_of(json!({"env": ["PATH"]})));
    assert!(has(&result, "PATH=/host/bin"));
}

#[test]
fn exec_omits_a_name_the_definition_lists_but_the_plan_does_not_carry() {
    let result = exec_argv(&plan_of(), &container_of(json!({"env": ["TZ"]})));
    assert!(!result.join(" ").contains("TZ="));
}

#[test]
fn exec_writes_the_pid_to_a_tmpfs_not_the_read_only_transcript_mount() {
    let result = exec_argv(&plan_of(), &container_of(json!({})));
    assert!(has(&result, "/tmp/.darkwire-r1.pid"));
    assert!(!result.join(" ").contains("/workspace/.darkwire"));
}

#[test]
fn reports_the_transcript_at_its_read_only_mount_outside_the_workspace() {
    assert_eq!(
        container_run_dir("dw-sbx-1", "x"),
        "/run/darkwire-runs/dw-sbx-1/x"
    );
}

#[test]
fn keeps_the_transcript_mount_outside_every_other_mount_it_establishes() {
    let mut options = ContainerCreateOptions::new(container_of(json!({})), none(), mount(), "c");
    options.runs_path = Some("/home/ghost/runs".to_owned());
    let argv = container_create_argv(&options).unwrap();
    let targets: Vec<&str> = argv
        .iter()
        .filter_map(|flag| {
            flag.split(',')
                .find_map(|part| part.strip_prefix("dst="))
                .filter(|target| !target.starts_with(RUNS_MOUNT_DIR))
        })
        .collect();
    assert_eq!(targets, ["/workspace"]);
    assert!(
        !targets
            .iter()
            .any(|target| RUNS_MOUNT_DIR.starts_with(&format!("{target}/")))
    );
}

#[test]
fn mounts_only_this_containers_own_transcripts_read_only() {
    let mut options = ContainerCreateOptions::new(container_of(json!({})), none(), mount(), "c");
    options.runs_path = Some("/home/ghost/runs".to_owned());
    let argv = container_create_argv(&options).unwrap();
    assert!(has(
        &argv,
        "type=bind,src=/home/ghost/runs/c,dst=/run/darkwire-runs/c,ro"
    ));
}

#[test]
fn takes_the_transcript_path_with_an_empty_tmpfs_when_there_is_nothing_to_mount() {
    let argv = argv(json!({}));
    assert!(has(
        &argv,
        "--tmpfs=/run/darkwire-runs:ro,nosuid,nodev,noexec,size=4k"
    ));
    assert!(
        !argv
            .iter()
            .any(|flag| flag.contains("dst=/run/darkwire-runs"))
    );
    // The `--mount` the bind would have taken is not left dangling in front of
    // the flag that replaced it.
    let at = argv
        .iter()
        .position(|flag| flag.starts_with("--tmpfs=/run/darkwire-runs"))
        .unwrap();
    assert_ne!(argv[at - 1], "--mount");
}

#[test]
fn kill_signals_the_recorded_pid_inside_the_container() {
    let argv = container_kill_argv("c", "r1", KillSignal::Term);
    assert!(has(&argv, "/tmp/.darkwire-r1.pid"));
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
    let script = script_of(&container_kill_argv("c", "r1", KillSignal::Term));
    assert!(script.contains("*[!0-9]*"));
    assert!(script.contains("| 0 | 1 )"));
}

#[test]
fn kill_signals_the_group_rather_than_the_leader_alone() {
    let script = script_of(&container_kill_argv("c", "r1", KillSignal::Term));
    assert!(script.contains(r#"kill -"$2" -- -"$p""#));
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
                return Err(WireError::new(ErrorKind::Tool, "aborted"));
            }
            Ok(self.outcome.clone())
        })
    }
}

fn runner(inner: &Arc<FakeInner>, runs_root: PathBuf, bin: Option<&str>) -> ContainerRunner {
    ContainerRunner::new(runner_options(inner, runs_root, bin))
}

fn runner_options(
    inner: &Arc<FakeInner>,
    runs_root: PathBuf,
    bin: Option<&str>,
) -> ContainerRunnerOptions {
    ContainerRunnerOptions {
        container: container_of(json!({})),
        container_name: "c".to_owned(),
        runs_root,
        bin: bin.map(str::to_owned),
        next_run_id: Arc::new(|| "run-1".to_owned()),
        inner: Some(Arc::clone(inner) as Arc<dyn CommandRunner>),
    }
}

#[test]
fn options_describe_themselves_without_naming_the_run_id_source() {
    let inner = FakeInner::new(FakeInner::ok());
    let options = runner_options(&inner, PathBuf::from("/host/runs"), Some("podman"));
    let text = format!("{options:?}");
    assert!(text.contains("ContainerRunnerOptions"));
    assert!(text.contains("kali"));
    assert!(text.contains("podman"));
    assert!(!text.contains("next_run_id"));
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
        Some("/run/darkwire-runs/c/run-1")
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

/// Records everything written to it, standing in for the caller's own progress
/// stream.
#[derive(Default)]
struct Recorder(Mutex<Vec<u8>>);

impl OutputTee for Recorder {
    fn write(&self, _stream: OutputStream, chunk: &[u8]) {
        self.0.lock().extend_from_slice(chunk);
    }
}

#[tokio::test]
async fn keeps_feeding_the_callers_progress_stream_while_it_writes_the_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let inner = FakeInner::new(FakeInner::ok());
    let progress = Arc::new(Recorder::default());
    let mut request = req(plan_of(), 1_000);
    request.tee = Some(Arc::clone(&progress) as Arc<dyn OutputTee>);
    runner(&inner, dir.path().to_path_buf(), None)
        .run(request)
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8(progress.0.lock().clone()).unwrap(),
        "scan line one\na warning\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("c/run-1/stdout.log")).unwrap(),
        "scan line one\n"
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
async fn hands_passed_through_values_to_the_client_in_its_environment() {
    let dir = tempfile::tempdir().unwrap();
    let inner = FakeInner::new(FakeInner::ok());
    let mut options = runner_options(&inner, dir.path().to_path_buf(), None);
    options.container = container_of(json!({"env": ["LANG"]}));
    ContainerRunner::new(options)
        .run(req(plan_of(), 0))
        .await
        .unwrap();
    let calls = inner.calls.lock();
    assert_eq!(
        calls[0].plan.env.get("LANG").map(String::as_str),
        Some("en_GB.UTF-8")
    );
    assert!(!calls[0].plan.args.join(" ").contains("en_GB.UTF-8"));
}

#[test]
fn a_transcript_keeps_its_cap_and_says_where_it_was_cut() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = Transcript::open(dir.path(), "c", "r1").unwrap();
    let chunk = vec![b'x'; 1024 * 1024];
    for _ in 0..6 {
        transcript.write(OutputStream::Stdout, &chunk);
    }
    transcript.write(OutputStream::Stderr, b"short\n");
    transcript.close();
    let stdout = std::fs::read(transcript.host_dir().join("stdout.log")).unwrap();
    let cap = usize::try_from(TRANSCRIPT_MAX_BYTES).unwrap();
    assert!(stdout[..cap].iter().all(|byte| *byte == b'x'));
    let trailer = String::from_utf8_lossy(&stdout[cap..]);
    assert!(trailer.contains("transcript cut"), "{trailer}");
    assert!(
        trailer.contains(&(2 * 1024 * 1024).to_string()),
        "{trailer}"
    );
    assert_eq!(
        std::fs::read_to_string(transcript.host_dir().join("stderr.log")).unwrap(),
        "short\n"
    );
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
    let calls = inner.calls.lock();
    assert!(
        calls
            .iter()
            .any(|call| call.plan.args.iter().any(|a| a == "KILL"))
    );
}

#[tokio::test(start_paused = true)]
async fn escalates_to_a_second_harder_signal_when_the_run_failed() {
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
    let calls = inner.calls.lock();
    let signals: Vec<&String> = calls
        .iter()
        .filter_map(|call| call.plan.args.last())
        .collect();
    assert_eq!(signals[1..], ["TERM", "KILL"]);
}

#[test]
fn a_transcript_is_created_on_the_host_before_the_container_writes() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = Transcript::open(dir.path(), "dw-sbx-1", "r9").unwrap();
    transcript.write(OutputStream::Stdout, b"hello\n");
    transcript.close();
    assert_eq!(
        transcript.host_dir(),
        dir.path().join("dw-sbx-1").join("r9")
    );
    assert_eq!(transcript.container_dir(), "/run/darkwire-runs/dw-sbx-1/r9");
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
        "Error response from daemon: No such container: dw-sbx-1\n",
        Some(1)
    ));
    assert!(gone(
        "Error response from daemon: Container dw-sbx-1 is not running\n",
        Some(1)
    ));
    assert!(gone("Error: No such container: dw-sbx-1\n", Some(1)));
    assert!(gone(
        "Error: no container with name or ID \"dw-sbx-1\" found\n",
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
