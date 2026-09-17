//! `exec` against real child processes, and the runner seam.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use darkwire_core::{ErrorKind, Result};
use darkwire_protocol::ToolSource;
use darkwire_security::ExecPlan;
use darkwire_tools::builtin::exec::effective_timeout;
use darkwire_tools::testkit::TestWorkspace;
use darkwire_tools::{
    BoxFuture, CommandRunner, RunOutcome, RunRequest, ToolContext, ToolExecution, ToolInvocation,
    ToolRegistry, exec_tool,
};
use parking_lot::Mutex;
use serde_json::{Value, json};

async fn run(args: Value, ctx: &ToolContext) -> ToolExecution {
    exec_tool().execute(args, ctx).await
}

async fn ok(args: Value, ctx: &ToolContext) -> ToolExecution {
    let execution = run(args, ctx).await;
    assert!(execution.kind.is_none(), "{}", execution.content);
    execution
}

#[tokio::test]
async fn runs_a_program_and_returns_its_output() {
    let ws = TestWorkspace::new();
    let result = ok(json!({"argv": ["printf", "hello"]}), ws.context()).await;
    assert!(!result.is_error);
    assert_eq!(result.content, "hello\n\nExit code: 0");
}

#[tokio::test]
async fn runs_in_the_workspace_root_so_relative_arguments_resolve() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("data.txt"), "contents").unwrap();
    let result = ok(json!({"argv": ["cat", "data.txt"]}), ws.context()).await;
    assert!(result.content.contains("contents"));
}

#[tokio::test]
async fn reports_a_non_zero_exit_as_a_result_not_a_thrown_failure() {
    // `grep` finding nothing exits 1, and a compiler failing is the answer the
    // model asked for — the output has to survive.
    let ws = TestWorkspace::new();
    let result = ok(json!({"argv": ["ls", "definitely-missing"]}), ws.context()).await;
    assert!(result.is_error);
    assert!(result.content.contains("[stderr]\n"));
    assert!(result.content.contains("Exit code: "));
    assert!(!result.content.contains("Exit code: 0"));
}

#[tokio::test]
async fn says_so_when_a_program_produced_nothing() {
    let ws = TestWorkspace::new();
    let result = ok(json!({"argv": ["true"]}), ws.context()).await;
    assert!(result.content.contains("(no output)"));
}

#[tokio::test]
async fn reports_both_streams() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("present.txt"), "x").unwrap();
    let result = ok(
        json!({"argv": ["ls", "present.txt", "missing.txt"]}),
        ws.context(),
    )
    .await;
    assert!(result.content.contains("present.txt"));
    assert!(result.content.contains("[stderr]\n"));
}

#[tokio::test]
async fn records_argv_and_the_validated_paths_for_the_audit_log() {
    let ws = TestWorkspace::new();
    fs::write(ws.root().join("data.txt"), "x").unwrap();
    let result = ok(json!({"argv": ["cat", "./data.txt"]}), ws.context()).await;
    assert_eq!(result.details.get("exitCode"), Some(&json!(0)));
    assert_eq!(
        result.details.get("paths"),
        Some(&json!([ws.root().join("data.txt").to_string_lossy()]))
    );
    assert_eq!(
        result.details.get("argv"),
        Some(&json!(["cat", "./data.txt"]))
    );
    assert_eq!(result.details.get("timedOut"), Some(&json!(false)));
    assert_eq!(result.details.get("signal"), Some(&Value::Null));
}

#[tokio::test]
async fn reports_a_program_that_does_not_exist() {
    let ws = TestWorkspace::new();
    let error = run(
        json!({"argv": ["darkwire-definitely-not-a-binary"]}),
        ws.context(),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::Tool));
    assert!(error.content.contains("Could not run"));
}

#[tokio::test]
async fn refuses_a_shell() {
    let ws = TestWorkspace::new();
    let error = run(json!({"argv": ["bash", "-c", "echo pwned"]}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::PermissionDenied));
    assert!(error.content.contains("shell"));
}

#[tokio::test]
async fn refuses_a_denied_binary() {
    let ws = TestWorkspace::new();
    let error = run(
        json!({"argv": ["curl", "https://example.com"]}),
        &ws.with(|config| config.exec.denied_binaries = vec!["curl".to_owned()]),
    )
    .await;
    assert_eq!(error.kind, Some(ErrorKind::PermissionDenied));
}

