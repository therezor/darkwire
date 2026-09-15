//! Versioned, bounded Unix-socket protocol for granted container operations.
use crate::container_pool::{
    ContainerPool, ContainerPoolOptions, DockerEngineOptions, docker_engine,
};
use ghostai_core::{ErrorKind, GhostError, Result, SystemClock};
use ghostai_protocol::toolbox::{OperationImplementation, ToolOperation};
use ghostai_protocol::{ContainerNetwork, SandboxRequest, ToolPermission};
use ghostai_security::toolbox::invalid;
use ghostai_security::{
    ExecGuardOptions, JailOptions, PolicyStore, WorkspaceJail, command_argv, guard_exec,
    validate_input,
};
use ghostai_tools::operations::OperationExecutor;
use ghostai_tools::{
    BoxFuture, OutputStream, OutputTee, PlacementRequest, RunRequest, ToolContext, ToolExecution,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::sync::CancellationToken;

const MAX_FRAME: usize = 4 * 1024 * 1024;

/// Operator-only service configuration. Never sourced from a workspace.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceConfig {
    /// Where the service listens. Its directory is the access boundary: the
    /// socket is group-readable and nothing else names it.
    pub socket: PathBuf,
    /// The policy directory both this service and the app read.
    pub policy_root: PathBuf,
    /// Lock file, installation identity and command transcripts.
    pub state_root: PathBuf,
    /// The same state directory as seen by the Docker daemon.
    pub daemon_state_root: PathBuf,
    /// Every workspace a client may name, and what it may reach from there. A
    /// workspace absent from here cannot be addressed at all.
    pub workspaces: BTreeMap<String, WorkspaceRegistration>,
    /// The container CLI. `podman` is wire-compatible for everything used here.
    #[serde(default = "docker")]
    pub engine: String,
    /// Digest-pinned image the egress gateway runs, without which restricted
    /// egress is refused rather than run wide open.
    #[serde(default)]
    pub gateway_image: Option<String>,
}
fn docker() -> String {
    "docker".into()
}

impl ServiceConfig {
    /// Resolve service-visible paths once, refusing workspace access to policy
    /// or control state. Daemon paths are explicit operator mappings, never
    /// accepted from a client request.
    fn validate_paths(&mut self) -> Result<()> {
        use std::path::{Component, Path};
        fn absolute(path: &Path) -> Result<()> {
            if !path.is_absolute()
                || path.parent().is_none()
                || path.components().any(|c| matches!(c, Component::ParentDir))
            {
                return Err(invalid(format!(
                    "Expected an absolute non-root path: {}",
                    path.display()
                )));
            }
            Ok(())
        }
        for path in [
            &self.socket,
            &self.policy_root,
            &self.state_root,
            &self.daemon_state_root,
        ] {
            absolute(path)?;
        }
        self.policy_root = self
            .policy_root
            .canonicalize()
            .map_err(|e| invalid(e.to_string()))?;
        self.state_root = self
            .state_root
            .canonicalize()
            .map_err(|e| invalid(e.to_string()))?;
        let socket_parent = self
            .socket
            .parent()
            .ok_or_else(|| invalid("Missing socket parent"))?
            .canonicalize()
            .map_err(|e| invalid(e.to_string()))?;
        self.socket = socket_parent.join(
            self.socket
                .file_name()
                .ok_or_else(|| invalid("Missing socket name"))?,
        );
        let mut protected = vec![self.state_root.clone(), self.socket.clone()];
        for directory in ["toolboxes", "tool-definitions", "containers"] {
            let path = self.policy_root.join(directory);
            protected.push(if path.exists() {
                path.canonicalize().map_err(|e| invalid(e.to_string()))?
            } else {
                path
            });
        }
        for registration in self.workspaces.values_mut() {
            absolute(&registration.path)?;
            absolute(&registration.daemon_path)?;
            registration.path = registration
                .path
                .canonicalize()
                .map_err(|e| invalid(e.to_string()))?;
            if !registration.path.is_dir()
                || protected.iter().any(|path| {
                    path.starts_with(&registration.path) || registration.path.starts_with(path)
                })
                || self
                    .daemon_state_root
                    .starts_with(&registration.daemon_path)
                || registration
                    .daemon_path
                    .starts_with(&self.daemon_state_root)
            {
                return Err(invalid(
                    "Workspace mounts must not overlap sandbox policy or control state",
                ));
            }
        }
        Ok(())
    }
}

