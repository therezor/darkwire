//! Sandbox service entry point; configuration is supplied by the operator.
#[tokio::main]
async fn main() {
    let result = async {
        if std::env::args().nth(1).as_deref() == Some("proxy") {
            ghostai_environment::proxy::serve(std::env::args().skip(2).collect()).await?;
            return Ok(());
        }
        if std::env::args().nth(1).as_deref() == Some("serve-env") {
            use ghostai_environment::service::{ServiceConfig, WorkspaceRegistration};
            let host = std::path::PathBuf::from(std::env::var("GHOSTAI_DATA_DIR")?);
            if !host.is_absolute() {
                return Err("GHOSTAI_DATA_DIR must be absolute".into());
            }
            ghostai_environment::service::serve(ServiceConfig {
                socket: "/run/ghostai/sandbox.sock".into(),
                policy_root: "/policies".into(),
                state_root: "/sandbox-state".into(),
                daemon_state_root: host.join("sandbox-state"),
                engine: "docker".into(),
                gateway_image: std::env::var("GHOSTAI_GATEWAY_IMAGE")
                    .ok()
                    .filter(|s| !s.is_empty()),
                workspaces: std::collections::BTreeMap::from([(
                    "default".into(),
                    WorkspaceRegistration {
                        path: "/workspaces/default".into(),
                        daemon_path: host.join("workspaces/default"),
                        toolboxes: std::env::var("GHOSTAI_SANDBOX_TOOLBOXES")
                            .unwrap_or_else(|_| "coding,review".into())
                            .split(',')
                            .map(str::trim)
                            .filter(|name| !name.is_empty())
                            .map(str::to_owned)
                            .collect(),
                        containers: std::env::var("GHOSTAI_SANDBOX_CONTAINERS")
                            .unwrap_or_else(|_| "dev".into())
                            .split(',')
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_owned)
                            .collect(),
                    },
                )]),
            })
            .await?;
            return Ok(());
        }
        let path = std::env::args()
            .nth(1)
            .ok_or("Usage: ghostai-environment /path/to/service.json")?;
        let config = serde_json::from_slice(&std::fs::read(path)?)?;
        ghostai_environment::service::serve(config).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
