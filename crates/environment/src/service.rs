//! Versioned, bounded Unix-socket protocol for running a command in a
//! container.
use crate::container_pool::{
    ContainerPool, ContainerPoolOptions, DockerEngineOptions, docker_engine,
};
use darkwire_core::{ErrorKind, Result, SystemClock, WireError};
use darkwire_protocol::rest::ResolveImageResponse;
use darkwire_protocol::{EnvironmentNetwork, SandboxRequest};
use darkwire_security::environment::invalid;
use darkwire_security::{ExecGuardOptions, JailOptions, PolicyStore, WorkspaceJail, guard_exec};
use darkwire_tools::{
    BoxFuture, CommandRunner, Environment, OutputStream, OutputTee, PlacementRequest, RunOutcome,
    RunRequest,
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
        let mut workspaces = std::mem::take(&mut self.workspaces);
        for registration in workspaces.values_mut() {
            self.check_registration(registration)?;
        }
        self.workspaces = workspaces;
        Ok(())
    }

    /// Canonicalise one workspace mount and refuse it if it overlaps ours.
    ///
    /// Split out of [`Self::validate_paths`] because a registration no longer
    /// has to be present at boot: a [`WorkspaceLookup`] answers for workspaces
    /// created since, and one that skipped this check would be a way around it.
    /// One function, so the two paths cannot drift into two answers.
    pub(crate) fn check_registration(
        &self,
        registration: &mut WorkspaceRegistration,
    ) -> Result<()> {
        let mut protected = vec![self.state_root.clone(), self.socket.clone()];
        let environments = self.policy_root.join("environments");
        protected.push(if environments.exists() {
            environments
                .canonicalize()
                .map_err(|e| invalid(e.to_string()))?
        } else {
            environments
        });
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
        Ok(())
    }
}

/// Answers "may this workspace exist, and what may it reach" per request.
///
/// `ServiceConfig.workspaces` is read once, which is right for a deployed
/// service: that map is an operator's allow-list bounding an app in another
/// trust domain, and it should not move without them saying so. An embedded
/// service is the same operator, the same process tree and the same policy
/// directory, so its answer is derived rather than configured. Deriving it once
/// at boot meant a workspace or an environment created afterwards could not run
/// until `serve` restarted.
///
/// A lookup is consulted only for a workspace the fixed map does not name, so a
/// deployed service that supplies none behaves exactly as before.
pub trait WorkspaceLookup: Send + Sync {
    /// The registration for this workspace, or `None` if there is no such
    /// workspace. Paths need not be canonical: the service checks and
    /// canonicalises what it is handed.
    fn resolve(&self, workspace: &str) -> Option<WorkspaceRegistration>;
}

/// Refuse a path that is relative, root, or walks upward.
///
/// Free rather than nested in `validate_paths`, because the per-registration
/// check the dynamic path shares needs it too.
fn absolute(path: &std::path::Path) -> Result<()> {
    if !path.is_absolute()
        || path.parent().is_none()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(invalid(format!(
            "Expected an absolute non-root path: {}",
            path.display()
        )));
    }
    Ok(())
}

/// One workspace a client may name, and the policy it may reach from there.
///
/// The app decides which environment an agent selects; this decides whether it
/// may be used in this workspace at all.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceRegistration {
    /// The workspace root as this service resolves it.
    pub path: PathBuf,
    /// The same directory as the daemon resolves it, which is not always the
    /// same string: a bind source is resolved by the daemon, not by this
    /// process.
    pub daemon_path: PathBuf,
    /// Environments that may be started here.
    #[serde(default)]
    pub environments: Vec<String>,
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
pub fn socket_path(env: Option<&str>, paths: &darkwire_core::WirePaths) -> PathBuf {
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
                    if value.get("event").and_then(Value::as_str) != Some("output") { break Ok::<Value, WireError>(value); }
                }
            } => {
                let value = result?;
                if let Some(error) = value.get("error").and_then(Value::as_str) { return Err(WireError::new(ErrorKind::Tool, error)); }
                Ok(value)
            },
            () = token.cancelled() => { let _ = socket.shutdown().await; Err(WireError::aborted("sandbox operation")) }
        }
    }
}
/// Commands run in an installed environment, through the service.
///
/// Holds the placement because [`CommandRunner::run`] carries only a plan:
/// which environment, workspace and session a command belongs to is a property
/// of the turn, resolved once when the environment is, not re-derived per
/// call. The plan's own `cwd` and environment are dropped on the way out —
/// they describe this machine, and the service composes the container's.
pub struct ContainerEnvironment {
    client: SandboxClient,
    placement: PlacementRequest,
}