/// One workspace a client may name, and the policy it may reach from there.
///
/// The app decides which toolbox and container an agent selects; this decides
/// whether they may be used in this workspace at all. Both have to agree, and
/// this is the half that owns the engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceRegistration {
    /// The workspace root as this service resolves it.
    pub path: PathBuf,
    /// The same directory as the daemon resolves it, which is not always the
    /// same string: a bind source is resolved by the daemon, not by this
    /// process.
    pub daemon_path: PathBuf,
    /// Toolboxes whose operations may be called here.
    pub toolboxes: Vec<String>,
    /// Containers that may be started here.
    #[serde(default)]
    pub containers: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u8,
    request: SandboxRequest,
}

/// Client has no engine handle and cannot submit shell commands or mount paths.
#[derive(Clone)]
pub struct SandboxClient {
    socket: PathBuf,
}

struct Progress(tokio::sync::mpsc::Sender<Value>);
impl OutputTee for Progress {
    fn write(&self, stream: OutputStream, chunk: &[u8]) {
        for chunk in chunk.chunks(4096) {
            let _ = self.0.try_send(json!({"event":"output", "stream": if stream == OutputStream::Stdout { "stdout" } else { "stderr" }, "text":String::from_utf8_lossy(chunk)}));
        }
    }
}
/// Where the service listens, given the environment and the install's paths.
///
/// One function rather than the same fallback written at each call site: the
/// app, the CLI and anything that later needs to reach the service have to
/// agree on the path, and three copies of a default is how they stop agreeing.
pub fn socket_path(env: Option<&str>, paths: &ghostai_core::GhostPaths) -> PathBuf {
    env.map_or_else(
        || paths.root.join("control").join("sandbox.sock"),
        PathBuf::from,
    )
}

