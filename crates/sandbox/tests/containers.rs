//! Real-daemon smoke test for the service boundary and shared container lifecycle.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_protocol::{ContainerNetwork, SandboxRequest};
use ghostai_sandbox::service::{SandboxClient, ServiceConfig, WorkspaceRegistration};
use ghostai_security::PolicyStore;
use serde_json::json;
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    process::{Child, Command},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct ServiceProcess(Child);
impl Drop for ServiceProcess {
    fn drop(&mut self) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.0.id().cast_signed()),
            nix::sys::signal::Signal::SIGTERM,
        );
        let _ = self.0.wait();
    }
}

#[tokio::test]
#[ignore = "builds a small tool image and requires a running Docker daemon"]
async fn container_service_shares_instances_and_enforces_toolbox_grants() {
    let image = build_tool_image();
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let store = install_policy(&root_path, &image);
    let writer = store.require_toolbox("writer").unwrap().digest().to_owned();
    let reader = store.require_toolbox("reader").unwrap().digest().to_owned();
    let intruder = store
        .require_toolbox("intruder")
        .unwrap()
        .digest()
        .to_owned();
    let token = CancellationToken::new();
    let (_service, client) = start_service(&root_path, &token).await;
    let execute =
        |toolbox: &str, digest: &str, operation: &str, agent: &str| SandboxRequest::Execute {
            toolbox: toolbox.into(),
            digest: digest.into(),
            container: "shared".into(),
            operation: operation.into(),
            workspace: "default".into(),
            agent: agent.into(),
            session: agent.into(),
            network: ContainerNetwork::default(),
            args: json!({}),
        };
    let first = client
        .request(execute("writer", &writer, "write", "alice"), &token)
        .await
        .unwrap();
    assert_eq!(first["isError"], false, "{first}");
    assert!(first["content"].as_str().unwrap().contains("shared"));
    assert!(
        client
            .request(execute("intruder", &intruder, "read", "mallory"), &token)
            .await
            .is_err()
    );
    let second = client
        .request(execute("reader", &reader, "read", "bob"), &token)
        .await
        .unwrap();
    assert_eq!(second["isError"], false, "{second}");
    assert!(
        client
            .request(execute("reader", &reader, "write", "bob"), &token)
            .await
            .is_err()
    );
    let listing = client.request(SandboxRequest::List, &token).await.unwrap();
    assert_eq!(
        listing["instances"].as_array().unwrap().len(),
        1,
        "{listing}"
    );
    let id = listing["instances"][0]["id"].as_str().unwrap().to_owned();
    let cancellation = CancellationToken::new();
    let timer = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(700)).await;
        timer.cancel();
    });
    assert!(
        client
            .request(execute("writer", &writer, "wait", "alice"), &cancellation)
            .await
            .is_err()
    );
    let recovered = tokio::time::timeout(
        Duration::from_secs(15),
        client.request(execute("reader", &reader, "read", "bob"), &token),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(recovered["isError"], false, "{recovered}");
    std::fs::remove_file(root_path.join("toolboxes/writer.yaml")).unwrap();
    assert!(
        client
            .request(execute("writer", &writer, "write", "alice"), &token)
            .await
            .is_err()
    );
    client
        .request(
            SandboxRequest::Stop {
                instance: id,
                force: false,
            },
            &token,
        )
        .await
        .unwrap();
    assert!(
        client.request(SandboxRequest::List, &token).await.unwrap()["instances"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

/// Builds the tiny tool image the smoke test runs in, and answers its id.
///
/// By id, never by tag: a container definition that named a tag would be
/// repointable after approval, which is the gate this whole service exists to
/// keep shut.
fn build_tool_image() -> String {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    eprintln!("Building the isolated smoke-test tool image");
    let output = Command::new("docker")
        .current_dir(&repo)
        .args([
            "build",
            "--target",
            "tools",
            "-q",
            "-f",
            "deploy/sandbox/Dockerfile",
            ".",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let image = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    assert!(image.starts_with("sha256:"), "{image}");
    image
}

/// Lays out one policy directory: three toolboxes over three operations, and
/// one shared container. The toolboxes are left unapproved, so the test decides
/// which approval hash each call is prepared at.
fn install_policy(root: &std::path::Path, image: &str) -> PolicyStore {
    for folder in [
        "toolboxes",
        "tool-definitions",
        "containers",
        "workspace",
        "state",
    ] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
    }
    std::fs::set_permissions(
        root.join("workspace"),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    std::fs::write(root.join("containers/shared.yaml"), json!({"schema":"ghostai.container/1","name":"shared","image":image,"shared":true,"security":{"tmpfs":["/tmp:rw,nosuid,size=16m"]}}).to_string()).unwrap();
    for (name, implementation) in [
        (
            "write",
            json!({"kind":"command","executable":"/bin/sh","argv":["-c","printf shared > shared.txt; cat shared.txt"]}),
        ),
        (
            "read",
            json!({"kind":"command","executable":"/bin/cat","argv":["shared.txt"]}),
        ),
        (
            "wait",
            json!({"kind":"command","executable":"/bin/sh","argv":["-c","trap '' TERM; sleep 60 & wait"]}),
        ),
    ] {
        std::fs::write(root.join("tool-definitions").join(format!("{name}.yaml")), json!({"schema":"ghostai.tool/1","description":name,"parameters":{"type":"object","properties":{},"additionalProperties":false},"implementation":implementation}).to_string()).unwrap();
    }
    for (name, operations) in [
        ("writer", vec!["write", "wait"]),
        ("reader", vec!["read"]),
        ("intruder", vec!["read"]),
    ] {
        let tools: Vec<_> = operations
            .into_iter()
            .map(|name| json!({"name":name,"definition":name,"permission":"allow"}))
            .collect();
        std::fs::write(
            root.join("toolboxes").join(format!("{name}.yaml")),
            json!({"schema":"ghostai.toolbox/1","name":name,"tools":tools}).to_string(),
        )
        .unwrap();
    }
    PolicyStore::new(root.to_path_buf())
}

/// Spawns the service over `root` as its own process and waits for it to
/// answer, which is what proves the socket is the boundary: the test holds no
/// engine handle and reaches everything through the client.
async fn start_service(
    root: &std::path::Path,
    token: &CancellationToken,
) -> (ServiceProcess, SandboxClient) {
    let config = ServiceConfig {
        socket: root.join("service.sock"),
        policy_root: root.to_path_buf(),
        state_root: root.join("state"),
        daemon_state_root: root.join("state"),
        engine: "docker".into(),
        gateway_image: None,
        workspaces: BTreeMap::from([(
            "default".into(),
            WorkspaceRegistration {
                path: root.join("workspace"),
                daemon_path: root.join("workspace"),
                toolboxes: vec!["writer".into(), "reader".into()],
                containers: vec!["shared".into()],
            },
        )]),
    };
    let config_path = root.join("service.yaml");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let service = ServiceProcess(
        Command::new(env!("CARGO_BIN_EXE_ghostai-sandbox"))
            .arg(&config_path)
            .spawn()
            .unwrap(),
    );
    let client = SandboxClient::new(&config.socket);
    for _ in 0..100 {
        if client.request(SandboxRequest::Health, token).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (service, client)
}
