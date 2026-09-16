//! The socket boundary: what a client may ask for, and what it is refused.
//!
//! Over a real Unix socket with a real `serve` behind it, and no container
//! daemon anywhere. That is not a limitation — every check under test happens
//! *before* the engine is touched, and a suite that needed Docker to assert a
//! refusal would be a suite CI could not run. The one end-to-end path that does
//! need a daemon lives in `docker_engine.rs`, behind `#[ignore]`.
//!
//! What is asserted here is the half of the contract the app cannot enforce for
//! itself: the service owns the engine, so it re-checks the workspace
//! registration and container definition against its own policy directory
//! rather than trusting what the caller says it resolved.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ghostai_environment::service::{SandboxClient, ServiceConfig, WorkspaceRegistration, serve};
use ghostai_protocol::rest::SandboxRequest;
use ghostai_protocol::{EnvironmentNetwork, NetworkMode};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use common::write;

const DIGEST: &str = "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// A service on its own socket, with one workspace and one container.
struct Harness {
    #[expect(dead_code, reason = "held so the directory outlives the service")]
    dir: tempfile::TempDir,
    root: PathBuf,
    client: SandboxClient,
    token: CancellationToken,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

fn container() -> Value {
    json!({"schema": "ghostai.environment/1", "name": "dev", "image": DIGEST})
}

impl Harness {
    async fn start() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let policy = root.join("policy");
        write(
            &policy.join("environments/dev.yaml"),
            container().to_string(),
        );
        std::fs::create_dir_all(root.join("workspaces/default")).unwrap();

        let socket = root.join("sandbox.sock");
        let token = CancellationToken::new();
        let state_root = root.join("state");
        let config = ServiceConfig {
            socket: socket.clone(),
            policy_root: policy,
            state_root: state_root.clone(),
            daemon_state_root: state_root,
            // A binary that is not there. Every refusal under test is decided
            // before anything would run, so the engine never has to exist —
            // and a test that reached one would fail loudly rather than
            // silently start a container on the machine running the suite.
            engine: root.join("no-such-engine").to_string_lossy().into_owned(),
            gateway_image: None,
            workspaces: BTreeMap::from([(
                "default".to_owned(),
                WorkspaceRegistration {
                    path: root.join("workspaces/default"),
                    daemon_path: root.join("workspaces/default"),
                    environments: vec!["dev".to_owned()],
                },
            )]),
        };
        let stop = token.clone();
        tokio::spawn(async move {
            tokio::select! {
                result = serve(config) => {
                    if let Err(error) = result {
                        eprintln!("the service stopped: {}", error.message);
                    }
                }
                () = stop.cancelled() => {}
            }
        });
        assert!(
            common::eventually(Duration::from_secs(5), || socket.exists()).await,
            "the service never bound its socket"
        );
        Harness {
            dir,
            root,
            client: SandboxClient::new(socket),
            token,
        }
    }

    async fn ask(&self, request: SandboxRequest) -> ghostai_core::Result<Value> {
        self.client
            .request(request, &CancellationToken::new())
            .await
    }
}

fn no_network() -> EnvironmentNetwork {
    EnvironmentNetwork::default()
}

fn message(result: ghostai_core::Result<Value>) -> String {
    match result {
        Ok(value) => panic!("expected a refusal, got {value}"),
        Err(error) => error.message,
    }
}

#[tokio::test]
async fn answers_a_health_check_without_a_daemon_behind_it() {
    let harness = Harness::start().await;
    // The service is up whether or not the engine is: an operator asking
    // "is it there" gets an answer that distinguishes the two.
    let health = harness.ask(SandboxRequest::Health).await.unwrap();
    assert_eq!(health["status"], json!("degraded"));
    assert_eq!(health["engineReady"], json!(false));
    assert!(
        health["engineError"]
            .as_str()
            .is_some_and(|e| !e.is_empty()),
        "a degraded answer names why: {health}"
    );
    // Degraded, not down: the service answered, which is the distinction an
    // operator needs before they go looking at the wrong process.
    assert_eq!(health["version"], json!(1));
}

#[tokio::test]
async fn lists_nothing_when_no_instance_has_been_started() {
    let harness = Harness::start().await;
    let listed = harness.ask(SandboxRequest::List).await.unwrap();
    assert_eq!(listed["instances"], json!([]));
}

#[tokio::test]
async fn refuses_a_workspace_it_was_never_told_about() {
    // The registration is the service's own, not the caller's claim. An app in
    // another trust domain naming a workspace the operator did not register
    // gets nothing, whatever its own config says.
    let harness = Harness::start().await;
    let refusal = harness
        .ask(SandboxRequest::Start {
            environment: "dev".to_owned(),
            workspace: "elsewhere".to_owned(),
            agent: "scanner".to_owned(),
            session: "s1".to_owned(),
            network: no_network(),
        })
        .await;
    assert!(message(refusal).contains("not registered"));
}

#[tokio::test]
async fn refuses_a_container_this_workspace_was_not_given() {
    let harness = Harness::start().await;
    write(
        &harness.root.join("policy/environments/other.yaml"),
        json!({"schema": "ghostai.environment/1", "name": "other", "image": DIGEST}).to_string(),
    );

    let refusal = harness
        .ask(SandboxRequest::Start {
            environment: "other".to_owned(),
            workspace: "default".to_owned(),
            agent: "scanner".to_owned(),
            session: "s1".to_owned(),
            network: no_network(),
        })
        .await;
    assert!(message(refusal).contains("not authorized"));
}