impl SandboxClient {
    /// A client over the socket at `socket`. Opens nothing until a request.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
        }
    }
    /// One request, and the answer to it.
    ///
    /// Output frames arriving before the answer are progress, not the result:
    /// they are passed over here so a caller that does not stream sees only
    /// the outcome. Cancelling shuts the socket down, which is what tells the
    /// service to stop the command rather than finish it unwatched.
    pub async fn request(
        &self,
        request: SandboxRequest,
        token: &CancellationToken,
    ) -> Result<Value> {
        let mut socket =
            tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&self.socket))
                .await
                .map_err(|_| invalid("Sandbox service connection timed out"))?
                .map_err(|e| {
                    invalid(format!(
                        "Sandbox service unavailable at {}: {e}",
                        self.socket.display()
                    ))
                })?;
        write_frame(
            &mut socket,
            &json!(Envelope {
                version: 1,
                request
            }),
        )
        .await?;
        tokio::select! {
            result = async {
                loop {
                    let value = read_frame(&mut socket).await?;
                    if value.get("event").and_then(Value::as_str) != Some("output") { break Ok::<Value, GhostError>(value); }
                }
            } => {
                let value = result?;
                if let Some(error) = value.get("error").and_then(Value::as_str) { return Err(GhostError::new(ErrorKind::Tool, error)); }
                Ok(value)
            },
            () = token.cancelled() => { let _ = socket.shutdown().await; Err(GhostError::aborted("sandbox operation")) }
        }
    }
}
impl OperationExecutor for SandboxClient {
    fn execute<'a>(
        &'a self,
        toolbox: &'a str,
        digest: &'a str,
        operation: &'a str,
        args: Value,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, ToolExecution> {
        Box::pin(async move {
            let Some(placement) = &ctx.placement else {
                return invalid("Missing sandbox identity").into();
            };
            let request = SandboxRequest::Execute {
                toolbox: toolbox.into(),
                digest: digest.into(),
                container: placement.container.clone(),
                operation: operation.into(),
                workspace: placement.workspace_id.clone(),
                agent: placement.agent_id.clone(),
                session: placement.session_key.clone(),
                network: placement.network.clone(),
                args,
            };
            match self.request(request, &ctx.token).await {
                Ok(value) => {
                    let mut result = ToolExecution::ok(
                        value
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    );
                    result.is_error = value
                        .get("isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    result.details = value
                        .get("details")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    result
                }
                Err(error) => error.into(),
            }
        })
    }
}

async fn read_frame(socket: &mut UnixStream) -> Result<Value> {
    let length = socket.read_u32().await.map_err(|e| {
        invalid(format!(
            "Sandbox connection lost; execution outcome may be unknown: {e}"
        ))
    })? as usize;
    if length > MAX_FRAME {
        return Err(invalid("Sandbox frame exceeds limit"));
    }
    let mut bytes = vec![0; length];
    socket
        .read_exact(&mut bytes)
        .await
        .map_err(|e| invalid(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))
}
async fn write_frame(socket: &mut UnixStream, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|e| invalid(e.to_string()))?;
    if bytes.len() > MAX_FRAME {
        return Err(invalid("Sandbox response exceeds limit"));
    }
    // Bounded by `MAX_FRAME` two lines above, so the conversion cannot fail —
    // written as a checked one anyway, because a silent truncation here would
    // frame the response at the wrong length and desynchronise the socket.
    let length =
        u32::try_from(bytes.len()).map_err(|_| invalid("Sandbox response exceeds limit"))?;
    socket
        .write_u32(length)
        .await
        .map_err(|e| invalid(e.to_string()))?;
    socket
        .write_all(&bytes)
        .await
        .map_err(|e| invalid(e.to_string()))
}

struct Service {
    config: ServiceConfig,
    store: Arc<PolicyStore>,
    pool: Arc<ContainerPool>,
}
impl Service {
    fn authorize_toolbox(&self, toolbox: &str, workspace: &str) -> Result<()> {
        let registration = self
            .config
            .workspaces
            .get(workspace)
            .ok_or_else(|| invalid("Workspace is not registered with the sandbox service"))?;
        if registration.toolboxes.iter().any(|name| name == toolbox) {
            Ok(())
        } else {
            Err(invalid("Toolbox is not authorized for this workspace"))
        }
    }

    /// The placement one request resolves to, checked against this service's
    /// own registration rather than trusted from the client.
    ///
    /// The app decides which container an agent selects; the service decides
    /// whether this workspace may use it at all. Both have to agree, and the
    /// service's answer is the one that owns the engine.
    fn spec(
        &self,
        container: &str,
        workspace: &str,
        agent: &str,
        session: &str,
        network: &ContainerNetwork,
    ) -> Result<PlacementRequest> {
        let registration = self
            .config
            .workspaces
            .get(workspace)
            .ok_or_else(|| invalid("Workspace is not registered with the sandbox service"))?;
        if !registration.containers.iter().any(|name| name == container) {
            return Err(invalid("Container is not authorized for this workspace"));
        }
        self.store.require_container(container)?;
        Ok(PlacementRequest {
            agent_id: agent.into(),
            workspace_id: workspace.into(),
            session_key: session.into(),
            toolbox: String::new(),
            container: container.into(),
            network: network.clone(),
            workspace_root: registration.path.to_string_lossy().into_owned(),
        })
    }
}

/// One `Execute` request, after the placement it resolves to is known.
///
/// A struct rather than eight parameters: every field is a name the client
/// supplied, they are all `String`, and a call site that transposed two of them
/// would type-check and authorise the wrong thing.
struct ExecuteRequest {
    spec: PlacementRequest,
    toolbox: String,
    digest: String,
    container: String,
    operation: String,
    workspace: String,
    args: Value,
}

