//! `darkwire serve` — the same agent, behind a port.
//!
//! This is the second composition root, and the only place where every piece
//! meets: one SQLite connection shared by the session store, the auth tables
//! and the notifications; one runtime over it; one approval gate threaded into
//! the loop *and* into the hub; one hub serving both the WebSocket and every
//! channel; one router serving the API and the built UI.
//!
//! The order below is not arbitrary — each step needs the one before it:
//!
//!  1. **Read the config, open the database.** The config names the workspace,
//!     which decides where the database is; everything else shares that one
//!     connection so writes share a write-ahead log.
//!  2. **Build the approval gate.** It has to exist before the runtime, because
//!     the runtime hands it to the loop at construction: without it, a tool
//!     whose policy is `ask` runs unattended behind a browser.
//!  3. **Build the runtime, then the hub over its loop.** The hub reads the
//!     loop through a function, so a settings save moves the next turn onto the
//!     new provider while the running one keeps the loop it started on.
//!  4. **Resolve the UI root, then build the server over the adapter.**
//!  5. **Bind the listener**, and only then write the ready file — the port is
//!     not knowable before the bind when `--port 0` asked the OS for one.
//!  6. **Start the channels.** They bridge to the same hub, so a channel turn
//!     is a web turn that arrived somewhere else.
//!
//! Shutdown runs the same list backwards, and it is not decoration: the
//! scheduler stops starting work, the channels stop accepting, the hub aborts
//! what is running, the listener closes, and only then does the connection
//! close — the reverse order would close the database under a turn that is
//! still writing to it.

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use darkwire_channels::{
    ChannelFactory, ChannelHub, ChannelHubConnectOptions, ChannelHubConnection, ChannelManager,
    ChannelManagerOptions,
};
use darkwire_core::Clock;
use darkwire_core::paths::WirePaths;
use darkwire_core::{
    Database, ErrorKind, LoadConfigOptions, LoadedConfig, Result, SystemClock, WireError,
    ensure_dir, load_config,
};
use darkwire_protocol::automation::AutomationRun;
use darkwire_protocol::rest::ChannelStatus;
use darkwire_protocol::ws::{
    ClientMessage, NotificationBody, NotificationLevel, NotificationTag, ToolsChanged,
    ToolsChangedTag,
};
use darkwire_protocol::{ServerMessage, new_uuid};
use darkwire_runtime::{
    RuntimeOptions, VaultChoice, WireRuntime, create_runtime, resolve_agent_or_default,
};
use darkwire_security::CredentialVault;
use darkwire_security::jail::JailCheck;
use darkwire_security::random::{OsRandom, RandomSource};
use darkwire_server::hub::{ConnectOptions, Frame, HubClient, HubEvent, OutboundStream};
use darkwire_server::runtime::DirectChatInput;
use darkwire_server::scheduler::SchedulerPort;
use darkwire_server::scheduler::{
    DirectChat, ReadTaskFile, SchedulerConnectOptions, SchedulerConnection, SchedulerOptions,
};
use darkwire_server::{
    CreateNotificationInput, HubApprovalGate, HubApprovalGateOptions, NotificationStore, Scheduler,
    ServerAutomationResolver, ServerOptions, ServerRuntime, SessionHub, SessionHubOptions, UiRoot,
    UnattendedApproval, WireServer, create_server,
};
use darkwire_tools::{ListenerId, ToolRegistry};
use darkwire_tui::palette_for;
use tokio_util::sync::CancellationToken;

use crate::Streams;
use crate::i18n::{Env, Translations};
use crate::program::{Globals, ServeArgs};
use crate::runtime::{env_map, install_logger, load_options};
use crate::server_runtime::{CliServerRuntime, ServerRuntimeOptions};
use crate::telegram::{
    TELEGRAM_CHANNEL_ID, TelegramFactoriesOptions, TelegramStatusOptions, telegram_factories,
    telegram_status,
};

/// What a bound server writes down about itself.
///
/// A file rather than a line on stdout, because stdout is logs and a file is a
/// contract: a supervisor that has to know the port can poll for the file
/// appearing and read it once, without parsing whatever the logger happened to
/// emit first. `--json` prints the same record for the case where the caller
/// owns the pipe and would rather not have a path to clean up.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyRecord {
    /// The port that was actually bound. The point of the file when `--port 0`
    /// asked the operating system for one.
    pub port: u16,
    /// The one-time code that claims an install with no password.
    ///
    /// `null` on every run but a first one. It appears here and in the banner
    /// and nowhere else, which is the property that lets the server come up
    /// unclaimed instead of refusing to start and leaving the UI that would set
    /// a password unreachable.
    pub setup_code: Option<String>,
    /// This process, so a supervisor can signal it without a process table
    /// search that might match a second install.
    pub pid: u32,
}

/// Writes the record where a supervisor can see it appear atomically.
///
/// Written to a sibling temporary file and renamed, because a reader polling
/// for the path must never observe a half-written record: a rename within one
/// directory is atomic, and a direct write is two syscalls with a visible state
/// between them.
pub fn write_ready_file(path: &Path, record: &ReadyRecord) -> Result<()> {
    let body = serde_json::to_string(record).map_err(|error| {
        WireError::new(ErrorKind::Internal, "Could not encode the ready record").with_source(error)
    })?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temporary, body.as_bytes()).map_err(|error| {
        WireError::new(
            ErrorKind::Storage,
            format!("Could not write {}: {error}", temporary.display()),
        )
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        WireError::new(
            ErrorKind::Storage,
            format!(
                "Could not move the ready file into {}: {error}",
                path.display()
            ),
        )
    })
}

// The end-to-end seams
//
// Three pairs of functions, each of which answers "nothing" in a shipping build
// and is compiled away with it. They are written as pairs rather than as `if
// cfg!(…)` so that a build without the feature contains no trace of the seam at
// all — not a branch, not a string, not the module they call into.

/// The vault the browser suite runs on, shared by the runtime and the adapter.
#[cfg(feature = "test-hooks")]
fn test_vault(paths: &WirePaths) -> Option<Arc<parking_lot::Mutex<CredentialVault>>> {
    crate::test_hooks::vault(paths)
}

/// Never, in a shipping build.
#[cfg(not(feature = "test-hooks"))]
fn test_vault(paths: &WirePaths) -> Option<Arc<parking_lot::Mutex<CredentialVault>>> {
    let _ = paths;
    None
}

/// A given vault, or the default: open `vault.json` on demand and only if it is
/// already there.
fn vault_choice(vault: Option<Arc<parking_lot::Mutex<CredentialVault>>>) -> VaultChoice {
    vault.map_or(VaultChoice::Default, VaultChoice::Given)
}

/// The comparison that stands in for argon2id while the suite is running.
#[cfg(feature = "test-hooks")]
fn test_hasher() -> Option<Arc<dyn darkwire_server::PasswordHasher>> {
    crate::test_hooks::hasher()
}

