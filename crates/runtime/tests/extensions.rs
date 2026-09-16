//! Extensions in the composition root.
//!
//! A real child process, because the host boundary *is* a process: a fake loader
//! would be testing something the shipped code does not do. The fixture is
//! copied into a temporary install and approved by digest first, which is also
//! the only way to test that an unapproved one is left alone.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{Install, configured};
use darkwire_core::{Clock, SystemClock};
use darkwire_protocol::ToolSource;
use darkwire_runtime::{ExtensionChoice, RuntimeOptions, WireRuntime, create_runtime};
use darkwire_security::ExtensionStore;
use serde_json::{Value, json};

fn fixture(id: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(id)
}

/// Copies a fixture extension into `<root>/extensions/<id>`.
///
/// Copied rather than approved where it lies, for the reason the host's own
/// suite gives: the approval digest covers every byte under an install, so a
/// suite that approved the checkout would record a digest that moves the moment
/// anything writes beside it.
fn install_extension(root: &Path, id: &str) -> PathBuf {
    let target = root.join("extensions").join(id);
    std::fs::create_dir_all(&target).unwrap();
    for entry in std::fs::read_dir(fixture(id)).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
    }
    root.join("extensions")
}

/// Approves what is installed, so the host will run it.
fn approve(install: &Install, id: &str) {
    let dir = install.root.join("extensions");
    let store = ExtensionStore::new(
        install.database.clone(),
        &dir,
        Arc::new(SystemClock) as Arc<dyn Clock>,
    )
    .unwrap();
    store.approve(id).unwrap();
}

fn options(install: &Install) -> RuntimeOptions {
    RuntimeOptions {
        extensions: ExtensionChoice::Dir(install.root.join("extensions")),
        // The host's timings are real, so the clock has to be too: a paused
        // clock advances the instant every task is idle on I/O, and a child
        // process is exactly that for most of its life.
        clock: Some(Arc::new(SystemClock)),
        ..install.options()
    }
}

/// A runtime with the greeter installed and approved.
async fn with_greeter(install: &Install) -> Arc<WireRuntime> {
    install_extension(&install.root, "greeter");
    approve(install, "greeter");
    let runtime = create_runtime(options(install)).unwrap();
    assert!(
        common::eventually(Duration::from_secs(20), || runtime
            .extensions()
            .is_some_and(|host| host.loaded_count() == 1))
        .await,
        "the greeter should have loaded"
    );
    runtime
}