impl Service {
    /// Reads back part of a command's own transcript.
    ///
    /// The run name is checked against an alphabet rather than joined blind:
    /// it becomes a path component under a directory holding every run, so a
    /// separator in it would read another instance's output.
    fn read_transcript(
        &self,
        spec: &PlacementRequest,
        definition: &ToolOperation,
        args: &Value,
    ) -> Result<Value> {
        use std::io::{Read, Seek};
        validate_input(definition, args)?;
        let run = args
            .get("run")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("Transcript run is required"))?;
        if run.is_empty()
            || run.len() > 128
            || !run.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(invalid("Invalid transcript run"));
        }
        let stream = args
            .get("stream")
            .and_then(Value::as_str)
            .unwrap_or("stdout");
        if !matches!(stream, "stdout" | "stderr") {
            return Err(invalid("Invalid transcript stream"));
        }
        let root = self.pool.transcript_directory(spec)?;
        let mut file = std::fs::File::open(root.join(run).join(format!("{stream}.log")))
            .map_err(|e| invalid(e.to_string()))?;
        let offset = args
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(8192)
            .min(65536);
        file.seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| invalid(e.to_string()))?;
        let mut bytes = Vec::new();
        file.take(limit)
            .read_to_end(&mut bytes)
            .map_err(|e| invalid(e.to_string()))?;
        let next = offset.saturating_add(bytes.len() as u64);
        Ok(
            json!({"content":String::from_utf8_lossy(&bytes),"isError":false,"details":{"nextOffset":next}}),
        )
    }

    /// Runs one granted operation in its container.
    ///
    /// Both definitions are re-read here even though the caller resolved them:
    /// the caller is the app, and the service owns the engine. They are then
    /// re-read again on a ticker for as long as the command runs, because an
    /// edit during a twenty-minute scan has to reach the command that is
    /// already inside the container.
    async fn execute(
        &self,
        request: ExecuteRequest,
        token: CancellationToken,
        progress: Option<tokio::sync::mpsc::Sender<Value>>,
    ) -> Result<Value> {
        let ExecuteRequest {
            spec,
            toolbox,
            digest,
            container,
            operation,
            workspace,
            args,
        } = request;
        self.authorize_toolbox(&toolbox, &workspace)?;
        let installed = self.store.require_toolbox(&toolbox)?;
        if digest != installed.digest() {
            return Err(invalid(
                "The toolbox definition changed since the call was prepared",
            ));
        }
        let installed_container = self.store.require_container(&container)?;
        let grant = installed
            .resolved
            .toolbox
            .tools
            .iter()
            .find(|g| g.name == operation && g.permission != ToolPermission::Deny)
            .ok_or_else(|| invalid("Operation is not granted by the toolbox"))?;
        let definition = installed
            .resolved
            .operations
            .get(&grant.name)
            .ok_or_else(|| invalid("Operation is unavailable"))?;
        if matches!(
            definition.implementation,
            OperationImplementation::Transcript
        ) {
            return self.read_transcript(&spec, definition, &args);
        }
        if !matches!(
            definition.implementation,
            OperationImplementation::Command { .. }
        ) {
            return Err(invalid("Only command operations run in tool containers"));
        }
        let jail = Arc::new(WorkspaceJail::new(JailOptions::new(&spec.workspace_root))?);
        let command = command_argv(definition, &args, &jail)?;
        let env = std::env::vars().collect();
        let mut plan = guard_exec(
            &command,
            &ExecGuardOptions {
                jail: &jail,
                config: None,
                env: &env,
                sandboxed: true,
            },
        )?;
        plan.max_output_bytes = plan.max_output_bytes.min(128 * 1024);
        plan.timeout_ms = if plan.timeout_ms == 0 {
            300_000
        } else {
            plan.timeout_ms.min(300_000)
        };
        let runner = self
            .pool
            .resolve_turn(&spec)?
            .ok_or_else(|| invalid("Container runner unavailable"))?;
        let run_token = token.child_token();
        let run = runner.run(RunRequest {
            timeout_ms: plan.timeout_ms,
            plan: plan.clone(),
            token: run_token.clone(),
            clock: Arc::new(SystemClock),
            tee: progress.map(|sender| Arc::new(Progress(sender)) as Arc<dyn OutputTee>),
        });
        tokio::pin!(run);
        let mut interval = tokio::time::interval(REVALIDATE_EVERY);
        loop {
            tokio::select! {
                outcome = &mut run => {
                    let outcome = outcome?;
                    let content = format!("{}\n{}\nExit code: {:?}{}", outcome.stdout, outcome.stderr, outcome.code, if outcome.timed_out { " (timed out)" } else { "" });
                    let run_id = outcome.transcript_dir.as_ref().and_then(|dir| dir.rsplit('/').next()).map(str::to_owned);
                    return Ok(json!({"content":content,"isError":outcome.timed_out || outcome.code != Some(0), "details":{"transcriptDir":outcome.transcript_dir,"run":run_id,"truncated":outcome.truncated}}));
                },
                _ = interval.tick() => {
                    let toolbox_current = self.store.require_toolbox(&toolbox).is_ok_and(|a| a.digest() == installed.digest());
                    let container_current = self.store.require_container(&container).is_ok_and(|a| a.digest == installed_container.digest);
                    if token.is_cancelled() || !toolbox_current || !container_current {
                        run_token.cancel();
                        let _ = (&mut run).await;
                        return Err(invalid("Cancelled, or a definition changed while this ran"));
                    }
                }
            }
        }
    }
}

