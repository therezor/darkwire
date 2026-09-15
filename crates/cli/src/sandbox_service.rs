//! The sandbox service, started inside `ghostai serve` when none is deployed.
//!
//! The app never holds a container engine handle — that separation is the whole
//! point of the service — but a single-binary install still has to be able to
//! run a containerised agent. So `serve` starts the service as a task of its
//! own on a socket under the install root, and talks to it exactly as it would
//! talk to one deployed beside it. One code path, one set of approval checks,
//! and the engine still owned by one component.
//!
//! **Nothing is started when the operator has deployed a service themselves.**
//! `GHOSTAI_SANDBOX_SOCKET` naming one, or a socket already listening at the
//! default path, means this returns without binding: two services over one
//! state directory would both try to reap the other's containers, and the lock
//! the service takes would refuse the second anyway.
//!
//! The workspace registration is generated rather than configured, and that is
//! the one place this differs from a deployed service. That allow-list exists
//! to bound a *remote* app: it is what stops an app in another trust domain
//! naming a container the operator never meant it to reach. An embedded service
//! is the same operator, the same process tree and the same policy directory,
//! so it registers every workspace in the registry against every installed
//! definition — and the approval each one still needs is unchanged.
//!
//! A workspace created *after* boot is not reachable from a container until
//! `serve` restarts, because the registration is read once. That is stated in
//! `docs/sandbox-service.md` rather than solved with a reload path, because a
//! deployed service has the same property and configuring one is the answer for
//! an install that adds workspaces while running.

use std::collections::BTreeMap;
use std::sync::Arc;

use ghostai_core::paths::workspace_dir_for;
use ghostai_core::workspace_store::WorkspaceStore;
use ghostai_core::{GhostPaths, Result};
use ghostai_environment::service::{ServiceConfig, WorkspaceRegistration, socket_path};
use ghostai_security::PolicyStore;

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
/// installed could ask for a container.
pub async fn start_embedded(
    paths: &GhostPaths,
    workspaces: &WorkspaceStore,
    env: &Env,
) -> Option<Arc<tokio::task::JoinHandle<Result<()>>>> {
    let socket = socket_path(env.get("GHOSTAI_SANDBOX_SOCKET"), paths);
    // A live socket is a service somebody else is running, whether they
    // configured one or left an earlier `serve` running. Connecting is the only
    // reliable test: a stale socket file outlives the process that bound it.
    if tokio::net::UnixStream::connect(&socket).await.is_ok() {
        tracing::info!(socket = %socket.display(), "using the sandbox service already listening");
        return None;
    }
    if env.get("GHOSTAI_SANDBOX_SOCKET").is_some() {
        // Named but not answering. Started here it would bind a path the
        // operator pointed elsewhere for a reason, so the refusal is left to
        // the first command that needs a container, where it can name the
        // socket that did not answer.
        return None;
    }

    let policies = PolicyStore::new(paths.policy_dir.clone());
    let toolboxes: Vec<String> = policies
        .list_toolboxes()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    let containers: Vec<String> = policies
        .list_containers()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    if containers.is_empty() {
        // Nothing could name a container, so binding a socket and holding a
        // maintenance timer open would buy nothing. An operator who installs
        // one afterwards restarts `serve`, which is what they would do to pick
        // up the approval anyway.
        return None;
    }

    let registrations = registrations(paths, workspaces, &toolboxes, &containers);
    let state_root = paths.root.join("sandbox");
    let config = ServiceConfig {
        socket: socket.clone(),
        policy_root: paths.policy_dir.clone(),
        state_root: state_root.clone(),
        // GhostAI is on the host here, so the daemon sees the same paths this
        // process does. A containerised GhostAI has to deploy the service
        // separately and map them explicitly.
        daemon_state_root: state_root,
        engine: env
            .get("GHOSTAI_CONTAINER_ENGINE")
            .unwrap_or("docker")
            .to_owned(),
        gateway_image: env
            .get("GHOSTAI_GATEWAY_IMAGE")
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        workspaces: registrations,
    };
    let task = Arc::new(tokio::spawn(async move {
        // A failure here is not a boot failure. Everything that does not need a
        // container keeps working, and the first command that does gets a
        // sentence naming the socket that was not there.
        if let Err(error) = ghostai_environment::service::serve(config).await {
            tracing::warn!(error = %error.message, "the embedded sandbox service stopped");
            return Err(error);
        }
        Ok(())
    }));
    await_socket(&socket).await;
    Some(task)
}

/// Every registered workspace, each reachable by the same definitions.
///
/// A workspace whose id no longer resolves to a directory is skipped rather
/// than failing the boot: the registry outlives a directory somebody deleted by
/// hand, and refusing to start the server over one is a worse answer than
/// refusing the turn that names it.
fn registrations(
    paths: &GhostPaths,
    workspaces: &WorkspaceStore,
    toolboxes: &[String],
    containers: &[String],
) -> BTreeMap<String, WorkspaceRegistration> {
    let records = workspaces.list().unwrap_or_else(|error| {
        // Registering nothing in silence would make every containerised turn
        // report "Workspace is not registered", which names the wrong problem.
        tracing::warn!(
            error = %error.message,
            "could not read the workspace registry; no workspace can run a container"
        );
        Vec::new()
    });
    let mut map = BTreeMap::new();
    for record in records {
        let Ok(path) = workspace_dir_for(paths, &record.id) else {
            continue;
        };
        map.insert(
            record.id,
            WorkspaceRegistration {
                daemon_path: path.clone(),
                path,
                toolboxes: toolboxes.to_vec(),
                containers: containers.to_vec(),
            },
        );
    }
    map
}

/// Polls until the spawned service answers, or the bound elapses.
///
/// Elapsing is not an error here. The service logs its own failure, and the
/// first request that needs it reports the socket by name — waiting longer at
/// boot would only delay a server that works without containers.
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