#[tokio::test(flavor = "multi_thread")]
async fn puts_an_extensions_tools_in_the_shared_registry_tagged_as_such() {
    let install = Install::with(&configured("llama3"));
    let runtime = with_greeter(&install).await;

    assert!(
        common::eventually(Duration::from_secs(20), || runtime
            .tools()
            .names()
            .iter()
            .any(|name| name.contains("greet")))
        .await,
        "tools: {:?}",
        runtime.tools().names()
    );
    let name = runtime
        .tools()
        .names()
        .into_iter()
        .find(|name| name.contains("greet"))
        .unwrap();
    assert_eq!(
        runtime.tools().source_of(&name),
        Some(ToolSource::Extension)
    );
    runtime.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keeps_them_across_a_reconfigure_like_mcps() {
    let install = Install::with(&configured("llama3"));
    let runtime = with_greeter(&install).await;
    assert!(
        common::eventually(Duration::from_secs(20), || runtime
            .tools()
            .names()
            .iter()
            .any(|name| name.contains("greet")))
        .await
    );

    // A settings save must not tear down every extension and start it again,
    // which is why the host lives beside the registry rather than inside a
    // build.
    runtime
        .reconfigure(&json!({"server": {"port": 4567}}))
        .unwrap();
    assert!(
        runtime
            .tools()
            .names()
            .iter()
            .any(|name| name.contains("greet")),
        "tools: {:?}",
        runtime.tools().names()
    );
    assert!(runtime.tools().has("read_file"));
    runtime.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn takes_an_extensions_tools_away_when_it_is_disabled() {
    let install = Install::with(&configured("llama3"));
    let runtime = with_greeter(&install).await;
    assert!(
        common::eventually(Duration::from_secs(20), || runtime
            .tools()
            .names()
            .iter()
            .any(|name| name.contains("greet")))
        .await
    );

    runtime
        .reconfigure(&json!({"extensions": {"disabled": ["greeter"]}}))
        .unwrap();
    assert!(
        common::eventually(Duration::from_secs(20), || !runtime
            .tools()
            .names()
            .iter()
            .any(|name| name.contains("greet")))
        .await,
        "tools: {:?}",
        runtime.tools().names()
    );
    // The built-ins are untouched: teardown is exact by extension id.
    assert!(runtime.tools().has("read_file"));
    runtime.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn does_not_load_anything_an_operator_has_not_approved() {
    // Code loading itself into the host is the class of attack the digest
    // closes: installed is not approved.
    let install = Install::with(&configured("llama3"));
    install_extension(&install.root, "greeter");
    let runtime = create_runtime(options(&install)).unwrap();

    assert!(
        common::eventually(Duration::from_secs(10), || !runtime
            .extensions()
            .unwrap()
            .status()
            .is_empty())
        .await,
        "the row should exist even though nothing ran"
    );
    assert_eq!(runtime.extensions().unwrap().loaded_count(), 0);
    assert!(
        !runtime
            .tools()
            .names()
            .iter()
            .any(|name| name.contains("greet"))
    );
    runtime.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn boots_with_no_host_at_all_when_extensions_are_switched_off() {
    let install = Install::with(&configured("llama3"));
    install_extension(&install.root, "greeter");
    approve(&install, "greeter");
    // `Off` is the harness default, and this is what it proves: an install with
    // nothing in the directory pays nothing either way, and a test that wants to
    // show the registry holds only built-ins can say so.
    let runtime = install.runtime().unwrap();
    assert!(runtime.extensions().is_none());
    assert!(runtime.tools().has("read_file"));
    assert!(
        !runtime
            .tools()
            .names()
            .iter()
            .any(|name| name.contains("greet"))
    );
    runtime.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn adds_an_extensions_prompt_section_to_the_loop() {
    let install = Install::with(&configured("llama3"));
    let runtime = with_greeter(&install).await;
    // A prompt section is read *during* a build rather than per turn, so this is
    // what the listener-plus-rebuild exists for: an extension that loaded after
    // the last build would contribute nothing until something else happened.
    assert!(
        common::eventually(Duration::from_secs(20), || !runtime
            .extensions()
            .unwrap()
            .contributors()
            .is_empty())
        .await
    );
    assert!(runtime.agent_loop().is_some());
    runtime.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hands_an_extension_its_settings_block_from_the_config_tree() {
    let mut tree: Value = configured("llama3");
    tree["extensions"] = json!({"settings": {"greeter": {"tone": "warm"}}});
    let install = Install::with(&tree);
    let runtime = with_greeter(&install).await;
    // The block reaches the child in its handshake; what this asserts here is
    // that the composition root passed the whole `extensions` config through
    // rather than only its `disabled` list.
    assert_eq!(
        runtime.config().extensions.settings["greeter"]["tone"],
        "warm"
    );
    assert_eq!(runtime.extensions().unwrap().loaded_count(), 1);
    runtime.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reloading_extensions_is_the_path_an_approval_takes() {
    // Approving records a row in a table rather than editing `config.yaml`, so
    // neither a reconfigure nor a reload — which both start from settings —
    // would ever notice it.
    let install = Install::with(&configured("llama3"));
    install_extension(&install.root, "greeter");
    let runtime = create_runtime(options(&install)).unwrap();
    assert!(
        common::eventually(Duration::from_secs(10), || !runtime
            .extensions()
            .unwrap()
            .status()
            .is_empty())
        .await
    );
    assert_eq!(runtime.extensions().unwrap().loaded_count(), 0);

    approve(&install, "greeter");
    runtime.reload_extensions();
    assert!(
        common::eventually(Duration::from_secs(20), || runtime
            .extensions()
            .unwrap()
            .loaded_count()
            == 1)
        .await,
        "status: {:?}",
        runtime.extensions().unwrap().status()
    );
    runtime.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn survives_an_extension_that_cannot_start_and_still_builds_a_loop() {
    let install = Install::with(&configured("llama3"));
    let dir = install_extension(&install.root, "greeter");
    approve(&install, "greeter");
    // Approved, then broken: the digest no longer matches, so the host refuses
    // to run it — and the install must still come up.
    std::fs::write(dir.join("greeter/index.mjs"), "syntax ( error").unwrap();

    let runtime = create_runtime(options(&install)).unwrap();
    assert!(runtime.configured());
    assert!(runtime.agent_loop().is_some());
    assert!(runtime.tools().has("read_file"));
    runtime.close().await;
}
