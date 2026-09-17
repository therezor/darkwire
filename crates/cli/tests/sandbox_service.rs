//! The service `darkwire serve` starts for itself.
//!
//! One property, and it is the one a single-binary install depends on: the
//! socket is there by the time `serve` returns, on a machine that has never had
//! an environment.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;

use darkwire::i18n::Env;
use darkwire::sandbox_service::start_embedded;
use darkwire_core::clock::SystemClock;
use darkwire_core::db::Database;
use darkwire_core::paths::{ResolveWirePaths, WirePaths};
use darkwire_core::workspace_store::WorkspaceStore;

fn paths_in(root: &std::path::Path) -> WirePaths {
    let paths = WirePaths::resolve(ResolveWirePaths {
        root: Some(root.to_string_lossy().into_owned()),
        home: Some(root.to_path_buf()),
        ..ResolveWirePaths::default()
    })
    .unwrap();
    std::fs::create_dir_all(&paths.workspaces_dir).unwrap();
    paths
}

/// The regression: an install that has never installed an environment.
///
/// The service canonicalises its policy root, and that directory is created by
/// installing a definition. This used to return early on an install with none,
/// so `serve` never ran and never hit it; it starts unconditionally now, which
/// makes "never had an environment" the ordinary case rather than one that
/// could not reach here. Without the directory the service refused to bind, and
/// every container request reported a socket that was not there.
#[tokio::test]
async fn it_binds_on_an_install_that_has_no_policy_directory() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths_in(root.path());
    assert!(
        !paths.policy_dir.exists(),
        "the fixture has to start without one"
    );

    let workspaces = Arc::new(
        WorkspaceStore::new(
            Database::in_memory().unwrap(),
            paths.clone(),
            Arc::new(SystemClock),
        )
        .unwrap(),
    );
    let env = Env::empty();

    let task = start_embedded(&paths, &workspaces, &env)
        .await
        .expect("the service to be started");

    let socket = paths.root.join("control/sandbox.sock");
    assert!(
        tokio::net::UnixStream::connect(&socket).await.is_ok(),
        "nothing answered on {}",
        socket.display()
    );
    task.abort();
}