/// Never, in a shipping build: argon2id is the only hasher there is.
#[cfg(not(feature = "test-hooks"))]
fn test_hasher() -> Option<Arc<dyn darkwire_server::PasswordHasher>> {
    None
}

/// Registers `e2e_wait`, the one tool that exists only for the browser suite.
///
/// `ToolSource::Extension` rather than `Builtin`, and that is not cosmetic:
/// every reconfigure calls `unregister_by_source(Builtin)` and registers the
/// built-ins again, so a settings save in the middle of a spec would take the
/// waiting tool out from under the turn that was using it.
#[cfg(feature = "test-hooks")]
fn register_test_tools(runtime: &WireRuntime) {
    if let Some(tool) = crate::test_hooks::wait_tool() {
        // A name collision is the only failure, and there cannot be one: no
        // built-in is called `e2e_wait`.
        let _ = runtime
            .tools()
            .register(tool, darkwire_protocol::tools::ToolSource::Extension);
    }
}

/// Nothing to register, in a shipping build.
#[cfg(not(feature = "test-hooks"))]
fn register_test_tools(runtime: &WireRuntime) {
    let _ = runtime;
}

/// Finds the built UI.
///
/// An explicit `--ui` must exist — pointing at the wrong directory and getting a
/// silently API-only server is a worse afternoon than an error at startup. With
/// no flag it is the bundle compiled into this binary, and a build that carries
/// none is honest rather than broken: the API works and `GET /` is a JSON 404.
pub fn resolve_ui_root(explicit: Option<&str>) -> Result<UiRoot> {
    let Some(explicit) = explicit else {
        return Ok(if darkwire_server::ui::has_embedded_bundle() {
            UiRoot::Embedded
        } else {
            UiRoot::None
        });
    };

    let root = std::path::absolute(explicit).unwrap_or_else(|_| PathBuf::from(explicit));
    if !root.join(darkwire_server::ui::INDEX_FILE).exists() {
        return Err(WireError::new(
            ErrorKind::Config,
            format!(
                "No {} in {}. Is that the built UI directory?",
                darkwire_server::ui::INDEX_FILE,
                root.display()
            ),
        ));
    }
    Ok(UiRoot::Dir(root))
}

/// Pumps one hub connection's outbound frames into a sink.
///
/// The hub hands a connection over as a client plus a stream of already-encoded
/// frames, because the WebSocket route would have encoded the same bytes a
/// moment later anyway. A channel and the scheduler both want the value back,
/// so this is where the bytes are turned round again — once, here, rather than
/// in each of them.
fn pump(mut stream: OutboundStream, send: impl Fn(ServerMessage) + Send + 'static) {
    tokio::spawn(async move {
        while let Some(outbound) = stream.next().await {
            let darkwire_server::hub::Outbound::Text(text) = outbound else {
                // A close instruction: there is nothing for a non-socket
                // consumer to do with a status code, and the stream ending is
                // the signal it actually acts on.
                break;
            };
            match serde_json::from_str::<ServerMessage>(&text) {
                Ok(message) => send(message),
                Err(error) => {
                    tracing::warn!(%error, "a hub frame could not be read back");
                }
            }
        }
    });
}

/// An agent loop, as the hub's runner port.
///
/// The hub states what it needs of a loop as a trait so that a route test can
/// stand a scripted one in its place; this is the adapter for the real thing,
/// and it lives here because the composition root is the only place that holds
/// both.
struct LoopRunner {
    agent_loop: darkwire_agent::AgentLoop,
}

impl darkwire_server::hub::TurnRunner for LoopRunner {
    fn run(
        &self,
        input: darkwire_agent::TurnInput,
        parent: &CancellationToken,
    ) -> Box<dyn darkwire_server::hub::TurnHandle> {
        Box::new(self.agent_loop.run(input, parent))
    }

    fn steer(&self, session_key: &str, content: &str) {
        self.agent_loop.steer(session_key, content);
    }
}

/// The `automation` tool's reach into a scheduler that does not exist yet.
///
/// The knot: the loop is handed its automation resolver at construction, the
/// real resolver is built over the job store, and the job store is created
/// while the server is built — which needs the runtime the loop belongs to. So
/// the loop is handed this, and this is filled in once the real one exists. The
/// loop resolves through it once per turn, so filling it in later is enough.
///
/// Before it is filled in, the tool reports that nothing can be scheduled,
/// which is the honest answer during a boot that has not reached the scheduler.
#[derive(Default)]
struct LateAutomation {
    inner: parking_lot::RwLock<Option<Arc<dyn darkwire_tools::AutomationResolver>>>,
}

impl LateAutomation {
    fn fill(&self, resolver: Arc<dyn darkwire_tools::AutomationResolver>) {
        *self.inner.write() = Some(resolver);
    }
}

impl darkwire_tools::AutomationResolver for LateAutomation {
    fn for_turn(
        &self,
        request: &darkwire_tools::PlacementRequest,
    ) -> Option<Arc<dyn darkwire_tools::AutomationPort>> {
        self.inner.read().as_ref()?.for_turn(request)
    }
}

/// The engine, as the routes reach it before it exists.
///
/// The same knot as [`LateAutomation`] and it is the one that had actually come
/// untied: the scheduler is built over the job store, the job store is created
/// while the server is built, and the server's routes need the scheduler — so
/// `create_server` was handed `None` and nothing ever handed it anything else.
/// `Run now` answered "this build has no scheduler" on every install, and so
/// did every route that asks the engine to look again.
///
/// Before it is filled in there genuinely is no engine, and the routes say so.
#[derive(Default)]
struct LateScheduler {
    inner: parking_lot::RwLock<Option<Arc<Scheduler>>>,
}

impl LateScheduler {
    fn fill(&self, scheduler: &Arc<Scheduler>) {
        *self.inner.write() = Some(Arc::clone(scheduler));
    }
}

impl SchedulerPort for LateScheduler {
    fn run_now(&self, job_id: &str) -> Result<AutomationRun> {
        let Some(scheduler) = self.inner.read().clone() else {
            return Err(WireError::new(
                ErrorKind::NotFound,
                "This build has no scheduler, so a job cannot be run on demand.",
            ));
        };
        scheduler.run_now(job_id)
    }

    fn refresh(&self) {
        if let Some(scheduler) = self.inner.read().clone() {
            scheduler.refresh();
        }
    }

    fn enabled(&self) -> bool {
        self.inner
            .read()
            .as_ref()
            .is_some_and(|scheduler| scheduler.enabled())
    }
}

/// The hub and the notification store, as the approval gate reaches them.
///
/// The same knot as [`LateAutomation`], tied the other way round: the gate is
/// built *before* the runtime because the runtime hands it to the loop, and the
/// two things it needs to answer an unattended prompt — a watcher count and
/// somewhere to raise a notification — are built after it.
///
/// The hub is held weakly because the hub holds the gate: a strong handle here
/// would be a cycle, and neither would ever be dropped. The notification store
/// holds nothing back, so it is held strongly.
#[derive(Default)]
struct LateWatch {
    hub: parking_lot::RwLock<Option<std::sync::Weak<SessionHub>>>,
    notifications: parking_lot::RwLock<Option<Arc<NotificationStore>>>,
}