#[tokio::test]
async fn refuses_an_argument_reaching_outside_the_workspace() {
    let ws = TestWorkspace::new();
    let error = run(json!({"argv": ["cat", "../../etc/passwd"]}), ws.context()).await;
    assert_eq!(error.kind, Some(ErrorKind::JailEscape));
}

#[tokio::test]
async fn passes_only_the_allow_listed_environment_through() {
    let ws = TestWorkspace::new();
    let mut ctx = ws.context().clone();
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("PATH".to_owned(), std::env::var("PATH").unwrap_or_default());
    env.insert("HOME".to_owned(), "/home/x".to_owned());
    env.insert("DARKWIRE_API_KEY".to_owned(), "secret".to_owned());
    ctx.env = Arc::new(env);
    let result = ok(json!({"argv": ["env"]}), &ctx).await;
    assert!(result.content.contains("PATH="));
    assert!(result.content.contains("HOME=/home/x"));
    assert!(!result.content.contains("DARKWIRE_API_KEY"));
}

#[tokio::test]
async fn caps_output_while_the_child_writes_keeping_the_head_of_it() {
    let ws = TestWorkspace::new();
    let result = ok(
        json!({"argv": ["printf", "%0200000d", "0"]}),
        &ws.with(|config| config.exec.max_output_bytes = 64),
    )
    .await;
    assert!(result.content.contains("output truncated at 64 bytes"));
    assert!(result.content.contains(&"0".repeat(64)));
}

#[tokio::test]
async fn kills_a_program_that_outlives_its_timeout() {
    let ws = TestWorkspace::new();
    let result = ok(
        json!({"argv": ["sleep", "60"], "timeoutMs": 150}),
        ws.context(),
    )
    .await;
    assert!(result.is_error);
    assert!(result.content.contains("exceeding its time limit"));
    assert!(result.content.contains("Killed by SIG"));
}

#[tokio::test]
async fn lets_the_model_ask_for_less_time_than_the_operator_allows_never_more() {
    let ws = TestWorkspace::new();
    let started = Instant::now();
    let result = ok(
        json!({"argv": ["sleep", "60"], "timeoutMs": 5_000}),
        &ws.with(|config| config.exec.timeout_ms = 150),
    )
    .await;
    assert!(result.content.contains("exceeding its time limit"));
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[tokio::test]
async fn applies_the_operator_timeout_when_the_model_asks_for_none() {
    let ws = TestWorkspace::new();
    let result = ok(
        json!({"argv": ["sleep", "60"]}),
        &ws.with(|config| config.exec.timeout_ms = 150),
    )
    .await;
    assert!(result.content.contains("exceeding its time limit"));
}

#[test]
fn reconciles_the_two_timeouts() {
    let plan = |timeout_ms| ExecPlan {
        file: "x".to_owned(),
        args: Vec::new(),
        cwd: "/".into(),
        env: indexmap::IndexMap::new(),
        timeout_ms,
        max_output_bytes: 1,
        paths: Vec::new(),
    };
    assert_eq!(effective_timeout(&plan(0), None), 0);
    assert_eq!(effective_timeout(&plan(0), Some(0)), 0);
    assert_eq!(effective_timeout(&plan(0), Some(30_000)), 30_000);
    assert_eq!(effective_timeout(&plan(5_000), Some(0)), 5_000);
    assert_eq!(effective_timeout(&plan(5_000), Some(60_000)), 5_000);
    assert_eq!(effective_timeout(&plan(5_000), Some(150)), 150);
}

#[tokio::test]
async fn kills_the_child_when_the_turn_is_aborted() {
    let ws = TestWorkspace::new();
    let token = ws.token().clone();
    let running = run(json!({"argv": ["sleep", "60"]}), ws.context());
    let cancel = async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    };
    let (execution, ()) = tokio::join!(running, cancel);
    assert_eq!(execution.kind, Some(ErrorKind::Aborted));
}

