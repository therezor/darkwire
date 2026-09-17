//! The sandbox service, started inside `darkwire serve` when none is deployed.
//!
//! The app never holds a container engine handle — that separation is the whole
//! point of the service — but a single-binary install still has to be able to
//! run a containerised agent. So `serve` starts the service as a task of its
//! own on a socket under the install root, and talks to it exactly as it would
//! talk to one deployed beside it. One code path, one set of approval checks,
//! and the engine still owned by one component.
//!
//! **Nothing is started when the operator has deployed a service themselves.**
//! `DARKWIRE_SANDBOX_SOCKET` naming one, or a socket already listening at the
//! default path, means this returns without binding: two services over one
//! state directory would both try to reap the other's containers, and the lock
//! the service takes would refuse the second anyway.
//!
//! The workspace registration is generated rather than configured, and that is
//! the one place this differs from a deployed service. That allow-list exists
//! to bound a *remote* app: it is what stops an app in another trust domain
//! naming an environment the operator never meant it to reach. An embedded
//! service is the same operator, the same process tree and the same policy directory,
//! so it registers every workspace in the registry against every installed
//! definition — and the approval each one still needs is unchanged.
//!
//! **It is answered per request rather than read at boot.** Deriving it once
//! meant a workspace or an environment created while the server ran could not
//! be used until `serve` restarted, which is a confusing way to learn that a
//! definition you just saved is "not authorized". A deployed service keeps the
//! configured map it was given; only this generated one moves.
//!
//! For the same reason the service starts with no environment installed at all.
//! It costs nothing, because the engine is probed on first use rather than at
//! boot, and it is what lets the Environments screen resolve an image digest
//! before there is an environment to put it in.

use std::collections::BTreeMap;
use std::sync::Arc;

use darkwire_core::paths::{ensure_dir, workspace_dir_for};
use darkwire_core::workspace_store::WorkspaceStore;
use darkwire_core::{Result, WirePaths};
use darkwire_environment::service::{
    ServiceConfig, WorkspaceLookup, WorkspaceRegistration, serve_with, socket_path,
};
use darkwire_security::PolicyStore;

use crate::i18n::Env;

/// How long to wait for the spawned service to answer on its socket.
///
/// `serve()` binds asynchronously, so returning the moment the task is spawned
/// would race the first turn. This is a bound on the wait, not an expected
/// duration: binding a Unix socket takes microseconds.
const BIND_TIMEOUT_MS: u64 = 2_000;