impl LateWatch {
    fn fill_hub(&self, hub: &Arc<SessionHub>) {
        *self.hub.write() = Some(Arc::downgrade(hub));
    }

    fn fill_notifications(&self, notifications: Arc<NotificationStore>) {
        *self.notifications.write() = Some(notifications);
    }

    fn hub(&self) -> Option<Arc<SessionHub>> {
        self.hub.read().as_ref().and_then(std::sync::Weak::upgrade)
    }

    /// How many clients can see a session.
    ///
    /// One when the hub is not up: during boot there is no turn, and the safe
    /// reading of "I cannot tell" is "somebody is there", which raises nothing
    /// rather than raising a notification for every request.
    fn watchers(&self, session_key: &str) -> usize {
        self.hub().map_or(1, |hub| hub.watchers(session_key))
    }

    /// Sends somebody to look at a prompt that went to an empty room.
    ///
    /// Both halves, and they are not redundant: the row survives a closed tab,
    /// and the frame updates an open one without a poll. A scheduled run is the
    /// case this exists for — its prompt is raised against a session no browser
    /// is in, and an unanswered request is denied when it expires.
    fn announce(&self, approval: &UnattendedApproval) {
        let Some(notifications) = self.notifications.read().clone() else {
            return;
        };
        let raised = notifications.create(CreateNotificationInput {
            title: format!("Approval needed for \"{}\"", approval.tool_name),
            body: format!(
                "The \"{}\" agent asked to run \"{}\" on a session nobody was watching. \
                 Open the session to answer it. An unanswered request is denied when it expires.",
                approval.agent_id, approval.tool_name
            ),
            level: NotificationLevel::Warning,
            session_key: Some(approval.session_key.clone()),
            job_id: None,
        });
        let raised = match raised {
            Ok(raised) => raised,
            Err(error) => {
                tracing::warn!(%error, "an unattended approval could not be recorded");
                return;
            }
        };
        let Some(hub) = self.hub() else {
            return;
        };
        hub.broadcast(&HubEvent::Notification(NotificationBody {
            tag: NotificationTag,
            id: raised.id,
            title: raised.title,
            body: raised.body,
            level: raised.level,
            created_at_ms: raised.created_at_ms,
            session_key: raised.session_key,
            job_id: raised.job_id,
        }));
    }
}

/// Runs `act` once per scheduler tick, however many times it is asked.
///
/// The tool registry notifies per *mutation*, and a mutation is one tool: an
/// MCP server registering forty is forty notifications, and a settings save
/// unregisters every built-in and registers them again before it is done. A
/// frame per mutation would be a `tools.changed` storm on every save, and a
/// client cannot tell forty frames from one meaningful change.
///
/// A yield rather than a timer, so the batch closes before anything can observe
/// the intermediate state and there is nothing to clean up on shutdown. Off a
/// runtime there is nothing to yield to, so the call runs through — which is
/// the honest answer for a caller that is not inside the server at all.
pub fn coalesce(act: Arc<dyn Fn() + Send + Sync>) -> impl Fn() + Send + Sync + 'static {
    let queued = Arc::new(std::sync::atomic::AtomicBool::new(false));
    move || {
        if queued.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            queued.store(false, std::sync::atomic::Ordering::Release);
            act();
            return;
        };
        let queued = Arc::clone(&queued);
        let act = Arc::clone(&act);
        handle.spawn(async move {
            tokio::task::yield_now().await;
            queued.store(false, std::sync::atomic::Ordering::Release);
            act();
        });
    }
}

/// Reads a heartbeat's task file, through the jail.
///
/// `payload.file` is operator-authored and workspace-relative, so it goes
/// through the jail rather than the filesystem directly — the jail is what
/// turns `../../.ssh/id_rsa` into a refusal instead of a file the heartbeat
/// model then reads aloud. The job's *own* workspace, not the runtime's
/// default: a heartbeat in a named workspace that read the default one's
/// `TASK.md` would skip forever on a file it could not see.
///
/// A missing file is `not_found`, which the scheduler reads as "nothing to do"
/// rather than a fault: an install with no `TASK.md` is the normal case.
///
/// Capped rather than read whole, because this runs every interval forever and
/// a large file would be paid for on each one.
pub async fn read_workspace_file(runtime: &WireRuntime, input: &ReadTaskFile) -> Result<String> {
    let verdict = runtime
        .jails()
        .for_workspace(&input.workspace_id)
        .check(&input.path);
    let accepted = match verdict {
        JailCheck::Accept(accepted) => accepted,
        JailCheck::Reject { message, .. } => {
            return Err(WireError::new(
                ErrorKind::JailEscape,
                format!("Cannot read {}: {message}", input.path),
            )
            .with_detail("path", input.path.clone()));
        }
    };

    let file = match tokio::fs::File::open(&accepted.path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(WireError::new(
                ErrorKind::NotFound,
                format!("No {} in the workspace.", input.path),
            )
            .with_detail("path", input.path.clone()));
        }
        Err(error) => return Err(WireError::from(error)),
    };

    let cap = u64::try_from(input.max_bytes).unwrap_or(u64::MAX);
    let mut body = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut tokio::io::AsyncReadExt::take(file, cap), &mut body)
        .await
        .map_err(WireError::from)?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// The channel manager, and everything needed to build the next one.
///
/// A new manager rather than a restart of the old one, because
/// [`ChannelManager`] fixes its factories at construction and refuses a
/// registration after `start` — which is the property that stops a channel
/// appearing halfway through a process's life without anyone deciding it
/// should. So a settings save, a channel credential, or an extension approval
/// each mean building one from scratch, and this holds the ingredients.
struct ChannelSet {
    hub: Arc<dyn ChannelHub>,
    runtime: Arc<WireRuntime>,
    /// Filled in once the adapter exists: the Telegram console answers through
    /// the same port the REST routes do, and that port is built over a runtime
    /// that is built over this set's own rebuild callback.
    server: parking_lot::RwLock<Option<Arc<dyn ServerRuntime>>>,
    paths: WirePaths,
    env: Env,
    /// Registered before the pumps start, ahead of the built-ins.
    injected: Vec<ChannelFactory>,
    clock: Arc<SystemClock>,
    state: tokio::sync::Mutex<ChannelState>,
}

/// What is running, and why the last rebuild did not leave anything running.
#[derive(Default)]
struct ChannelState {
    manager: Option<Arc<ChannelManager>>,
    /// Held rather than thrown, because a rebuild happens *after* a settings
    /// save has been written and answered. This is what carries the reason to
    /// the panel that caused it.
    error: Option<String>,
}

impl ChannelSet {
    fn fill_server(&self, server: Arc<dyn ServerRuntime>) {
        *self.server.write() = Some(server);
    }