#[tokio::test]
async fn surfaces_the_abort_through_the_registry_as_a_cancellation() {
    let ws = TestWorkspace::new();
    let registry = ToolRegistry::new();
    registry.register(exec_tool(), ToolSource::Builtin).unwrap();
    let token = ws.token().clone();
    let call = ToolInvocation::with_json("exec", json!({"argv": ["sleep", "60"]}).to_string());
    let running = registry.execute_scoped(&call, ws.context(), None);
    let cancel = async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    };
    let (execution, ()) = tokio::join!(running, cancel);
    assert!(execution.is_error);
    assert_eq!(execution.kind, Some(ErrorKind::Aborted));
}

/// Records what it was asked to run, and answers without a process.
struct Recording {
    seen: Mutex<Vec<RunRequest>>,
    outcome: RunOutcome,
}

impl CommandRunner for Recording {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        Box::pin(async move {
            self.seen.lock().push(request);
            Ok(self.outcome.clone())
        })
    }
}

fn recording() -> Arc<Recording> {
    Arc::new(Recording {
        seen: Mutex::new(Vec::new()),
        outcome: RunOutcome {
            stdout: "from somewhere else".to_owned(),
            code: Some(0),
            ..RunOutcome::default()
        },
    })
}

fn with_runner(ws: &TestWorkspace, runner: &Arc<Recording>) -> ToolContext {
    let mut ctx = ws.context().clone();
    ctx.runner = Arc::clone(runner) as Arc<dyn CommandRunner>;
    ctx
}

#[tokio::test]
async fn uses_the_context_runner_instead_of_spawning() {
    let ws = TestWorkspace::new();
    let runner = recording();
    let result = ok(json!({"argv": ["false"]}), &with_runner(&ws, &runner)).await;
    assert_eq!(runner.seen.lock().len(), 1);
    assert!(result.content.contains("from somewhere else"));
    assert!(!result.is_error);
}

#[tokio::test]
async fn hands_the_runner_a_plan_that_is_already_guarded() {
    let ws = TestWorkspace::new();
    let runner = recording();
    ok(json!({"argv": ["printf", "x"]}), &with_runner(&ws, &runner)).await;
    let seen = runner.seen.lock();
    assert_eq!(seen[0].plan.cwd, ws.root());
    assert_eq!(seen[0].plan.file, "printf");
    assert!(seen[0].plan.env.contains_key("PATH"));
}

#[tokio::test]
async fn still_refuses_a_denied_command_before_any_runner_is_consulted() {
    let ws = TestWorkspace::new();
    let runner = recording();
    let mut ctx = with_runner(&ws, &runner);
    ctx = ctx.with_config({
        let mut config = darkwire_protocol::AgentSettings::default();
        config.exec.denied_binaries = vec!["printf".to_owned()];
        config
    });
    let error = run(json!({"argv": ["printf", "x"]}), &ctx).await;
    assert_eq!(error.kind, Some(ErrorKind::PermissionDenied));
    assert!(runner.seen.lock().is_empty());
}

#[tokio::test]
async fn passes_the_reconciled_timeout_not_the_models_request() {
    let ws = TestWorkspace::new();
    let runner = recording();
    let mut ctx = with_runner(&ws, &runner);
    ctx = ctx.with_config({
        let mut config = darkwire_protocol::AgentSettings::default();
        config.exec.timeout_ms = 5_000;
        config
    });
    ok(json!({"argv": ["printf", "x"], "timeoutMs": 60_000}), &ctx).await;
    assert_eq!(runner.seen.lock()[0].timeout_ms, 5_000);
}

#[tokio::test]
async fn names_the_transcript_when_a_truncated_run_kept_one() {
    let ws = TestWorkspace::new();
    let runner = Arc::new(Recording {
        seen: Mutex::new(Vec::new()),
        outcome: RunOutcome {
            stdout: "head".to_owned(),
            truncated: true,
            code: Some(0),
            transcript_dir: Some("/run/darkwire-runs/c/r1".to_owned()),
            ..RunOutcome::default()
        },
    });
    let result = ok(json!({"argv": ["printf", "x"]}), &with_runner(&ws, &runner)).await;
    assert!(
        result
            .content
            .contains("/run/darkwire-runs/c/r1/stdout.log")
    );
    assert!(result.content.contains("read_file cannot"));
    assert_eq!(
        result.details.get("transcriptDir"),
        Some(&json!("/run/darkwire-runs/c/r1"))
    );
}
