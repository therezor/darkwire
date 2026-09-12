//! One real host over a temporary install root, for the tests that spawn.
//!
//! Every fixture is *copied* into the root rather than approved where it lies,
//! for the reason the testkit gives: the approval digest covers every byte
//! under an install, so a suite that approved the checkout would record a digest
//! that moves the moment anything writes beside it. Copying also makes "edit a
//! file and watch the row go `drifted`" expressible, which is the one assertion
//! that cannot be faked.

#![allow(
    dead_code,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a shared harness is used by some of its consumers, and a fixture that will not load is a failing test either way"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ghostai_core::{Database, SystemClock};
use ghostai_extension_host::{ExtensionHost, ExtensionHostOptions, Timings};
use ghostai_protocol::{ExtensionState, ExtensionStatus, ExtensionsConfig};
use ghostai_security::ExtensionStore;
use tempfile::TempDir;

/// Deadlines short enough that the suite runs in under a second.
///
/// Real time, not paused: a paused clock advances the instant every task is
/// idle on I/O, and a child process is exactly that for most of its life — the
/// handshake cap would fire before Node had finished booting.
pub fn quick() -> Timings {
    Timings {
        init_timeout: Duration::from_secs(10),
        kill_grace: Duration::from_millis(200),
        respawn_delay: Duration::from_millis(100),
    }
}

/// A host over a temporary root, with fixtures copied in.
pub struct Harness {
    pub host: ExtensionHost,
    pub store: ExtensionStore,
    pub root: PathBuf,
    _temp: TempDir,
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn copy_tree(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        let into = target.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &into);
        } else if kind.is_file() {
            std::fs::copy(entry.path(), into).unwrap();
        }
    }
}

impl Harness {
    /// A host with these fixtures installed and none of them approved.
    pub fn with(ids: &[&str]) -> Harness {
        Harness::from_paths(
            &ids.iter()
                .map(|id| (*id, fixtures().join(id)))
                .collect::<Vec<_>>(),
        )
    }

    /// A host with the in-tree example installed under its own id.
    pub fn with_example() -> Harness {
        let example =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/hello-extension");
        Harness::from_paths(&[("hello", example)])
    }

    fn from_paths(installs: &[(&str, PathBuf)]) -> Harness {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let extensions = root.join("extensions");
        std::fs::create_dir_all(&extensions).unwrap();

        for (id, source) in installs {
            let target = extensions.join(id);
            std::fs::create_dir_all(&target).unwrap();
            for entry in std::fs::read_dir(source).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name();
                let name = name.to_string_lossy().into_owned();
                // What a checkout carries and a shipped extension does not.
                if matches!(name.as_str(), "node_modules" | "test" | ".turbo")
                    || name.ends_with(".tsbuildinfo")
                {
                    continue;
                }
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    copy_tree(&entry.path(), &target.join(&name));
                } else if kind.is_file() {
                    std::fs::copy(entry.path(), target.join(&name)).unwrap();
                }
            }
        }

        let store = ExtensionStore::new(
            Database::in_memory().unwrap(),
            &extensions,
            Arc::new(SystemClock),
        )
        .unwrap();
        let host = ExtensionHost::new(
            ExtensionHostOptions::new(store.clone(), &root).with_timings(quick()),
        );
        Harness {
            host,
            store,
            root,
            _temp: temp,
        }
    }

    /// The install directory of one fixture, for editing a byte of it.
    pub fn install(&self, id: &str) -> PathBuf {
        self.root.join("extensions").join(id)
    }

    /// Approves what is on disk now.
    pub fn approve(&self, id: &str) {
        self.store.approve(id).unwrap();
    }

    /// Reconciles and waits for every start to land.
    pub async fn settle(&self, config: &ExtensionsConfig) {
        self.host.reconcile(config);
        assert!(
            self.host.quiesce(Duration::from_secs(30)).await,
            "the host did not finish starting its extensions"
        );
    }

    /// Reconciles with the default config and waits.
    pub async fn settle_default(&self) {
        self.settle(&ExtensionsConfig::default()).await;
    }

    /// One extension's row.
    pub fn row(&self, id: &str) -> ExtensionStatus {
        self.host
            .status()
            .into_iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("no row for {id}"))
    }

    /// One extension's state.
    pub fn state(&self, id: &str) -> ExtensionState {
        self.row(id).state
    }
}

/// Waits until `check` holds, or gives up after `timeout`.
///
/// For the assertions that follow an event the host reports on its own — a
/// crash, a restart — where there is nothing to await.
pub async fn eventually(timeout: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    check()
}