impl ContainerEnvironment {
    /// An environment for one turn's placement.
    #[must_use]
    pub fn new(client: SandboxClient, placement: PlacementRequest) -> Self {
        Self { client, placement }
    }
}

impl CommandRunner for ContainerEnvironment {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        Box::pin(async move {
            let mut argv = Vec::with_capacity(request.plan.args.len() + 1);
            argv.push(request.plan.file.clone());
            argv.extend(request.plan.args.iter().cloned());
            let value = self
                .client
                .request(
                    SandboxRequest::Exec {
                        environment: self.placement.environment.clone(),
                        workspace: self.placement.workspace_id.clone(),
                        agent: self.placement.agent_id.clone(),
                        session: self.placement.session_key.clone(),
                        argv,
                        timeout_ms: request.timeout_ms,
                        max_output_bytes: request.plan.max_output_bytes,
                        network: self.placement.network.clone(),
                    },
                    &request.token,
                )
                .await?;
            serde_json::from_value(value).map_err(|e| invalid(e.to_string()))
        })
    }
}

impl Environment for ContainerEnvironment {
    fn confined(&self) -> bool {
        true
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
    /// Consulted for a workspace the configured map does not name. `None` for a
    /// deployed service, whose allow-list is the operator's to write.
    lookup: Option<Arc<dyn WorkspaceLookup>>,
}
impl Service {
    /// The placement one request resolves to, checked against this service's
    /// own registration rather than trusted from the client.
    ///
    /// The app decides which environment an agent selects; the service decides
    /// whether this workspace may use it at all. Both have to agree, and the
    /// service's answer is the one that owns the engine.
    fn spec(
        &self,
        environment: &str,
        workspace: &str,
        agent: &str,
        session: &str,
        network: &EnvironmentNetwork,
    ) -> Result<PlacementRequest> {
        // The configured map first, so a deployed allow-list is never widened by
        // a lookup. Only a workspace it does not name reaches one, and what
        // comes back goes through the same overlap check the boot-time
        // registrations did.
        let registration = if let Some(registration) = self.config.workspaces.get(workspace) {
            registration.clone()
        } else {
            let mut resolved = self
                .lookup
                .as_ref()
                .and_then(|lookup| lookup.resolve(workspace))
                .ok_or_else(|| {
                    invalid("Workspace is not registered with the environment service")
                })?;
            self.config.check_registration(&mut resolved)?;
            resolved
        };
        if !registration
            .environments
            .iter()
            .any(|name| name == environment)
        {
            return Err(invalid("Environment is not authorized for this workspace"));
        }
        self.store.require_environment(environment)?;
        Ok(PlacementRequest {
            agent_id: agent.into(),
            workspace_id: workspace.into(),
            session_key: session.into(),
            environment: environment.into(),
            network: network.clone(),
            workspace_root: registration.path.to_string_lossy().into_owned(),
        })
    }
}

impl Service {
    /// Guards one argv and runs it in the caller's environment.
    ///
    /// The service guards again because it, rather than the app, owns the engine.
    async fn run_guarded(
        &self,
        spec: &PlacementRequest,
        command: &[String],
        bounds: Bounds,
        drift: (String, String),
        token: CancellationToken,
        progress: Option<tokio::sync::mpsc::Sender<Value>>,
    ) -> Result<RunOutcome> {
        let jail = Arc::new(WorkspaceJail::new(JailOptions::new(&spec.workspace_root))?);
        let env = std::env::vars().collect();
        let mut plan = guard_exec(
            command,
            &ExecGuardOptions {
                jail: &jail,
                config: None,
                env: &env,
                sandboxed: true,
            },
        )?;
        plan.max_output_bytes = bounds.output().min(MAX_OUTPUT_BYTES);
        plan.timeout_ms = match bounds.timeout() {
            0 => MAX_TIMEOUT_MS,
            asked => asked.min(MAX_TIMEOUT_MS),
        };
        let runner = self
            .pool
            .resolve_turn(spec)?
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
                outcome = &mut run => return outcome,
                _ = interval.tick() => {
                    if token.is_cancelled() || !self.definition_unchanged(&drift) {
                        run_token.cancel();
                        let _ = (&mut run).await;
                        return Err(invalid("Cancelled, or a definition changed while this ran"));
                    }
                }
            }
        }
    }

    /// Whether the environment still hashes to what the call was prepared at.
    fn definition_unchanged(&self, drift: &(String, String)) -> bool {
        let (environment, digest) = drift;
        self.store
            .require_environment(environment)
            .is_ok_and(|current| &current.digest == digest)
    }
}

