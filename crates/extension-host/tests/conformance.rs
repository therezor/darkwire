//! The suite that spawns real child processes.
//!
//! Everything here runs Node. That is the point: the wire is proved over an
//! in-memory pipe in `rpc.rs`, and what is left — that a manifest, a directory
//! and a program agree with each other, and that a child which will not leave
//! is made to — can only be proved against a process.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use crate::common;

use std::path::PathBuf;
use std::time::Duration;

use darkwire_extension_host::testkit::{Expect, extension_conformance};
use darkwire_protocol::{ExtensionState, ExtensionsConfig};
use tokio_util::sync::CancellationToken;

use common::{Harness, eventually};

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/hello-extension")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_reference_extension_passes_its_own_conformance_suite() {
    let report = extension_conformance(
        &example_dir(),
        Expect {
            tools: 1,
            commands: 1,
            context: 1,
        },
    )
    .await
    .expect("the reference extension conforms");

    assert_eq!(report.status.state, ExtensionState::Ready);
    assert!(
        report.status.warnings.is_empty(),
        "{:?}",
        report.status.warnings
    );
    // The namespace rule, applied: the extension called it `greet`.
    assert_eq!(report.status.tools, vec!["ext_hello_greet".to_owned()]);
    assert_eq!(report.status.commands, vec!["hello-time".to_owned()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_handshake_carries_the_settings_the_extension_reads() {
    let harness = Harness::with_example();
    harness.approve("hello");

    let mut config = ExtensionsConfig::default();
    config.settings.insert(
        "hello".to_owned(),
        serde_json::from_value(serde_json::json!({"greeting": "Ahoy"})).unwrap(),
    );
    harness.settle(&config).await;
    assert_eq!(harness.state("hello"), ExtensionState::Ready);

    // The settings reached the child through `params._meta.darkwire` and it read
    // them once, exactly where a v1 extension read them in `activate`.
    let outcome = harness
        .host
        .run_command("hello-time", "", None, &CancellationToken::new())
        .await
        .expect("the command is served");
    assert!(outcome.ok);
    assert!(outcome.message.starts_with("Ahoy"), "{}", outcome.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_reaches_the_child_and_comes_back() {
    let harness = Harness::with_example();
    harness.approve("hello");
    harness.settle_default().await;

    let tools = harness.host.tools();
    assert_eq!(tools.len(), 1);
    let tool = &tools[0];
    assert_eq!(tool.definition().name, "ext_hello_greet");
    // The one hint the bridge believes at face value.
    assert_eq!(tool.risk(), darkwire_protocol::ToolRisk::Safe);
    // And the description says whose it is, not "from the hello MCP server".
    assert!(tool.definition().description.contains("Greet"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kind_the_manifest_declares_and_the_extension_lacks_is_a_row_warning() {
    let harness = Harness::with(&["absent"]);
    harness.approve("absent");
    harness.settle_default().await;

    let row = harness.row("absent");
    assert_eq!(row.state, ExtensionState::Ready);
    let warnings = row.warnings.join("\n");
    assert!(warnings.contains("darkwire/commands/list"), "{warnings}");
    assert!(warnings.contains("darkwire/context/static"), "{warnings}");
    // `tools` is declared *and* implemented, so it earns no sentence.
    assert!(!warnings.contains("tools/list"), "{warnings}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kind_the_extension_serves_and_the_manifest_omits_is_dropped_with_a_warning() {
    let harness = Harness::with(&["chatty"]);
    harness.approve("chatty");
    harness.settle_default().await;

    let row = harness.row("chatty");
    assert_eq!(row.state, ExtensionState::Ready);
    // The tool it declared is there; the command it did not declare is not.
    assert_eq!(row.tools, vec!["ext_chatty_echo".to_owned()]);
    assert!(row.commands.is_empty());
    assert!(harness.host.commands().is_empty());
    let warnings = row.warnings.join("\n");
    assert!(
        warnings.contains("\"contributes\" does not declare"),
        "{warnings}"
    );
    assert!(warnings.contains("commands"), "{warnings}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_v1_bundle_lands_failed_with_the_sentence_that_names_the_reason() {
    let harness = Harness::with(&["oldschool"]);
    // Approvable — the v1 policy still passes — and still unrunnable, which is
    // the point: the refusal is about the contract, not about the bytes.
    harness.approve("oldschool");
    harness.settle_default().await;

    let row = harness.row("oldschool");
    assert_eq!(row.state, ExtensionState::Failed);
    let sentence = row.last_error.clone().unwrap_or_default();
    assert!(sentence.contains("darkwire.extension/1"), "{sentence}");
    assert!(sentence.contains("darkwire.extension/2"), "{sentence}");
    // And it is described, not merely refused: the row carries its manifest.
    assert_eq!(row.label, "Old School");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edited_byte_moves_the_row_to_drifted_and_stops_the_process() {
    let harness = Harness::with_example();
    harness.approve("hello");
    harness.settle_default().await;
    assert_eq!(harness.state("hello"), ExtensionState::Ready);

    // The digest covers the code, not only the manifest.
    let script = harness.install("hello").join("index.mjs");
    let source = std::fs::read_to_string(&script).unwrap();
    std::fs::write(&script, format!("{source}\n// edited\n")).unwrap();

    harness.settle_default().await;
    assert_eq!(harness.state("hello"), ExtensionState::Drifted);
    assert!(harness.host.tools().is_empty());
    assert!(harness.row("hello").last_error.is_some());

    // Approving the new bytes starts it again from them, with no restart of
    // the host: the "reloading needs a restart" rule is gone with the module
    // registry that caused it.
    harness.approve("hello");
    harness.settle_default().await;
    assert_eq!(harness.state("hello"), ExtensionState::Ready);
    assert_eq!(harness.host.tools().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unapproved_extension_is_a_row_rather_than_a_refused_boot() {
    let harness = Harness::with(&["chatty"]);
    harness.settle_default().await;

    let row = harness.row("chatty");
    assert_eq!(row.state, ExtensionState::Unapproved);
    assert!(row.last_error.unwrap_or_default().contains("approve"));
    assert!(harness.host.tools().is_empty());
    assert_eq!(harness.host.loaded_count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_extension_keeps_its_row_and_loses_its_process() {
    let harness = Harness::with_example();
    harness.approve("hello");
    harness.settle_default().await;
    assert_eq!(harness.host.loaded_count(), 1);

    let mut config = ExtensionsConfig::default();
    config.disabled.push("hello".to_owned());
    harness.settle(&config).await;

    assert_eq!(harness.state("hello"), ExtensionState::Disabled);
    assert!(harness.host.tools().is_empty());
    assert_eq!(harness.host.loaded_count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_command_ends_it_inside_the_grace_period() {
    let harness = Harness::with(&["slow"]);
    harness.approve("slow");
    harness.settle_default().await;
    assert_eq!(harness.state("slow"), ExtensionState::Ready);

    let token = CancellationToken::new();
    let cancelling = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancelling.cancel();
    });

    let started = std::time::Instant::now();
    let outcome = harness
        .host
        .run_command("slow-forever", "", Some("s1"), &token)
        .await
        .expect("the command is served");

    // The extension never answers. What ends the call is the cancellation, and
    // it ends as a failed result rather than as a propagated error.
    assert!(!outcome.ok);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the cancelled command took {:?}",
        started.elapsed()
    );
    // And the extension is still loaded: a cancelled command is not a crash.
    assert_eq!(harness.state("slow"), ExtensionState::Ready);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_context_section_that_never_arrives_costs_only_itself() {
    use darkwire_agent::StaticPromptContext;

    let harness = Harness::with(&["slow"]);
    harness.approve("slow");
    harness.settle_default().await;

    let contributors = harness.host.contributors();
    assert_eq!(contributors.len(), 1);

    let context = StaticPromptContext {
        session_key: "s1".to_owned(),
        ..StaticPromptContext::default()
    };
    let started = std::time::Instant::now();
    let section = contributors[0].static_section(&context).await;

    // The static half arrived; the runtime half timed out and placed nothing.
    assert!(
        section
            .unwrap_or_default()
            .contains("A section that arrives")
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the capped call took {:?}",
        started.elapsed()
    );
    assert_eq!(harness.state("slow"), ExtensionState::Ready);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_will_not_leave_is_terminated_and_then_killed() {
    let harness = Harness::with(&["stubborn"]);
    harness.approve("stubborn");
    harness.settle_default().await;
    assert_eq!(harness.state("stubborn"), ExtensionState::Ready);

    // It ignores a closed stdin and traps SIGTERM, so only SIGKILL ends it.
    let started = std::time::Instant::now();
    harness.host.stop().await;
    let elapsed = started.elapsed();

    // Both escalations were waited out — a child that left on stdin close would
    // be gone in well under one grace period.
    assert!(
        elapsed >= Duration::from_millis(400),
        "the escalation did not wait out both stages: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the escalation did not finish: {elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_while_ready_becomes_a_failed_row_and_one_restart() {
    let harness = Harness::with_example();
    harness.approve("hello");
    harness.settle_default().await;
    assert_eq!(harness.state("hello"), ExtensionState::Ready);

    // Killed out from under the host, which is what a segfault, an OOM kill or
    // a `process.exit(1)` look like from here.
    let pid = harness
        .host
        .pid("hello")
        .expect("a running extension has a pid");
    kill_extension(pid);

    assert!(
        eventually(Duration::from_secs(10), || harness.state("hello")
            == ExtensionState::Failed)
        .await,
        "the crash was never reported: {:?}",
        harness.row("hello")
    );
    // The bag goes whole: a tool whose process is gone must not be offered to
    // a turn that starts in the meantime.
    assert!(harness.host.tools().is_empty());
    assert!(
        harness
            .row("hello")
            .last_error
            .unwrap_or_default()
            .contains("signal")
    );

    // The one automatic restart brings it back on its own.
    assert!(
        eventually(Duration::from_secs(15), || harness.state("hello")
            == ExtensionState::Ready)
        .await,
        "the extension was not restarted: {:?}",
        harness.row("hello")
    );
    assert_eq!(harness.host.tools().len(), 1);
}

/// Kills an extension's child out from under the host.
#[cfg(unix)]
fn kill_extension(pid: u32) {
    let pid = i32::try_from(pid).expect("a pid fits");
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGKILL,
    );
}
