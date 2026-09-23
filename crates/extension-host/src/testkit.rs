//! The conformance suite: does this directory behave like an extension?
//!
//! It is the half of the contract an extension author cannot check from inside
//! their own code, and that is the whole reason it exists here rather than in a
//! helper they write. Three of the four rules are only visible from the host's
//! side of the pipe:
//!
//!  - the handshake completes, and completes within the cap;
//!  - every kind the manifest **declares** answers its list method;
//!  - every kind the extension **answers** is one the manifest declared;
//!  - every id it hands back is namespaced to the extension.
//!
//! So this runs the real host against a real child. It copies the directory
//! into a temporary tree first, because the approval digest covers every byte
//! under an install and a working checkout has build output, test files and
//! dependency trees in it that a shipped extension would not — `test.tsbuildinfo`
//! alone is rewritten by every type-check, which would move the digest between
//! two runs of the same suite.
//!
//! Free of any test framework, deliberately: it is called from a vitest test in
//! one repository, from `cargo test` in this one, and from the `check` example
//! that an operator command will eventually be.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::{Database, ErrorKind, Result, SystemClock, WireError};
use darkwire_protocol::{ExtensionState, ExtensionStatus, ExtensionsConfig};
use darkwire_security::ExtensionStore;

use crate::host::{ExtensionHost, ExtensionHostOptions, Timings};

/// Directory and file names a working checkout carries and a shipped extension
/// does not.
///
/// Skipped when the install is copied, so the suite measures the extension
/// rather than whatever the author's editor and toolchain left behind.
const NOT_SHIPPED: &[&str] = &["node_modules", "test", "tests", "dist", ".turbo", ".git"];

/// What the caller expects the extension to contribute.
///
/// Counts rather than names, because a suite that pinned names would have to be
/// edited every time an extension renamed a tool — and the rule being checked is
/// "what the manifest declares is what the extension serves", which counts
/// express exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Expect {
    /// How many tools, after the `ext_<id>_` prefix is applied.
    pub tools: usize,
    /// How many commands.
    pub commands: usize,
    /// How many prompt contributors. One or zero, in practice.
    pub context: usize,
}

/// What the suite found.
#[derive(Debug, Clone)]
pub struct ConformanceReport {
    /// The extension's row, exactly as a panel would show it.
    pub status: ExtensionStatus,
    /// What it actually contributed.
    pub found: Expect,
}

impl ConformanceReport {
    /// The one-line summary the `check` example prints.
    pub fn summary(&self) -> String {
        format!(
            "{}: {} tools={} commands={} context={}",
            self.status.id,
            state_name(self.status.state),
            self.found.tools,
            self.found.commands,
            self.found.context,
        )
    }
}

fn state_name(state: ExtensionState) -> &'static str {
    match state {
        ExtensionState::Ready => "ready",
        ExtensionState::Unapproved => "unapproved",
        ExtensionState::Drifted => "drifted",
        ExtensionState::Disabled => "disabled",
        ExtensionState::Failed => "failed",
    }
}

fn failed(message: String) -> WireError {
    WireError::new(ErrorKind::Extension, message)
}

/// Copies an install into `target`, skipping what a checkout carries.
///
/// Recursive and symlink-free, matching what the digest walks: a symlinked file
/// is content outside the bytes being approved, so copying one would produce a
/// tree the real gate would refuse.
fn copy_install(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if NOT_SHIPPED.contains(&name.as_ref()) || name.ends_with(".tsbuildinfo") {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            copy_install(&entry.path(), &target.join(name.as_ref()))?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target.join(name.as_ref()))?;
        }
    }
    Ok(())
}

/// Where a conformance run puts its copy and its ledger.
///
/// Held by the caller for the life of the run: dropping it removes the tree,
/// which is what takes the copied install with it.
#[derive(Debug)]
pub struct ConformanceSandbox {
    root: PathBuf,
}

impl ConformanceSandbox {
    /// The root the extension was copied under.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for ConformanceSandbox {
    fn drop(&mut self) {
        // Best effort. A temporary tree that outlives a crashed run is a
        // nuisance, not a failure worth reporting over the real one.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Runs one extension directory through the real host and reports what it did.
///
/// Fails when the extension does not reach `ready`, or when what it contributed
/// is not what `expect` says — with the row's own sentence and warnings in the
/// message, since those are what an author needs and are already phrased for a
/// person.
pub async fn extension_conformance(dir: &Path, expect: Expect) -> Result<ConformanceReport> {
    let manifest = darkwire_security::read_extension_manifest(dir)?;
    let id = manifest.id.clone();

    // The id names the directory, and the policy gate enforces that — so the
    // copy has to be made under the id rather than under whatever the author
    // called their checkout.
    let root = std::env::temp_dir().join(format!(
        "darkwire-conformance-{}-{}",
        id,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let install_root = root.join("extensions");
    let sandbox = ConformanceSandbox { root: root.clone() };
    copy_install(dir, &install_root.join(&id))?;

    let store = ExtensionStore::new(Database::in_memory()?, &install_root, Arc::new(SystemClock))?;
    store.approve(&id)?;

    let host = ExtensionHost::new(
        ExtensionHostOptions::new(store, &root).with_timings(Timings {
            init_timeout: Duration::from_secs(10),
            kill_grace: Duration::from_millis(500),
            respawn_delay: Duration::from_secs(5),
            write_timeout: crate::rpc::REQUEST_TIMEOUT,
        }),
    );
    host.reconcile(&ExtensionsConfig::default());
    if !host.quiesce(Duration::from_secs(30)).await {
        host.stop().await;
        return Err(failed(format!(
            "The extension \"{id}\" did not finish starting within 30 seconds."
        )));
    }

    let Some(status) = host.status().into_iter().find(|row| row.id == id) else {
        host.stop().await;
        return Err(failed(format!(
            "The extension \"{id}\" produced no status row at all."
        )));
    };

    let found = Expect {
        tools: host.tools().len(),
        commands: host.commands().len(),
        context: host.contributors().len(),
    };
    host.stop().await;
    drop(sandbox);

    if status.state != ExtensionState::Ready {
        return Err(failed(format!(
            "The extension \"{id}\" is {} rather than ready.\n  {}",
            state_name(status.state),
            status.last_error.clone().unwrap_or_default()
        )));
    }
    if !status.warnings.is_empty() {
        return Err(failed(format!(
            "The extension \"{id}\" started with warnings:\n  {}",
            status.warnings.join("\n  ")
        )));
    }
    if found != expect {
        return Err(failed(format!(
            "The extension \"{id}\" contributed {found:?}, not {expect:?}."
        )));
    }

    Ok(ConformanceReport { status, found })
}