    /// The factories this install has, newest settings first.
    ///
    /// The injected ones lead, an extension's come next and the built-in
    /// Telegram factory is last: an extension's channel is something an
    /// operator installed, and the manager refuses a duplicate id outright, so
    /// the ordering decides nothing except which of two identical ids is
    /// reported first.
    fn factories(&self) -> Result<Vec<ChannelFactory>> {
        let mut factories = self.injected.clone();
        if let Some(host) = self.runtime.extensions() {
            factories.extend(host.channels());
        }
        let Some(server) = self.server.read().clone() else {
            // Only reachable before the adapter is filled in, which is before
            // the first rebuild: an install whose Telegram factory depends on a
            // port that does not exist yet has no bot to start.
            return Ok(factories);
        };
        factories.extend(telegram_factories(&TelegramFactoriesOptions {
            runtime: Arc::clone(&self.runtime),
            server,
            paths: self.paths.clone(),
            env: self.env.clone(),
            new_id: Arc::new(new_id(Arc::clone(&self.clock))),
        })?);
        Ok(factories)
    }

    /// Stops what is running and starts a manager built from the settings as
    /// they now stand.
    ///
    /// `fatal` is the difference between the two callers. At boot a channel
    /// that will not start is a startup error — a bad token should stop the
    /// process rather than leave a channel silently dead. Afterwards it must
    /// not be: the server is already serving, the settings have already been
    /// written and answered, and taking the process down over a mistyped token
    /// in a panel would be a far worse answer than a red line in it.
    async fn rebuild(&self, fatal: bool) -> Result<()> {
        let mut state = self.state.lock().await;
        if let Some(manager) = state.manager.take() {
            manager.stop().await;
        }
        state.error = None;

        let build = self.factories().and_then(|factories| {
            ChannelManager::new(ChannelManagerOptions {
                hub: Arc::clone(&self.hub),
                channels: channel_blocks(&self.runtime.config()),
                factories,
                bus: None,
                bus_options: None,
                clock: Arc::clone(&self.clock) as Arc<dyn Clock>,
                new_id: Arc::new(new_id(Arc::clone(&self.clock))),
                max_sessions: darkwire_channels::DEFAULT_MAX_CHANNEL_SESSIONS,
                workspace_id: None,
            })
        });
        let outcome = match build {
            Ok(manager) => manager.start().await.map(|()| manager),
            Err(error) => Err(error),
        };
        match outcome {
            Ok(manager) => {
                state.manager = Some(Arc::new(manager));
                Ok(())
            }
            Err(error) => {
                state.error = Some(error.message.clone());
                if fatal {
                    return Err(error);
                }
                tracing::error!(%error, "channels could not be restarted");
                Ok(())
            }
        }
    }

    /// What the settings panel shows for every channel this build has.
    fn statuses(&self) -> Vec<ChannelStatus> {
        let state = self.state.try_lock().ok();
        let running = state.as_ref().is_some_and(|state| {
            state
                .manager
                .as_ref()
                .is_some_and(|manager| manager.channel(TELEGRAM_CHANNEL_ID).is_some())
        });
        let start_error = state.as_ref().and_then(|state| state.error.clone());
        vec![telegram_status(&TelegramStatusOptions {
            config: &self.runtime.config(),
            paths: &self.paths,
            env: &self.env,
            running,
            // The manager answers with `Arc<dyn Channel>`, and the username
            // lives on the concrete Telegram channel, which is not recoverable
            // from the trait object. A running bot is still reported as
            // running; it is only the `@name` line that is unavailable.
            username: None,
            start_error,
        })]
    }

    async fn stop(&self) {
        let mut state = self.state.lock().await;
        if let Some(manager) = state.manager.take() {
            manager.stop().await;
        }
    }

    fn channel_ids(&self) -> Vec<String> {
        self.state
            .try_lock()
            .ok()
            .and_then(|state| state.manager.as_ref().map(|manager| manager.channel_ids()))
            .unwrap_or_default()
    }
}

/// One hub connection, as a channel and the scheduler both want it.
struct Bridged {
    client: HubClient,
}

impl Bridged {
    fn receive(&self, frame: &ClientMessage) {
        match serde_json::to_value(frame) {
            Ok(value) => self.client.receive(Frame::Value(value)),
            Err(error) => tracing::warn!(%error, "a client frame could not be encoded"),
        }
    }
}

impl ChannelHubConnection for Bridged {
    fn session_key(&self) -> String {
        self.client.session_key()
    }

    fn receive(&self, frame: ClientMessage) {
        Bridged::receive(self, &frame);
    }

    fn close(&self) {
        self.client.close();
    }
}

impl SchedulerConnection for Bridged {
    fn receive(&self, frame: ClientMessage) {
        Bridged::receive(self, &frame);
    }

    fn close(&self) {
        self.client.close();
    }
}

/// The session hub, as `darkwire-channels` states it.
///
/// The channels crate depends on neither the server nor the runtime, so the
/// hub reaches it as a trait and this is the only place the two are joined.
struct HubBridge {
    hub: Arc<SessionHub>,
}

impl ChannelHub for HubBridge {
    fn connect(&self, options: ChannelHubConnectOptions) -> Arc<dyn ChannelHubConnection> {
        let send = options.send;
        let (client, stream) = self.hub.connect(ConnectOptions {
            session_key: options.session_key,
            channel: options.channel,
            agent_id: options.agent_id,
            // A channel *can* answer an approval — somebody is reading the chat
            // — so it counts as a watcher, unlike a scheduled run.
            unattended: false,
            workspace_id: options.workspace_id,
            max_buffered_bytes: None,
        });
        pump(stream, move |message| send(message));
        Arc::new(Bridged { client })
    }
}

/// A server that is up, and the one call that takes it down again.
pub struct RunningServer {
    /// The bound address, as an operator would type it.
    pub url: String,
    /// The address the listener actually took.
    pub address: SocketAddr,
    /// The one-time code that claims an install with no password.
    pub setup_code: Option<String>,
    /// Where the UI is served from.
    pub ui: UiRoot,
    /// The composition root underneath.
    pub runtime: Arc<WireRuntime>,
    hub: Arc<SessionHub>,
    scheduler: Arc<Scheduler>,
    channels: Arc<ChannelSet>,
    /// Detaches the `tools.changed` producer on the way down.
    tools: (Arc<ToolRegistry>, ListenerId),
    /// The connection every store shares, dropped last.
    ///
    /// Taken rather than left to the struct's own drop: the connection closes
    /// when the last handle goes, and releasing this one *inside* `close` — after
    /// the scheduler, the channels, the hub and the listener have all stopped —
    /// is what keeps the file from being pulled out from under a turn that is
    /// still writing to it.
    database: parking_lot::Mutex<Option<Database>>,
    token: CancellationToken,
    listener: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The sandbox service this process started, when it started one.
    ///
    /// Aborted on the way down rather than awaited: it holds an exclusive lock
    /// on its state directory, and a test that starts a second server in the
    /// same install would otherwise be refused by a service that outlived the
    /// first. Its own containers are reaped by the sweep the next one runs.
    sandbox_service:
        parking_lot::Mutex<Option<Arc<tokio::task::JoinHandle<darkwire_core::Result<()>>>>>,
    closed: parking_lot::Mutex<bool>,
}