/// How often a running command's two definitions are re-read from disk.
///
/// An in-flight scan is the one place an edit has to reach code that is
/// already inside the container; the idle sweep cannot see it.
const REVALIDATE_EVERY: Duration = Duration::from_millis(250);

impl Service {
    async fn handle(
        &self,
        request: SandboxRequest,
        token: CancellationToken,
        progress: Option<tokio::sync::mpsc::Sender<Value>>,
    ) -> Result<Value> {
        match request {
            SandboxRequest::Health => match self.pool.probe_engine() {
                Ok(()) => Ok(
                    json!({"version":1,"status":"ready","engine":self.config.engine,"engineReady":true}),
                ),
                Err(error) => Ok(
                    json!({"version":1,"status":"degraded","engine":self.config.engine,"engineReady":false,"engineError":error.message}),
                ),
            },
            SandboxRequest::List => Ok(json!({"instances":self.pool.status()})),
            SandboxRequest::Stop { instance, force } => {
                self.pool.stop_instance(&instance, force)?;
                Ok(json!({"stopped":instance}))
            }
            SandboxRequest::Restart { instance, force } => {
                self.pool.restart_instance(&instance, force)?;
                Ok(json!({"restarted":instance}))
            }
            SandboxRequest::Start {
                container,
                workspace,
                agent,
                session,
                network,
            } => {
                let spec = self.spec(&container, &workspace, &agent, &session, &network)?;
                self.pool.warm(&spec)?;
                Ok(json!({"instances":self.pool.status()}))
            }
            SandboxRequest::Execute {
                toolbox,
                digest,
                container,
                operation,
                workspace,
                agent,
                session,
                network,
                args,
            } => {
                let spec = self.spec(&container, &workspace, &agent, &session, &network)?;
                self.execute(
                    ExecuteRequest {
                        spec,
                        toolbox,
                        digest,
                        container,
                        operation,
                        workspace,
                        args,
                    },
                    token,
                    progress,
                )
                .await
            }
        }
    }
}

