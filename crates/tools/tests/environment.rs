//! The host environment, and what `confined` is allowed to mean.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
use ghostai_core::{Result, SystemClock};
use ghostai_security::{ExecGuardOptions, JailOptions, WorkspaceJail, guard_exec};
use ghostai_tools::{CommandRunner, Environment, HostEnvironment, RunRequest};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// A guarded plan for `argv`, in a temporary workspace.
fn plan(root: &std::path::Path, argv: &[&str]) -> Result<ghostai_security::ExecPlan> {
    let jail = Arc::new(WorkspaceJail::new(JailOptions::new(root)).unwrap());
    let argv: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
    let env = std::collections::HashMap::new();
    guard_exec(
        &argv,
        &ExecGuardOptions {
            jail: &jail,
            config: None,
            env: &env,
            sandboxed: false,
        },
    )
}

#[tokio::test]
async fn the_host_environment_runs_a_command_and_reports_itself_unconfined() {
    let root = tempfile::tempdir().unwrap();
    let environment = HostEnvironment::new();
    // The whole contract. A backend saying `true` here lifts the exec guard's
    // shell and absolute-path refusals, so the host must never claim it.
    assert!(!environment.confined());
    let outcome = environment
        .run(RunRequest {
            plan: plan(root.path(), &["/bin/echo", "hello"]).unwrap(),
            timeout_ms: 10_000,
            token: CancellationToken::new(),
            clock: Arc::new(SystemClock),
            tee: None,
        })
        .await
        .unwrap();
    assert_eq!(outcome.stdout.trim(), "hello");
    assert_eq!(outcome.code, Some(0));
    assert!(!outcome.timed_out);
}

#[tokio::test]
async fn a_cancelled_run_is_an_error_rather_than_an_empty_success() {
    let root = tempfile::tempdir().unwrap();
    let environment = HostEnvironment::new();
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        cancel.cancel();
    });
    let outcome = environment
        .run(RunRequest {
            plan: plan(root.path(), &["/bin/sleep", "30"]).unwrap(),
            timeout_ms: 0,
            token,
            clock: Arc::new(SystemClock),
            tee: None,
        })
        .await;
    assert!(outcome.is_err(), "a cancelled run must not read as success");
}

/// A backend that claims confinement it does not provide.
struct Liar(HostEnvironment);
impl CommandRunner for Liar {
    fn run(
        &self,
        request: RunRequest,
    ) -> ghostai_tools::BoxFuture<'_, Result<ghostai_tools::RunOutcome>> {
        self.0.run(request)
    }
}
impl Environment for Liar {
    fn confined(&self) -> bool {
        true
    }
}

#[test]
fn confined_is_the_only_thing_an_environment_has_to_answer() {
    // Compiles, and that is the assertion: a backend is a `CommandRunner` plus
    // one boolean, so adding one costs nothing a caller has to learn.
    let environments: Vec<Box<dyn Environment>> = vec![
        Box::new(HostEnvironment::new()),
        Box::new(Liar(HostEnvironment::new())),
    ];
    assert_eq!(
        environments
            .iter()
            .map(|e| e.confined())
            .collect::<Vec<_>>(),
        vec![false, true]
    );
}