/// What a caller asked for, before the service clamps it.
#[derive(Debug, Clone, Copy, Default)]
struct Bounds {
    timeout_ms: u64,
    max_output_bytes: u64,
}

impl Bounds {
    fn timeout(self) -> u64 {
        self.timeout_ms
    }

    fn output(self) -> u64 {
        if self.max_output_bytes == 0 {
            MAX_OUTPUT_BYTES
        } else {
            self.max_output_bytes
        }
    }
}

/// The ceilings the service imposes whatever a caller asks for.
const MAX_OUTPUT_BYTES: u64 = 128 * 1024;
const MAX_TIMEOUT_MS: u64 = 300_000;

/// How often a running command's environment definition is re-read from disk.
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
            SandboxRequest::ResolveImage { reference } => {
                // On a blocking pool thread: a pull can take minutes and this
                // runs on the reactor that answers every other request. The
                // accept loop spawns a task per connection, so an `exec` in
                // another session is unaffected either way.
                let pool = Arc::clone(&self.pool);
                let asked = reference.clone();
                let resolved = tokio::task::spawn_blocking(move || pool.resolve_image(&asked))
                    .await
                    .map_err(|error| invalid(format!("Resolving the image panicked: {error}")))??;
                serde_json::to_value(ResolveImageResponse {
                    reference,
                    image: resolved.image,
                    pulled: resolved.pulled,
                })
                .map_err(|e| invalid(e.to_string()))
            }
            SandboxRequest::Stop { instance } => {
                self.pool.stop_instance(&instance)?;
                Ok(json!({"stopped":instance}))
            }
            SandboxRequest::Restart { instance } => {
                self.pool.restart_instance(&instance)?;
                Ok(json!({"restarted":instance}))
            }
            SandboxRequest::Start {
                environment,
                workspace,
                agent,
                session,
                network,
            } => {
                let spec = self.spec(&environment, &workspace, &agent, &session, &network)?;
                self.pool.warm(&spec)?;
                Ok(json!({"instances":self.pool.status()}))
            }
            SandboxRequest::Exec {
                environment,
                workspace,
                agent,
                session,
                argv,
                timeout_ms,
                max_output_bytes,
                network,
            } => {
                let spec = self.spec(&environment, &workspace, &agent, &session, &network)?;
                let installed = self.store.require_environment(&environment)?;
                let outcome = self
                    .run_guarded(
                        &spec,
                        &argv,
                        Bounds {
                            timeout_ms,
                            max_output_bytes,
                        },
                        (environment.clone(), installed.digest.clone()),
                        token,
                        progress,
                    )
                    .await?;
                serde_json::to_value(&outcome).map_err(|e| invalid(e.to_string()))
            }
        }
    }
}

/// Start the operator-configured service. Socket permissions are restricted to its group.
///
/// The configured workspace map is the whole allow-list. Use [`serve_with`] to
/// supply a [`WorkspaceLookup`] as well.
pub async fn serve(config: ServiceConfig) -> Result<()> {
    serve_with(config, None).await
}

/// [`serve`], plus a lookup for workspaces the configured map does not name.
pub async fn serve_with(
    mut config: ServiceConfig,
    lookup: Option<Arc<dyn WorkspaceLookup>>,
) -> Result<()> {
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
        lookup,
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
            "policies/environments",
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
                    environments: vec!["dev".into()],
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
            root.path().join("policies/environments");
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