/// Start the operator-configured service. Socket permissions are restricted to its group.
pub async fn serve(mut config: ServiceConfig) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    std::fs::create_dir_all(&config.state_root).map_err(|e| invalid(e.to_string()))?;
    if let Some(parent) = config.socket.parent() {
        std::fs::create_dir_all(parent).map_err(|e| invalid(e.to_string()))?;
    }
    config.validate_paths()?;
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.state_root.join("service.lock"))
        .map_err(|e| invalid(e.to_string()))?;
    let _lease = nix::fcntl::Flock::lock(lease, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|(_, e)| {
            invalid(format!(
                "Another sandbox service owns this state directory: {e}"
            ))
        })?;
    let identity_path = config.state_root.join("installation-id");
    let installation = match std::fs::read_to_string(&identity_path) {
        Ok(id) => id,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let id = uuid::Uuid::now_v7().to_string();
            std::fs::write(&identity_path, &id).map_err(|e| invalid(e.to_string()))?;
            id
        }
        Err(error) => return Err(invalid(error.to_string())),
    };
    let prefix = format!("service:{installation}/");
    let owner = format!("{prefix}{}", uuid::Uuid::now_v7());
    if let Ok(metadata) = std::fs::symlink_metadata(&config.socket) {
        if !metadata.file_type().is_socket() {
            return Err(invalid("Refusing to replace a non-socket path"));
        }
        if UnixStream::connect(&config.socket).await.is_ok() {
            return Err(invalid("Sandbox socket is already serving"));
        }
        std::fs::remove_file(&config.socket).map_err(|e| invalid(e.to_string()))?;
    }
    if let Some(parent) = config.socket.parent() {
        std::fs::create_dir_all(parent).map_err(|e| invalid(e.to_string()))?;
    }
    let listener = UnixListener::bind(&config.socket)
        .map_err(|e| invalid(format!("Cannot bind {}: {e}", config.socket.display())))?;
    std::fs::set_permissions(&config.socket, std::fs::Permissions::from_mode(0o660))
        .map_err(|e| invalid(e.to_string()))?;
    let store = Arc::new(PolicyStore::new(config.policy_root.clone()));
    let pool = build_pool(&config, Arc::clone(&store), owner, &prefix);
    let service = Arc::new(Service {
        config,
        store,
        pool,
    });
    let connections = Arc::new(tokio::sync::Semaphore::new(64));
    let mut maintenance = tokio::time::interval(MAINTENANCE_EVERY);
    let mut termination = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| invalid(e.to_string()))?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => { service.pool.close(); let _ = std::fs::remove_file(&service.config.socket); return Ok(()); },
            _ = termination.recv() => { service.pool.close(); let _ = std::fs::remove_file(&service.config.socket); return Ok(()); },
            _ = maintenance.tick() => {
                service.pool.maintain();
            },
            accepted = listener.accept() => {
                let (socket, _) = accepted.map_err(|e| invalid(e.to_string()))?;
                let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else { continue; };
                let service = Arc::clone(&service);
                tokio::spawn(async move {
                    let _permit = permit;
                    serve_connection(&service, socket).await;
                });
            }
        }
    }
}

/// The pool the service runs everything in.
///
/// `owner` and `prefix` come from the installation identity: containers this
/// installation started are its own to reap, and a peer installation's are not.
/// The path mappings are sorted longest first, so the most specific operator
/// mapping wins rather than whichever was declared first.
fn build_pool(
    config: &ServiceConfig,
    store: Arc<PolicyStore>,
    owner: String,
    prefix: &str,
) -> Arc<ContainerPool> {
    let peers = prefix.to_owned();
    let engine = docker_engine(DockerEngineOptions {
        bin: config.engine.clone(),
        gateway_image: config.gateway_image.clone(),
        owner: Some(owner.clone()),
        is_owner_alive: Some(Arc::new(move |candidate| !candidate.starts_with(&peers))),
        ..Default::default()
    });
    let mut options = ContainerPoolOptions::new(store, engine, config.state_root.join("runs"));
    options.bin = Some(config.engine.clone());
    options.owner = Some(owner);
    let mut mappings: Vec<_> = config
        .workspaces
        .values()
        .map(|w| (w.path.clone(), w.daemon_path.clone()))
        .collect();
    mappings.push((config.state_root.clone(), config.daemon_state_root.clone()));
    mappings.sort_by_key(|(source, _)| std::cmp::Reverse(source.components().count()));
    options.host_path = Some(Arc::new(move |path| {
        for (source, target) in &mappings {
            if let Ok(relative) = std::path::Path::new(path).strip_prefix(source) {
                return target.join(relative).to_string_lossy().into_owned();
            }
        }
        path.to_owned()
    }));
    ContainerPool::new(options)
}