impl std::fmt::Debug for RunningServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningServer")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl RunningServer {
    /// The channels that are up, which is empty when a rebuild failed.
    pub fn channel_ids(&self) -> Vec<String> {
        self.channels.channel_ids()
    }

    /// Idempotent, and safe to call from a signal handler.
    ///
    /// Backwards through the construction list: the scheduler stops starting
    /// work, the channels stop accepting, the hub aborts what is running, the
    /// listener closes, and the connection goes last — closing it first would
    /// pull the database out from under a turn that is still writing to it.
    ///
    /// The scheduler goes first *and* is awaited, because it drives its turns
    /// through the hub: stopping the hub underneath an in-flight run would
    /// leave the run row `pending` with nothing to close it.
    pub async fn close(&self) {
        {
            let mut closed = self.closed.lock();
            if *closed {
                return;
            }
            *closed = true;
        }
        self.scheduler.stop();
        self.channels.stop().await;
        // Before the hub, so a registry mutation during the runtime's own close
        // — an MCP server's tools going as it disconnects — does not try to
        // broadcast to sockets that are being torn down.
        let (registry, listener) = &self.tools;
        registry.unsubscribe(*listener);
        self.hub.close();
        self.token.cancel();
        let listener = self.listener.lock().take();
        if let Some(handle) = listener {
            let _ = handle.await;
        }
        self.runtime.close().await;
        if let Some(service) = self.sandbox_service.lock().take() {
            service.abort();
        }
        // Last, and only now: every writer above has stopped.
        drop(self.database.lock().take());
    }
}

/// Everything `start` is injected with, beyond what the flags said.
pub struct ServeOptions {
    /// The parsed flags.
    pub args: ServeArgs,
    /// `--home`, and the log level the globals resolved.
    pub globals: Globals,
    /// The environment to read.
    pub env: Env,
    /// Registered before the pumps start, ahead of the built-ins.
    pub channels: Vec<darkwire_channels::ChannelFactory>,
}

/// The install's paths and its open database, before anything else exists.
///
/// Split out of [`start`] because it is the one step with no dependency on
/// anything the rest of boot builds: it reads the config file only to learn
/// where the database lives, and everything after it works from the runtime's
/// own load rather than this one.
fn open_store(
    globals: &Globals,
    workspace: Option<&str>,
    env: &Env,
) -> Result<(LoadedConfig, Database)> {
    let loaded = load_config(LoadConfigOptions {
        paths: load_options(globals, workspace, env),
        file: None,
    })?;
    if let Some(parent) = loaded.paths.db_file.parent() {
        ensure_dir(parent)?;
    }
    let database = Database::open(&loaded.paths.db_file)?;
    Ok((loaded, database))
}

/// Brings the whole stack up and returns it. Does not block.
///
/// Separate from [`run`] so a test can start a real server, drive it, and shut
/// it down — without a signal handler, a banner, or a process that never
/// returns.
pub async fn start(options: ServeOptions) -> Result<Arc<RunningServer>> {
    let ServeOptions {
        args,
        globals,
        env,
        channels: injected,
    } = options;

    // (1) Read once, here, only for the database path: the runtime loads it
    // again for itself, and the config it ends up with is the one everything
    // else uses.
    let (loaded, database) = open_store(&globals, args.workspaces.as_deref(), &env)?;
    let clock = Arc::new(SystemClock);

    // (2) Before the runtime, because the runtime hands it to the loop.
    //
    // The hub does not exist yet, so the watcher count is filled in after it
    // does: during boot there is no turn, and the safe reading of "I cannot
    // tell" is "somebody is there", which raises nothing rather than raising a
    // notification for every request.
    let watch = Arc::new(LateWatch::default());
    let approvals = build_gate(&watch, &clock);

    // The `automation` tool's reach into the scheduler, late-bound for the same
    // knot: the store it writes through is built with the server, which needs a
    // runtime that is built here.
    let automation = Arc::new(LateAutomation::default());

    // The engine's stand-in, for the same reason and in the same shape.
    let late_scheduler = Arc::new(LateScheduler::default());

    // One vault, shared by the runtime and the adapter, or none at all. Built
    // here rather than in either of them because a credential written over HTTP
    // has to be readable by the next turn: two instances over one file would
    // leave the runtime holding what it read at boot.
    let injected_vault = test_vault(&loaded.paths);

    // (3) The runtime, then the hub over its loop.
    let runtime = create_runtime(RuntimeOptions {
        home: globals.home.clone(),
        workspaces: args.workspaces.clone(),
        approvals: Some(approvals.clone()),
        automation: Some(Arc::clone(&automation) as Arc<dyn darkwire_tools::AutomationResolver>),
        env: Some(env_map(&env)),
        database: Some(database.clone()),
        clock: Some(clock.clone()),
        vault: vault_choice(injected_vault.clone()),
        ..RuntimeOptions::default()
    })?;
    register_test_tools(&runtime);

    // Started after the runtime, because it reads the policy directory the
    // runtime just resolved, and before the first turn, because a turn is what
    // needs it. Nothing is started when an operator has deployed a service of
    // their own; see the module docs.
    let sandbox_service =
        crate::sandbox_service::start_embedded(&loaded.paths, runtime.workspaces(), &env).await;

    // The host is folded into the settings; the port is not, and the asymmetry
    // is the point. The boot policy refuses a non-loopback bind with
    // authentication off, and it reads the config it was handed — so a
    // `--host 0.0.0.0` applied only at bind time would walk straight past the
    // one check that exists to stop it. A port carries no such decision, and
    // `--port 0` is not even expressible in the config, whose schema requires a
    // real port number.
    if let Some(host) = args.host.as_deref() {
        runtime.reconfigure(&serde_json::json!({"server": {"host": host}}))?;
    }

    let config = runtime.config();
    let hub = build_hub(&runtime, &config, &approvals, &clock);
    watch.fill_hub(&hub);

    // The channel set before the adapter, because the adapter rebuilds through
    // it — and filled in with the adapter afterwards, because the Telegram
    // console answers through the adapter. The same late binding the approval
    // gate and the `automation` tool each use, for the same knot.
    let channels = Arc::new(ChannelSet {
        hub: Arc::new(HubBridge {
            hub: Arc::clone(&hub),
        }),
        runtime: Arc::clone(&runtime),
        server: parking_lot::RwLock::new(None),
        paths: loaded.paths.clone(),
        env: env.clone(),
        injected,
        clock: clock.clone(),
        state: tokio::sync::Mutex::new(ChannelState::default()),
    });

    // (4) The adapter, then the UI root, then the server.
    let server_runtime = build_adapter(&runtime, &channels, &env, injected_vault);
    channels.fill_server(Arc::clone(&server_runtime) as Arc<dyn ServerRuntime>);
    let ui = resolve_ui_root(args.ui.as_deref())?;

    let built: WireServer = create_server(ServerOptions {
        config: config.clone(),
        runtime: Arc::clone(&server_runtime) as Arc<dyn darkwire_server::ServerRuntime>,
        hub: Arc::clone(&hub),
        ui: ui.clone(),
        database: database.clone(),
        // The stand-in, filled in below once the engine exists. Handing `None`
        // here is what left `Run now` answering "this build has no scheduler"
        // on an install that has one.
        scheduler: Some(Arc::clone(&late_scheduler) as Arc<dyn SchedulerPort>),
        clock: clock.clone(),
        random: Arc::new(OsRandom),
        password: args.password.clone(),
        username: args.username.clone(),
        hasher: test_hasher(),
    })?;

    // After the server is built, which is where `--password` is applied:
    // minting a code for an install that was just given a password would print
    // a credential nobody needs.
    let setup_code = if config.server.auth.enabled && !built.auth.has_password()? {
        Some(built.auth.issue_setup_code()?)
    } else {
        None
    };

    watch.fill_notifications(Arc::clone(&built.notifications));

    // The producer for `tools.changed`, and the reason it hangs off the
    // *registry* rather than off the MCP manager: the extension host needs the
    // same seam, and the registry is the thing they have in common. A tool list
    // replaced there moves the registry's revision, and every open tab learns
    // without either of them knowing a socket exists.
    //
    // It also closes a gap that predates MCP: switching `exec` off in the
    // settings panel already mutated the registry, and nothing told the browser
    // until the next reload.
    let (registry, listener) = publish_tool_changes(&runtime, &server_runtime, &hub);

    let scheduler = build_scheduler(&runtime, &server_runtime, &built, &hub, &clock);
    late_scheduler.fill(&scheduler);
    fill_automation(&automation, &runtime, &built, &scheduler, &clock);

    // (5) Bind, then write the ready file: the port is not knowable before the
    // bind when `--port 0` asked the OS for one.
    let host = config.server.host.clone();
    let port = args.port.unwrap_or(config.server.port);
    let address = bind_address(&host, port)?;
    let token = CancellationToken::new();
    let (bound, handle) = built.serve(address, token.child_token()).await?;

    // (6) The channels, last, over the hub every other transport shares. Fatal
    // at boot and only at boot: a bad token should stop the process rather than
    // leave a channel silently dead.
    channels.rebuild(true).await?;

    // After the listener, so a job that fires immediately — a missed one-shot
    // the boot sweep picks up — reaches a server that can already answer.
    scheduler.start()?;

    Ok(Arc::new(RunningServer {
        url: format!("http://{bound}"),
        address: bound,
        setup_code,
        ui,
        runtime,
        hub,
        scheduler,
        channels,
        tools: (registry, listener),
        database: parking_lot::Mutex::new(Some(database)),
        token,
        listener: parking_lot::Mutex::new(Some(handle)),
        sandbox_service: parking_lot::Mutex::new(sandbox_service),
        closed: parking_lot::Mutex::new(false),
    }))
}

