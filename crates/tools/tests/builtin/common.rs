//! Helpers every built-in's tests use: run a tool, take its text, or assert
//! that it refused.

use darkwire_tools::{AnyTool, ToolContext, ToolExecution};
use serde_json::Value;

pub async fn run(tool: &AnyTool, args: Value, ctx: &ToolContext) -> ToolExecution {
    tool.execute(args, ctx).await
}

pub async fn text(tool: &AnyTool, args: Value, ctx: &ToolContext) -> String {
    let execution = run(tool, args, ctx).await;
    assert!(!execution.is_error, "{}", execution.content);
    execution.content
}

pub async fn failure(tool: &AnyTool, args: Value, ctx: &ToolContext) -> ToolExecution {
    let execution = run(tool, args, ctx).await;
    assert!(
        execution.is_error,
        "expected a failure, got {}",
        execution.content
    );
    assert!(execution.kind.is_some());
    execution
}

/// Runs a tool against a FIFO, failing rather than hanging if it blocks in
/// `open`.
pub async fn run_on_fifo(
    tool: &AnyTool,
    args: Value,
    ctx: &ToolContext,
    fifo: &std::path::Path,
) -> ToolExecution {
    use std::os::unix::fs::OpenOptionsExt as _;
    let outcome =
        tokio::time::timeout(std::time::Duration::from_secs(5), run(tool, args, ctx)).await;
    if let Ok(execution) = outcome {
        return execution;
    }
    // Open the other end, so the stuck `open` returns and the runtime can
    // shut down instead of hanging the test.
    let flags = nix::fcntl::OFlag::O_NONBLOCK.bits();
    let _ = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(fifo);
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(flags)
        .open(fifo);
    panic!("the tool blocked on a FIFO");
}

/// Makes a FIFO at `path`.
pub fn fifo(path: &std::path::Path) {
    nix::unistd::mkfifo(path, nix::sys::stat::Mode::S_IRWXU).unwrap();
}

/// Links `linked` in the workspace to a directory outside it holding
/// `secret.txt`, and returns that directory.
pub fn link_out(ws: &darkwire_tools::testkit::TestWorkspace) -> std::path::PathBuf {
    let elsewhere = ws.outside().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("secret.txt"), "stolen").unwrap();
    std::os::unix::fs::symlink(&elsewhere, ws.root().join("linked")).unwrap();
    elsewhere
}

/// Links `alias` in the workspace to `real`, which holds `notes.md`.
pub fn link_in(ws: &darkwire_tools::testkit::TestWorkspace) {
    std::fs::create_dir(ws.root().join("real")).unwrap();
    std::fs::write(ws.root().join("real/notes.md"), "inside\n").unwrap();
    std::os::unix::fs::symlink(ws.root().join("real"), ws.root().join("alias")).unwrap();
}
