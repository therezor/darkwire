//! The child: what it can see, where its noise goes, and how it is stopped.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::path::PathBuf;
use std::time::Duration;

use ghostai_extension_host::{
    ENV_EXTENSION_DATA_DIR, ENV_EXTENSION_ID, SpawnOptions, data_dir_for, spawn,
};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn script(dir: &std::path::Path, body: &str) -> PathBuf {
    let file = dir.join("index.mjs");
    std::fs::write(&file, body).unwrap();
    file
}

fn options(dir: &std::path::Path) -> SpawnOptions {
    SpawnOptions::new(
        "probe",
        dir,
        vec!["node".to_owned(), "index.mjs".to_owned()],
        dir.join("data"),
    )
    .with_kill_grace(Duration::from_millis(200))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_child_sees_the_allow_list_and_the_two_the_host_sets() {
    // Cargo sets `CARGO_PKG_NAME` for this test process and nothing sets it for
    // an extension, which makes it the perfect stand-in for the thing this rule
    // exists to stop: a value in the host's own environment that a child has no
    // business seeing. It is asserted absent, then asked for by name and
    // asserted present — the same variable both ways, so the only difference is
    // the manifest.
    assert!(std::env::var("CARGO_PKG_NAME").is_ok(), "cargo sets this");

    let temp = TempDir::new().unwrap();
    script(
        temp.path(),
        "process.stdout.write(JSON.stringify(process.env) + '\\n');\n",
    );

    let spawned = spawn(options(temp.path())).expect("node is on PATH");
    let mut lines = BufReader::new(spawned.stdout).lines();
    let line = lines.next_line().await.unwrap().unwrap();
    let env: serde_json::Value = serde_json::from_str(&line).unwrap();

    // A provider key in `ghostai serve`'s environment must not land inside
    // third-party code, and nothing but the allow-list does.
    assert!(env["CARGO_PKG_NAME"].is_null(), "{env}");
    // The default four, where this host has them.
    assert_eq!(
        env["PATH"],
        serde_json::json!(std::env::var("PATH").unwrap())
    );
    // And the two the host sets itself, which are the whole of what an
    // extension knows about its installation before `initialize` arrives.
    assert_eq!(env[ENV_EXTENSION_ID], "probe");
    assert!(
        env[ENV_EXTENSION_DATA_DIR]
            .as_str()
            .unwrap_or_default()
            .ends_with("data")
    );

    // Now the same variable, named by the manifest.
    let asked = spawn(options(temp.path()).with_env(vec![
        "CARGO_PKG_NAME".to_owned(),
        // A name this host does not have is simply absent there: better than an
        // empty string, which a program reading `TMPDIR` would treat as a path.
        "GHOSTAI_NOT_SET_ANYWHERE".to_owned(),
        // A duplicate of the allow-list is not passed twice.
        "PATH".to_owned(),
    ]))
    .expect("node is on PATH");
    let mut lines = BufReader::new(asked.stdout).lines();
    let line = lines.next_line().await.unwrap().unwrap();
    let env: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        env["CARGO_PKG_NAME"],
        serde_json::json!(std::env::var("CARGO_PKG_NAME").unwrap())
    );
    assert!(env["GHOSTAI_NOT_SET_ANYWHERE"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_program_that_is_not_there_is_an_error_rather_than_a_panic() {
    let temp = TempDir::new().unwrap();
    let mut options = options(temp.path());
    options.command = vec!["ghostai-no-such-program".to_owned()];
    let error = spawn(options).expect_err("a missing program cannot spawn");
    assert!(
        error.message.contains("could not be started"),
        "{}",
        error.message
    );

    let mut empty = self::options(temp.path());
    empty.command = Vec::new();
    let error = spawn(empty).expect_err("an empty argv cannot spawn");
    assert!(error.message.contains("no command"), "{}", error.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_exits_on_its_own_reports_how() {
    let temp = TempDir::new().unwrap();
    script(temp.path(), "process.exit(3);\n");
    let spawned = spawn(options(temp.path())).expect("node is on PATH");

    let status = spawned.process.wait().await;
    assert!(status.contains("status 3"), "{status}");
    assert!(spawned.process.has_exited());
    assert_eq!(spawned.process.id(), "probe");
    assert!(spawned.process.pid().is_some());
    // Asking a process that has already gone to stop is a no-op, not a hang.
    spawned.process.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_leaves_on_a_closed_stdin_is_never_signalled() {
    let temp = TempDir::new().unwrap();
    script(
        temp.path(),
        "import {createInterface} from 'node:readline';\n\
         const l = createInterface({input: process.stdin});\n\
         l.on('close', () => process.exit(0));\n",
    );
    let spawned = spawn(options(temp.path())).expect("node is on PATH");
    // Dropping the host's write half is what closes the child's stdin.
    drop(spawned.stdin);

    let status = spawned.process.wait().await;
    // Status 0, not a signal: it left politely and nothing had to escalate.
    assert!(status.contains("status 0"), "{status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_ignores_every_signal_but_one_is_still_stopped() {
    let temp = TempDir::new().unwrap();
    script(
        temp.path(),
        "process.on('SIGTERM', () => {});\n\
         process.on('SIGINT', () => {});\n\
         setInterval(() => {}, 1000);\n",
    );
    let spawned = spawn(options(temp.path())).expect("node is on PATH");

    let started = std::time::Instant::now();
    spawned.process.stop().await;
    let elapsed = started.elapsed();

    // Two grace periods, then SIGKILL. Anything faster means an escalation was
    // skipped; anything much slower means one did not fire.
    assert!(elapsed >= Duration::from_millis(400), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    assert!(spawned.process.wait().await.contains("signal"));
}

#[tokio::test(flavor = "multi_thread")]
async fn stderr_is_drained_past_the_budget_so_the_child_never_blocks() {
    let temp = TempDir::new().unwrap();
    // Well past the 64 KiB budget: the logging stops and the draining does not,
    // which is the difference between a quiet child and a hung one.
    script(
        temp.path(),
        "for (let i = 0; i < 4000; i += 1) console.error('x'.repeat(64));\n\
         process.stdout.write('done\\n');\n",
    );
    let spawned = spawn(options(temp.path())).expect("node is on PATH");

    let mut lines = BufReader::new(spawned.stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("the child did not block on a full stderr pipe")
        .unwrap();
    assert_eq!(line.as_deref(), Some("done"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_child_runs_in_its_install_directory() {
    let temp = TempDir::new().unwrap();
    script(
        temp.path(),
        "process.stdout.write(process.cwd() + '\\n');\n",
    );
    let spawned = spawn(options(temp.path())).expect("node is on PATH");

    let mut lines = BufReader::new(spawned.stdout).lines();
    let cwd = lines.next_line().await.unwrap().unwrap();
    assert_eq!(
        std::fs::canonicalize(cwd).unwrap(),
        std::fs::canonicalize(temp.path()).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_host_can_write_to_a_child_that_reads() {
    let temp = TempDir::new().unwrap();
    script(
        temp.path(),
        "import {createInterface} from 'node:readline';\n\
         const l = createInterface({input: process.stdin});\n\
         l.on('line', (line) => process.stdout.write(line.toUpperCase() + '\\n'));\n\
         l.on('close', () => process.exit(0));\n",
    );
    let mut spawned = spawn(options(temp.path())).expect("node is on PATH");
    spawned.stdin.write_all(b"hello\n").await.unwrap();
    spawned.stdin.flush().await.unwrap();

    let mut lines = BufReader::new(spawned.stdout).lines();
    assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("HELLO"));
}

#[test]
fn the_data_directory_is_a_sibling_of_the_install_and_never_a_child() {
    let root = PathBuf::from("/var/ghostai");
    let data = data_dir_for(&root, "hello");
    assert_eq!(data, PathBuf::from("/var/ghostai/extension-data/hello"));
    // The whole rule: the approval digest covers every byte under the install,
    // so state written in there would revoke the approval on the first write.
    assert!(!data.starts_with(root.join("extensions")));
}