#[tokio::test]
async fn refuses_an_environment_whose_definition_was_removed() {
    let harness = Harness::start().await;
    std::fs::remove_file(harness.root.join("policy/environments/dev.yaml")).unwrap();
    let refusal = harness
        .ask(SandboxRequest::Start {
            environment: "dev".to_owned(),
            workspace: "default".to_owned(),
            agent: "scanner".to_owned(),
            session: "s1".to_owned(),
            network: no_network(),
        })
        .await;
    assert!(message(refusal).contains("No environment is installed"));
}

#[tokio::test]
async fn refuses_an_egress_request_nothing_could_enforce() {
    // The same coherence check the app makes on a save, made again here:
    // the service owns the gateway, so it decides whether a request is
    // enforceable rather than trusting that somebody checked.
    let harness = Harness::start().await;
    let mut network = no_network();
    network.mode = NetworkMode::Allowlist;
    let refusal = harness
        .ask(SandboxRequest::Start {
            environment: "dev".to_owned(),
            workspace: "default".to_owned(),
            agent: "scanner".to_owned(),
            session: "s1".to_owned(),
            network,
        })
        .await;
    assert!(message(refusal).contains("reaches nothing"));
}

#[tokio::test]
async fn refuses_to_stop_an_instance_that_does_not_exist() {
    let harness = Harness::start().await;
    let refusal = harness
        .ask(SandboxRequest::Stop {
            instance: "ghost-sbx-nope".to_owned(),
            force: false,
        })
        .await;
    assert!(message(refusal).contains("Unknown managed container"));

    let restart = harness
        .ask(SandboxRequest::Restart {
            instance: "ghost-sbx-nope".to_owned(),
            force: false,
        })
        .await;
    assert!(message(restart).contains("Unknown managed container"));
}

#[tokio::test]
async fn refuses_a_service_config_whose_roots_overlap() {
    // Policy under state, or a workspace under either, would let a client with
    // a write anywhere in its own workspace rewrite the policy it runs under.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let config = ServiceConfig {
        socket: root.join("sandbox.sock"),
        policy_root: root.join("state/policy"),
        state_root: root.join("state"),
        daemon_state_root: root.join("state"),
        engine: "docker".to_owned(),
        gateway_image: None,
        workspaces: BTreeMap::new(),
    };
    let refusal = serve(config).await;
    assert!(refusal.is_err(), "overlapping roots must not start");
}

#[tokio::test]
async fn refuses_a_relative_path_in_its_own_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let config = ServiceConfig {
        socket: dir.path().join("sandbox.sock"),
        policy_root: PathBuf::from("policy"),
        state_root: dir.path().join("state"),
        daemon_state_root: dir.path().join("state"),
        engine: "docker".to_owned(),
        gateway_image: None,
        workspaces: BTreeMap::new(),
    };
    assert!(serve(config).await.is_err());
}

#[tokio::test]
async fn a_socket_path_that_is_not_a_socket_is_left_alone() {
    // Replacing an arbitrary path would make a misconfigured `socket` delete an
    // operator's file.
    let dir = tempfile::tempdir().unwrap();
    let occupied = dir.path().join("not-a-socket");
    std::fs::write(&occupied, b"mine").unwrap();
    let state = dir.path().join("state");
    let config = ServiceConfig {
        socket: occupied.clone(),
        policy_root: dir.path().join("policy"),
        state_root: state.clone(),
        daemon_state_root: state,
        engine: "docker".to_owned(),
        gateway_image: None,
        workspaces: BTreeMap::new(),
    };
    assert!(serve(config).await.is_err());
    assert_eq!(std::fs::read(&occupied).unwrap(), b"mine");
}

/// The socket is the only way in, and it is bounded.
#[tokio::test]
async fn refuses_a_frame_that_is_not_a_versioned_request() {
    let harness = Harness::start().await;
    let socket = harness.root.join("sandbox.sock");
    for frame in [
        json!({"version": 1}),
        json!({"version": 99, "request": {"op": "health"}}),
        json!({"version": 1, "request": {"op": "nonsense"}}),
        json!({"version": 1, "request": {"op": "health"}, "extra": true}),
    ] {
        assert!(send_raw(&socket, &frame).await.is_err(), "accepted {frame}");
    }
    // Still serving afterwards: a bad frame closes its own connection, not the
    // service.
    assert!(harness.ask(SandboxRequest::Health).await.is_ok());
}

/// Writes one length-framed JSON value and reads the answer, bypassing the
/// typed client so a malformed frame can be sent at all.
async fn send_raw(socket: &Path, frame: &Value) -> Result<Value, String> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(frame).map_err(|e| e.to_string())?;
    stream
        .write_all(&u32::try_from(bytes.len()).unwrap().to_be_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stream.write_all(&bytes).await.map_err(|e| e.to_string())?;
    let mut length = [0u8; 4];
    stream
        .read_exact(&mut length)
        .await
        .map_err(|e| e.to_string())?;
    let mut body = vec![0u8; u32::from_be_bytes(length) as usize];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    if value.get("error").is_some() {
        return Err(value.to_string());
    }
    Ok(value)
}