/// Starts the service unless one is already reachable.
///
/// Returns the task that owns it, which the caller keeps alive for as long as
/// the server runs. `None` means somebody else owns the socket, or nothing
/// installed could ask for an environment.
pub async fn start_embedded(
    paths: &WirePaths,
    workspaces: &Arc<WorkspaceStore>,
    env: &Env,
) -> Option<Arc<tokio::task::JoinHandle<Result<()>>>> {
    let socket = socket_path(env.get("DARKWIRE_SANDBOX_SOCKET"), paths);
    // A live socket is a service somebody else is running, whether they
    // configured one or left an earlier `serve` running. Connecting is the only
    // reliable test: a stale socket file outlives the process that bound it.
    if tokio::net::UnixStream::connect(&socket).await.is_ok() {
        tracing::info!(socket = %socket.display(), "using the sandbox service already listening");
        return None;
    }
    if env.get("DARKWIRE_SANDBOX_SOCKET").is_some() {
        // Named but not answering. Started here it would bind a path the
        // operator pointed elsewhere for a reason, so the refusal is left to
        // the first command that needs an environment, where it can name the
        // socket that did not answer.
        return None;
    }

    // The service canonicalises its policy root, so the directory has to exist
    // before it binds. It usually does, because installing a definition creates
    // it, and this used to return early on an install that had none at all. It
    // no longer does, which makes an install that has never had an environment
    // the ordinary case rather than one that never reached here.
    //
    // Created here rather than in `serve`, because only the embedded service
    // owns this directory. A deployed one is handed a mount, and a missing
    // mount is a misconfiguration it should refuse rather than paper over.
    if let Err(error) = ensure_dir(&paths.policy_dir) {
        tracing::warn!(
            path = %paths.policy_dir.display(),
            error = %error.message,
            "could not create the policy directory; no container can start"
        );
        return None;
    }

    let state_root = paths.root.join("sandbox");
    let config = ServiceConfig {
        socket: socket.clone(),
        policy_root: paths.policy_dir.clone(),
        state_root: state_root.clone(),
        // DarkWire is on the host here, so the daemon sees the same paths this
        // process does. A containerised DarkWire has to deploy the service
        // separately and map them explicitly.
        daemon_state_root: state_root,
        engine: env
            .get("DARKWIRE_CONTAINER_ENGINE")
            .unwrap_or("docker")
            .to_owned(),
        gateway_image: env
            .get("DARKWIRE_GATEWAY_IMAGE")
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        // Empty, and answered by the lookup below instead. A deployed service
        // fills this from its own config file; here it would be a snapshot that
        // goes stale the moment a workspace or an environment is added.
        workspaces: BTreeMap::new(),
    };
    let lookup = Arc::new(DerivedRegistrations {
        paths: paths.clone(),
        workspaces: Arc::clone(workspaces),
        policies: PolicyStore::new(paths.policy_dir.clone()),
    });
    let task = Arc::new(tokio::spawn(async move {
        // A failure here is not a boot failure. Everything that does not need an
        // environment keeps working, and the first command that does gets a
        // sentence naming the socket that was not there.
        if let Err(error) = serve_with(config, Some(lookup)).await {
            tracing::warn!(error = %error.message, "the embedded sandbox service stopped");
            return Err(error);
        }
        Ok(())
    }));
    await_socket(&socket).await;
    Some(task)
}

/// Every registered workspace, each reachable by every installed definition.
///
/// Resolved per request rather than snapshotted, so a workspace created or an
/// environment installed while `serve` runs is usable without a restart. The
/// service canonicalises and overlap-checks whatever this returns, so the
/// answer here is a claim rather than a grant.
struct DerivedRegistrations {
    paths: WirePaths,
    workspaces: Arc<WorkspaceStore>,
    policies: PolicyStore,
}

impl WorkspaceLookup for DerivedRegistrations {
    fn resolve(&self, workspace: &str) -> Option<WorkspaceRegistration> {
        // Through the registry rather than straight to a directory: an id that
        // names no workspace must not become one by being asked for.
        let known = self
            .workspaces
            .list()
            .unwrap_or_else(|error| {
                // Answering "not registered" in silence would name the wrong
                // problem for every containerised turn.
                tracing::warn!(
                    error = %error.message,
                    "could not read the workspace registry; no workspace can run an environment"
                );
                Vec::new()
            })
            .into_iter()
            .any(|record| record.id == workspace);
        if !known {
            return None;
        }
        let path = workspace_dir_for(&self.paths, workspace).ok()?;
        Some(WorkspaceRegistration {
            daemon_path: path.clone(),
            path,
            // DarkWire is on the host here, so every installed definition is
            // reachable from every workspace. The allow-list bounds a remote
            // app, and there is not one.
            environments: self
                .policies
                .list_environments()
                .into_iter()
                .map(|entry| entry.name)
                .collect(),
        })
    }
}

/// Polls until the spawned service answers, or the bound elapses.
///
/// Elapsing is not an error here. The service logs its own failure, and the
/// first request that needs it reports the socket by name — waiting longer at
/// boot would only delay a server that works without environments.
async fn await_socket(socket: &std::path::Path) {
    let deadline = std::time::Duration::from_millis(BIND_TIMEOUT_MS);
    let poll = tokio::time::Duration::from_millis(10);
    let _ = tokio::time::timeout(deadline, async {
        loop {
            if tokio::net::UnixStream::connect(socket).await.is_ok() {
                return;
            }
            tokio::time::sleep(poll).await;
        }
    })
    .await;
}