/// The approval gate, over collaborators that do not exist yet.
///
/// Built before the runtime because the runtime hands it to the loop at
/// construction: without it, a tool whose policy is `ask` runs unattended
/// behind a browser. Both of its hooks read through [`LateWatch`], which is
/// filled in once the hub and the notification store exist.
fn build_gate(watch: &Arc<LateWatch>, clock: &Arc<SystemClock>) -> Arc<HubApprovalGate> {
    let for_watchers = Arc::clone(watch);
    let for_unattended = Arc::clone(watch);
    Arc::new(HubApprovalGate::new(HubApprovalGateOptions {
        clock: Some(Arc::clone(clock) as Arc<dyn Clock>),
        watchers: Some(Arc::new(move |session_key: &str| {
            for_watchers.watchers(session_key)
        })),
        // A scheduled run's prompt goes to an empty room. This is what sends
        // somebody to look at it while it is still open.
        on_unattended: Some(Arc::new(move |approval: UnattendedApproval| {
            for_unattended.announce(&approval);
        })),
    }))
}

/// The runtime as the server's routes want it, with the three channel hooks.
///
/// All three exist because the channel manager fixes its factories at
/// construction, so every write that can move a channel means building a new
/// one — and only the composition root knows a manager exists at all.
fn build_adapter(
    runtime: &Arc<WireRuntime>,
    channels: &Arc<ChannelSet>,
    env: &Env,
    vault: Option<Arc<parking_lot::Mutex<darkwire_security::CredentialVault>>>,
) -> Arc<CliServerRuntime> {
    let for_rebuild = Arc::clone(channels);
    let for_extensions = Arc::clone(channels);
    let for_statuses = Arc::clone(channels);
    CliServerRuntime::new(
        Arc::clone(runtime),
        ServerRuntimeOptions {
            env: env_map(env),
            vault,
            channels_changed: Some(Arc::new(move || {
                let set = Arc::clone(&for_rebuild);
                // Never fatal after boot: the write that caused it has already
                // been written and answered.
                Box::pin(async move {
                    let _ = set.rebuild(false).await;
                })
            })),
            // Approving an extension can bring a channel factory with it.
            extensions_changed: Some(Arc::new(move || {
                let set = Arc::clone(&for_extensions);
                Box::pin(async move {
                    let _ = set.rebuild(false).await;
                })
            })),
            channels: Some(Arc::new(move || for_statuses.statuses())),
            ..ServerRuntimeOptions::default()
        },
    )
}

/// Subscribes the hub to the tool registry, and answers with the way back off.
///
/// The producer for `tools.changed`, and the reason it hangs off the *registry*
/// rather than off the MCP manager: the extension host needs the same seam, and
/// the registry is the thing they have in common. A tool list replaced there
/// moves the registry's revision, and every open tab learns without either of
/// them knowing a socket exists.
///
/// It also closes a gap that predates MCP: switching `exec` off in the settings
/// panel already mutated the registry, and nothing told the browser until the
/// next reload.
fn publish_tool_changes(
    runtime: &Arc<WireRuntime>,
    server_runtime: &Arc<CliServerRuntime>,
    hub: &Arc<SessionHub>,
) -> (Arc<ToolRegistry>, ListenerId) {
    let for_tools = Arc::clone(hub);
    let for_definitions = Arc::clone(server_runtime);
    let registry = Arc::clone(runtime.tools());
    let listener = registry.subscribe(coalesce(Arc::new(move || {
        for_tools.broadcast(&HubEvent::ToolsChanged(ToolsChanged {
            tag: ToolsChangedTag,
            tools: for_definitions.registered_tools(),
        }));
    })));
    (registry, listener)
}

