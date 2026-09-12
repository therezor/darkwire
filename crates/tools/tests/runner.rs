//! `LocalRunner` against real child processes: caps, completion, cancellation
//! and the signal escalation.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ghostai_core::{ErrorKind, SystemClock};
use ghostai_security::ExecPlan;
use ghostai_tools::{
    CommandRunner, KILL_GRACE_MS, LocalRunner, OutputStream, OutputTee, RunRequest,
};
use indexmap::IndexMap;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

fn plan(argv: &[&str], max_output_bytes: u64) -> ExecPlan {
    let mut env = IndexMap::new();
    env.insert("PATH".to_owned(), std::env::var("PATH").unwrap_or_default());
    ExecPlan {
        file: argv[0].to_owned(),
        args: argv[1..].iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: PathBuf::from("/"),
        env,
        timeout_ms: 0,
        max_output_bytes,
        paths: Vec::new(),
    }
}

fn request(plan: ExecPlan, timeout_ms: u64) -> RunRequest {
    RunRequest {
        plan,
        timeout_ms,
        token: CancellationToken::new(),
        clock: Arc::new(SystemClock),
        tee: None,
    }
}

#[derive(Default)]
struct Recorder {
    stdout: Mutex<Vec<u8>>,
    stderr: Mutex<Vec<u8>>,
}

impl OutputTee for Recorder {
    fn write(&self, stream: OutputStream, chunk: &[u8]) {
        match stream {
            OutputStream::Stdout => self.stdout.lock().extend_from_slice(chunk),
            OutputStream::Stderr => self.stderr.lock().extend_from_slice(chunk),
        }
    }
}

#[tokio::test]
async fn collects_both_streams_and_the_exit_code() {
    let outcome = LocalRunner::new()
        .run(request(plan(&["printf", "hello"], 1024), 0))
        .await
        .unwrap();
    assert_eq!(outcome.stdout, "hello");
    assert_eq!(outcome.stderr, "");
    assert_eq!(outcome.code, Some(0));
    assert_eq!(outcome.signal, None);
    assert!(!outcome.timed_out);
    assert!(!outcome.truncated);
    assert!(outcome.transcript_dir.is_none());
}

#[tokio::test]
async fn reports_a_non_zero_exit_with_its_stderr() {
    let outcome = LocalRunner::new()
        .run(request(plan(&["ls", "/definitely/not/a/path"], 4096), 0))
        .await
        .unwrap();
    assert_ne!(outcome.code, Some(0));
    assert!(!outcome.stderr.is_empty());
}

#[tokio::test]
async fn fails_to_start_a_program_that_does_not_exist() {
    let error = LocalRunner::new()
        .run(request(plan(&["ghostai-definitely-not-a-binary"], 1024), 0))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Tool);
    assert!(error.message.contains("Could not run"));
}

#[tokio::test]
async fn caps_output_while_the_child_writes_and_keeps_the_head() {
    let outcome = LocalRunner::new()
        .run(request(plan(&["printf", "%0200000d", "0"], 64), 0))
        .await
        .unwrap();
    assert!(outcome.truncated);
    assert_eq!(outcome.stdout, "0".repeat(64));
}

#[tokio::test]
async fn an_endless_writer_is_stopped_by_the_cap_when_nobody_keeps_the_rest() {
    // Without a tee the pipe is closed once the budget is spent, so `yes`
    // dies of EPIPE rather than running until the timeout.
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        LocalRunner::new().run(request(plan(&["yes"], 32), 0)),
    )
    .await
    .expect("the cap should end the run")
    .unwrap();
    assert!(outcome.truncated);
    assert_eq!(outcome.stdout.len(), 32);
}

#[tokio::test]
async fn a_tee_receives_everything_while_the_cap_bounds_the_outcome() {
    let recorder = Arc::new(Recorder::default());
    let mut req = request(plan(&["printf", "%05000d", "0"], 64), 0);
    req.tee = Some(Arc::clone(&recorder) as Arc<dyn OutputTee>);
    let outcome = LocalRunner::new().run(req).await.unwrap();
    assert!(outcome.truncated);
    assert_eq!(outcome.stdout.len(), 64);
    assert_eq!(recorder.stdout.lock().len(), 5000);
    assert!(recorder.stderr.lock().is_empty());
}

#[tokio::test]
async fn kills_a_program_that_outlives_its_timeout() {
    let outcome = LocalRunner::new()
        .run(request(plan(&["sleep", "60"], 1024), 150))
        .await
        .unwrap();
    assert!(outcome.timed_out);
    assert_eq!(outcome.code, None);
    assert_eq!(outcome.signal.as_deref(), Some("SIGTERM"));
}

#[tokio::test]
async fn escalates_to_sigkill_after_the_grace_period() {
    // A zero grace makes the escalation certain to fire; the process is dead
    // either way, and both signals report as a kill.
    let outcome = LocalRunner::with_kill_grace(Duration::ZERO)
        .run(request(plan(&["sleep", "60"], 1024), 100))
        .await
        .unwrap();
    assert!(outcome.timed_out);
    assert!(outcome.signal.is_some());
    assert_eq!(KILL_GRACE_MS, 2_000);
}

#[tokio::test]
async fn cancelling_the_token_kills_the_child_and_reports_an_abort() {
    let mut req = request(plan(&["sleep", "60"], 1024), 0);
    let token = CancellationToken::new();
    req.token = token.clone();
    let runner = LocalRunner::default();
    let running = runner.run(req);
    let cancel = async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    };
    let (result, ()) = tokio::join!(running, cancel);
    assert_eq!(result.err().unwrap().kind, ErrorKind::Aborted);
}

#[tokio::test]
async fn cancelling_with_a_timeout_armed_is_still_an_abort() {
    let mut req = request(plan(&["sleep", "60"], 1024), 30_000);
    let token = CancellationToken::new();
    req.token = token.clone();
    let runner = LocalRunner::new();
    let running = runner.run(req);
    let cancel = async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        token.cancel();
    };
    let (result, ()) = tokio::join!(running, cancel);
    assert_eq!(result.err().unwrap().kind, ErrorKind::Aborted);
}

#[tokio::test]
async fn the_child_inherits_only_the_plan_environment() {
    let mut p = plan(&["env"], 65_536);
    p.env
        .insert("GHOSTAI_RUNNER_TEST".to_owned(), "yes".to_owned());
    let outcome = LocalRunner::new().run(request(p, 0)).await.unwrap();
    assert!(outcome.stdout.contains("GHOSTAI_RUNNER_TEST=yes"));
    let lines: Vec<&str> = outcome.stdout.lines().collect();
    assert!(
        lines
            .iter()
            .all(|line| line.starts_with("PATH=") || line.starts_with("GHOSTAI_RUNNER_TEST=")),
        "{lines:?}"
    );
}

#[test]
fn a_request_describes_itself_without_dumping_its_tee() {
    let text = format!("{:?}", request(plan(&["true"], 1), 5));
    assert!(text.contains("timeout_ms: 5"));
    assert!(text.contains("tee: false"));
}