/// One accepted connection: one request in, progress frames out, one answer.
///
/// The client socket is read from *while* the request runs, because a read that
/// completes is the client having gone away — and a command nobody is waiting
/// for should be cancelled rather than left running in a container.
async fn serve_connection(service: &Service, mut socket: UnixStream) {
    let result = async {
        let frame = tokio::time::timeout(FRAME_TIMEOUT, read_frame(&mut socket))
            .await
            .map_err(|_| invalid("Request timed out"))??;
        let envelope: Envelope =
            serde_json::from_value(frame).map_err(|e| invalid(e.to_string()))?;
        if envelope.version != 1 {
            return Err(invalid("Unsupported sandbox protocol version"));
        }
        let token = CancellationToken::new();
        let (sender, mut progress) = tokio::sync::mpsc::channel(64);
        let task = service.handle(envelope.request, token.clone(), Some(sender));
        tokio::pin!(task);
        let mut disconnect = [0u8; 1];
        loop {
            tokio::select! {
                result = &mut task => break result,
                _ = socket.read(&mut disconnect) => { token.cancel(); break (&mut task).await; },
                Some(event) = progress.recv() => {
                    if !matches!(tokio::time::timeout(FRAME_TIMEOUT, write_frame(&mut socket, &event)).await, Ok(Ok(()))) {
                        token.cancel(); break (&mut task).await;
                    }
                }
            }
        }
    }
    .await;
    let response = match result {
        Ok(value) => value,
        Err(error) => json!({"error":error.message}),
    };
    let _ = write_frame(&mut socket, &response).await;
}

/// How long one frame may take to cross the socket.
///
/// A client that stops reading must not hold a connection slot open, and a
/// request whose first frame never arrives is not a request.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// How often idle instances are swept and re-checked against the ledger.
const MAINTENANCE_EVERY: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test fixture setup")]
    use super::{ServiceConfig, WorkspaceRegistration};
    use std::collections::BTreeMap;

    fn config(root: &std::path::Path) -> ServiceConfig {
        for directory in [
            "policies",
            "policies/toolboxes",
            "policies/tool-definitions",
            "policies/containers",
            "state",
            "control",
            "workspace",
        ] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        ServiceConfig {
            socket: root.join("control/sandbox.sock"),
            policy_root: root.join("policies"),
            state_root: root.join("state"),
            daemon_state_root: "/daemon/state".into(),
            workspaces: BTreeMap::from([(
                "default".into(),
                WorkspaceRegistration {
                    path: root.join("workspace"),
                    daemon_path: "/daemon/workspace".into(),
                    toolboxes: vec!["coding".into()],
                    containers: vec!["dev".into()],
                },
            )]),
            engine: "docker".into(),
            gateway_image: None,
        }
    }

    #[test]
    fn service_paths_must_be_separate_operator_roots() {
        let root = tempfile::tempdir().unwrap();
        assert!(config(root.path()).validate_paths().is_ok());
        let mut overlap = config(root.path());
        overlap.workspaces.get_mut("default").unwrap().path =
            root.path().join("policies/toolboxes");
        assert!(overlap.validate_paths().is_err());
        let mut daemon_overlap = config(root.path());
        daemon_overlap
            .workspaces
            .get_mut("default")
            .unwrap()
            .daemon_path = "/daemon".into();
        assert!(daemon_overlap.validate_paths().is_err());
    }
}