/// The hub, over the runtime's loop and the gate built before it.
///
/// Both collaborators arrive as functions rather than as values, and for two
/// different reasons. The loop, so a settings save moves the *next* turn onto
/// the rebuilt one while the running turn keeps the one it started on — its
/// provider request is in flight and its tool definitions are already in the
/// model's context. The agent resolution, so an agent deleted a moment ago
/// stops resolving rather than living on because the hub was constructed before
/// the delete.
fn build_hub(
    runtime: &Arc<WireRuntime>,
    config: &darkwire_protocol::Config,
    approvals: &Arc<HubApprovalGate>,
    clock: &Arc<SystemClock>,
) -> Arc<SessionHub> {
    let for_loop = Arc::clone(runtime);
    let resolver = Arc::clone(runtime);
    SessionHub::new(SessionHubOptions {
        config: config.clone(),
        loop_for: Arc::new(move |agent_id| {
            Ok(for_loop.loop_for(agent_id)?.map(|one| {
                Arc::new(LoopRunner { agent_loop: one })
                    as Arc<dyn darkwire_server::hub::TurnRunner>
            }))
        }),
        resolve_agent_id: Arc::new(move |agent_id| {
            let config = resolver.config();
            resolve_agent_or_default(&config, agent_id).map_or_else(
                |_| darkwire_server::hub::AgentResolution {
                    agent_id: darkwire_protocol::DEFAULT_AGENT_ID.to_owned(),
                    miss: None,
                },
                |resolution| darkwire_server::hub::AgentResolution {
                    agent_id: resolution.agent.id,
                    miss: resolution.miss.map(miss_reason),
                },
            )
        }),
        store: Arc::clone(runtime.store()),
        approvals: Arc::clone(approvals),
        clock: Some(Arc::clone(clock) as Arc<dyn Clock>),
        new_id: None,
        max_queue_depth: None,
        max_sessions: None,
    })
}

/// The same distinction, in the two crates that each state it for themselves.
///
/// `darkwire-runtime` and `darkwire-server` both name the two ways an agent id
/// can miss, and neither depends on the other — so the mapping is written once
/// here rather than either crate reaching for the other's spelling.
fn miss_reason(miss: darkwire_runtime::AgentMissReason) -> darkwire_server::hub::AgentMissReason {
    match miss {
        darkwire_runtime::AgentMissReason::Unknown => {
            darkwire_server::hub::AgentMissReason::Unknown
        }
        darkwire_runtime::AgentMissReason::Disabled => {
            darkwire_server::hub::AgentMissReason::Disabled
        }
    }
}

/// The automation engine, over the stores the server just built.
///
/// Late-bound the way the plan describes: the engine needs a hub and a runtime,
/// the stores it drives are created while the server is built, and the server
/// needs the runtime. Building it here, after both, is what unties that.
fn build_scheduler(
    runtime: &Arc<WireRuntime>,
    server_runtime: &Arc<CliServerRuntime>,
    built: &WireServer,
    hub: &Arc<SessionHub>,
    clock: &Arc<SystemClock>,
) -> Arc<Scheduler> {
    let jobs = Arc::clone(&built.automation);
    let notifications = Arc::clone(&built.notifications);
    let store = Arc::clone(runtime.store());
    let for_config = Arc::clone(runtime);
    let for_connect = Arc::clone(hub);
    let for_broadcast = Arc::clone(hub);
    let for_chat = Arc::clone(server_runtime);
    let for_read = Arc::clone(runtime);

    Arc::new(Scheduler::new(SchedulerOptions {
        jobs,
        config: Arc::new(move || for_config.config()),
        // Through the hub, not straight to a loop: a job may name a session a
        // browser is also in, and the hub is the only thing that serialises one.
        connect: Arc::new(move |options: SchedulerConnectOptions| {
            let send = options.send;
            let (client, stream) = for_connect.connect(ConnectOptions {
                session_key: Some(options.session_key),
                channel: Some(options.channel),
                agent_id: options.agent_id,
                unattended: options.unattended,
                workspace_id: options.workspace_id,
                max_buffered_bytes: None,
            });
            pump(stream, move |message| send(message));
            Arc::new(Bridged { client }) as Arc<dyn SchedulerConnection>
        }),
        broadcast: Arc::new(move |event| {
            for_broadcast.broadcast(&darkwire_server::hub::HubEvent::Notification(
                NotificationBody {
                    tag: NotificationTag,
                    id: event.id,
                    title: event.title,
                    body: event.body,
                    level: event.level,
                    created_at_ms: event.created_at_ms,
                    session_key: event.session_key,
                    job_id: event.job_id,
                },
            ));
        }),
        raise: Arc::new(move |input| notifications.create(input)),
        delete_session: Some(Arc::new(move |session_key: &str| {
            let _ = store.delete_session(session_key);
        })),
        // The heartbeat's two decisions — whether there is anything to do, and
        // whether what it did worked — are one provider request each, straight
        // rather than through a turn: a loop would write them into the
        // conversation the operator reads.
        chat: Some(Arc::new(move |input: DirectChat| {
            let adapter = Arc::clone(&for_chat);
            Box::pin(async move {
                let request = DirectChatInput {
                    agent_id: input.agent_id,
                    model: input.model,
                    messages: input.messages,
                    tools: input.tools,
                    tool_choice: input.tool_choice,
                    max_tokens: input.max_tokens,
                    token: input.token,
                };
                match adapter.chat(request) {
                    Some(answer) => answer.await,
                    None => Err(WireError::new(
                        ErrorKind::NotFound,
                        "No provider is configured to answer with.",
                    )),
                }
            })
        })),
        read_file: Some(Arc::new(move |input: ReadTaskFile| {
            let runtime = Arc::clone(&for_read);
            Box::pin(async move { read_workspace_file(&runtime, &input).await })
        })),
        clock: Arc::clone(clock) as Arc<dyn Clock>,
        new_id: Box::new(new_id(Arc::clone(clock))),
        run_timeout_ms: None,
    }))
}

/// Closes the loop on the `automation` tool's late binding.
///
/// The real resolver needs the job store the server built and the scheduler
/// that drives it; the loop that calls the tool was constructed before either
/// existed. This is the one call that joins them, and it must run before the
/// listener accepts anything — a turn that reached the tool first would be told
/// nothing can be scheduled on an install where something can.
fn fill_automation(
    automation: &LateAutomation,
    runtime: &Arc<WireRuntime>,
    built: &WireServer,
    scheduler: &Arc<Scheduler>,
    clock: &Arc<SystemClock>,
) {
    let for_timezone = Arc::clone(runtime);
    let for_refresh = Arc::clone(scheduler);
    automation.fill(Arc::new(
        ServerAutomationResolver::new(
            Arc::clone(&built.automation),
            Arc::clone(runtime.store()),
            Arc::new(move || for_timezone.config().ui.timezone.clone()),
            Arc::clone(clock) as Arc<dyn Clock>,
        )
        .with_refresh(Arc::new(move || for_refresh.refresh())),
    ));
}

/// Ids for the rows the hub, the scheduler and the channels mint.
///
/// UUIDv7 off the injected clock and the OS generator, so a test that pauses
/// time still gets distinct ids and nothing here reaches for ambient
/// randomness.
fn new_id(clock: Arc<SystemClock>) -> impl Fn() -> String + Send + Sync + 'static {
    move || {
        let mut bytes = [0u8; 10];
        OsRandom.fill(&mut bytes);
        new_uuid(u64::try_from(clock.now_ms()).unwrap_or(0), &bytes)
    }
}

/// The per-channel settings blocks, as the manager wants them.
///
/// The config models the two known flags as fields and everything else as a
/// loose map; the manager takes one JSON object holding all three, because a
/// channel looks its own block up by id and has never heard of the two flags.
fn channel_blocks(
    config: &darkwire_protocol::Config,
) -> serde_json::Map<String, serde_json::Value> {
    serde_json::to_value(&config.channels)
        .ok()
        .and_then(|value| match value {
            serde_json::Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

/// The address to bind, refused before a listener exists.
fn bind_address(host: &str, port: u16) -> Result<SocketAddr> {
    let ip: IpAddr = host.parse().map_err(|_| {
        WireError::new(
            ErrorKind::Config,
            format!("\"{host}\" is not an address this server can bind."),
        )
        .with_detail("host", host)
    })?;
    Ok(SocketAddr::new(ip, port))
}

/// What an operator needs to know in the second after it starts.
pub fn banner(running: &RunningServer, colors: Option<bool>, t: &Translations) -> String {
    use darkwire_i18n::{args, keys};

    let c = palette_for(colors);
    let config = running.runtime.config();
    let auth_enabled = config.server.auth.enabled;
    let instance = running.runtime.instance();

    let mut rows: Vec<(String, String)> = vec![
        (t.t(keys::serve::URL), c.cyan.apply(&running.url)),
        (
            t.t(keys::serve::AUTH),
            if auth_enabled {
                c.green.apply(&t.t(keys::serve::AUTH_ENABLED))
            } else {
                // Not a warning for its own sake: the boot policy already
                // refused the dangerous version of this, so what is left is a
                // loopback bind that anyone with an account on this machine can
                // drive.
                c.yellow.apply(&t.tr(
                    keys::serve::AUTH_DISABLED,
                    args!["host" => config.server.host.as_str()],
                ))
            },
        ),
        (
            t.t(keys::serve::AGENT),
            match instance {
                Some(instance) if running.runtime.configured() => format!(
                    "{} · {}",
                    darkwire_providers::instance_label(&instance),
                    running.runtime.model()
                ),
                _ => c.yellow.apply(&t.t(keys::serve::AGENT_UNCONFIGURED)),
            },
        ),
        (
            t.t(keys::serve::WORKSPACES),
            running.runtime.paths().workspaces_dir.display().to_string(),
        ),
        (
            t.t(keys::serve::UI),
            match &running.ui {
                UiRoot::Dir(dir) => dir.display().to_string(),
                UiRoot::Embedded => t.t(keys::serve::UI_BUNDLED),
                UiRoot::None => c.dim.apply(&t.t(keys::serve::UI_UNBUILT)),
            },
        ),
    ];

    let channels = running.channel_ids();
    if !channels.is_empty() {
        rows.push((t.t(keys::serve::CHANNELS), channels.join(", ")));
    }

    let width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<String> = rows
        .iter()
        .map(|(label, value)| {
            let padding = " ".repeat(width.saturating_sub(label.chars().count()));
            format!("  {}{padding}  {value}", c.dim.apply(label))
        })
        .collect();
    let body = format!(
        "{}\n\n{}\n",
        c.bold.apply(&t.t(keys::serve::LISTENING)),
        lines.join("\n")
    );

    // Below the table rather than in it, because it is the one thing the
    // operator has to *act* on and a row in a list of five reads as another
    // status line. This is the whole reason the server starts unclaimed instead
    // of refusing: the code is the only way in, and the terminal printing it is
    // the only place it will ever appear.
    let setup = match running.setup_code.as_deref() {
        None => String::new(),
        Some(code) => format!(
            "\n{} {}\n\n      {}\n\n  {}\n",
            c.bold.apply(&t.t(keys::serve::FIRST_RUN)),
            t.t(keys::serve::FIRST_RUN_BODY),
            c.cyan.apply(&c.bold.apply(code)),
            c.dim.apply(&t.t(keys::serve::CODE_ONCE))
        ),
    };

    format!(
        "{body}{setup}\n{}\n",
        c.dim.apply(&t.t(keys::serve::PRESS_CTRL_C))
    )
}

/// Runs `darkwire serve` and stays up until it is asked to stop.
///
/// Answers with an exit code rather than ending the process, like every other
/// subcommand — the listener has to close before the process ends, or the last
/// responses are dropped on a socket that is already gone.
pub async fn run(
    globals: &Globals,
    args: ServeArgs,
    env: &Env,
    streams: &mut Streams,
) -> Result<u8> {
    install_logger(globals.serve_log_level(), env);

    let ready_file = args.ready_file.clone().map(PathBuf::from);
    let json = args.json;
    let colors = globals.color;

    let running = start(ServeOptions {
        args,
        globals: globals.clone(),
        env: env.clone(),
        channels: Vec::new(),
    })
    .await?;

    let record = ReadyRecord {
        port: running.address.port(),
        setup_code: running.setup_code.clone(),
        pid: std::process::id(),
    };
    if let Some(path) = ready_file.as_deref() {
        write_ready_file(path, &record)?;
    }

    // After the server, so the install's own `ui.locale` is available — the
    // same order the chat command uses and for the same reason.
    let t = Translations::for_env(env, Some(&running.runtime.config().ui.locale));

    if json {
        let line = serde_json::to_string(&record).map_err(|error| {
            WireError::new(ErrorKind::Internal, "Could not encode the ready record")
                .with_source(error)
        })?;
        writeln!(streams.out, "{line}").map_err(WireError::from)?;
    } else {
        write!(streams.out, "{}", banner(&running, colors, &t)).map_err(WireError::from)?;
    }
    streams.out.flush().map_err(WireError::from)?;

    wait_for_stop().await;
    // A newline first: Ctrl-C echoes `^C` at the cursor, and the shutdown line
    // would otherwise be appended to it.
    if !json {
        writeln!(streams.out).map_err(WireError::from)?;
    }

    running.close().await;
    if !json {
        writeln!(streams.out, "{}", t.t(darkwire_i18n::keys::serve::STOPPED))
            .map_err(WireError::from)?;
    }
    Ok(0)
}

/// Blocks until the operator, or the supervisor, asks for a stop.
///
/// Both signals, because the two callers are different and neither is optional:
/// `SIGINT` is somebody at a keyboard and `SIGTERM` is a supervisor, and a
/// server that ignored the second would be killed rather than shut down.
async fn wait_for_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(%error, "SIGTERM could not be handled; Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// The channel statuses an install with no channel reports.
///
/// Stated rather than left to an empty vector at the call site, so the reason —
/// a build with no channel wired is a normal build, not a broken one — has
/// somewhere to live.
#[must_use]
pub fn no_channel_statuses() -> Vec<ChannelStatus> {
    Vec::new()
}
