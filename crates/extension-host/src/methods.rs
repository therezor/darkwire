//! The `ghostai/` methods: what the host asks an extension, and the three
//! things an extension may say back.
//!
//! MCP supplies tools and nothing else, so everything past `tools/*` is
//! namespaced here. The set is deliberately small and the asymmetry in it is
//! the security argument: the host asks an extension for **five** things, and
//! an extension may ask the host for **one**.
//!
//! ```text
//! host  → ext   ghostai/context/static   {agentId}            → {sections}
//! host  → ext   ghostai/context/runtime  {agentId, sessionKey} → {sections}
//! host  → ext   ghostai/commands/list    {}                    → {commands}
//! host  → ext   ghostai/commands/run     {id, args, sessionKey} → {message, ok}
//! host  → ext   ghostai/channels/list    {}                    → {channels}
//! host  → ext   ghostai/channels/start   {channelId, settings} → {}
//! host  → ext   ghostai/channels/send    {channelId, message}  → {}
//! ext   → host  ghostai/secret           {}                    → {value?}
//! ext  ~> host  ghostai/channels/publish {channelId, ...}      (notification)
//! ext  ~> host  ghostai/channels/control {channelId, frame}    (notification)
//! ```
//!
//! `ghostai/secret` is the only ext→host *request*, and it takes no arguments
//! on purpose: an extension asks for "my secret", never for a namespace and a
//! key, so there is no shape of that call that reads another extension's
//! credential. The two notifications are the inbound half of a channel, and
//! they are notifications because nothing the host would answer is useful — a
//! message that could not be published is a host-side problem the extension
//! cannot act on.
//!
//! Two sharp edges are worth stating where a reader will meet them:
//!
//!  - **`ghostai/channels/list` is not in the original design and had to be.**
//!    A channel is registered as a *factory* keyed by id, and the manager builds
//!    it before anything starts — so the host has to know the ids before the
//!    first `start`. `contributes: ["channels"]` says that there are channels,
//!    not what they are called.
//!  - **The runtime context section is fetched in the static half.** The
//!    contributor trait's runtime half is synchronous and runs on every
//!    iteration; an RPC there would block a worker thread several times a turn.
//!    Both are fetched once per turn under their own caps and the runtime one is
//!    cached per session, which is what the trait's own documentation asks a
//!    contributor that needs I/O to do.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use ghostai_agent::{ContextContributor, RuntimePromptContext, StaticPromptContext};
use ghostai_channels::{
    Channel, ChannelContext, ChannelControl, ChannelControlFrame, ChannelFactory, ChannelInbound,
};
use ghostai_core::message_bus::{OutboundKind, OutboundMessage};
use ghostai_core::{ErrorKind, GhostError, Result};
use ghostai_protocol::{ClientMessage, ContentPart};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::rpc::{RpcClient, RpcError, RpcFailure, RpcHandler};

/// The once-per-session prompt section.
pub const CONTEXT_STATIC: &str = "ghostai/context/static";
/// The per-turn prompt section.
pub const CONTEXT_RUNTIME: &str = "ghostai/context/runtime";
/// The commands an extension serves.
pub const COMMANDS_LIST: &str = "ghostai/commands/list";
/// Running one of them.
pub const COMMANDS_RUN: &str = "ghostai/commands/run";
/// The channels an extension serves.
pub const CHANNELS_LIST: &str = "ghostai/channels/list";
/// Connecting one.
pub const CHANNELS_START: &str = "ghostai/channels/start";
/// Rendering one outbound message on it.
pub const CHANNELS_SEND: &str = "ghostai/channels/send";
/// An inbound message, ext→host.
pub const CHANNELS_PUBLISH: &str = "ghostai/channels/publish";
/// A control frame, ext→host.
pub const CHANNELS_CONTROL: &str = "ghostai/channels/control";
/// This extension's own vault secret, ext→host.
pub const SECRET: &str = "ghostai/secret";
/// MCP's own log notification, which becomes a `tracing` line.
pub const LOG_MESSAGE: &str = "notifications/message";

/// How long a per-turn context section may take before it is skipped.
///
/// A cap rather than a timeout that fails the turn: a slow extension costs its
/// own section and nothing else. See [`ExtensionContributor`].
pub const RUNTIME_CONTEXT_CAP: Duration = Duration::from_secs(1);

/// One section of the system prompt, as an extension writes it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContextSection {
    /// The heading. Empty places the body with no heading.
    #[serde(default)]
    pub title: String,
    /// The prose.
    #[serde(default)]
    pub body: String,
}

/// What both context methods answer.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ContextSections {
    /// In order. Empty places nothing.
    #[serde(default)]
    pub sections: Vec<ContextSection>,
}

impl ContextSections {
    /// The sections as one block, or `None` when there are none worth placing.
    ///
    /// An empty body under a heading is dropped rather than rendered: a heading
    /// with nothing under it reads to a model as a section it failed to
    /// understand.
    pub fn render(&self) -> Option<String> {
        let rendered: Vec<String> = self
            .sections
            .iter()
            .filter(|section| !section.body.trim().is_empty())
            .map(|section| {
                if section.title.trim().is_empty() {
                    section.body.trim().to_owned()
                } else {
                    format!("## {}\n\n{}", section.title.trim(), section.body.trim())
                }
            })
            .collect();
        if rendered.is_empty() {
            None
        } else {
            Some(rendered.join("\n\n"))
        }
    }
}

/// One command, as an extension declares it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandEntry {
    /// `<extensionId>` or `<extensionId>-<suffix>`. Typed after the slash.
    pub id: String,
    /// The line the autocomplete shows.
    #[serde(default)]
    pub description: String,
    /// What to write after the name, in prose. Empty means it takes none.
    #[serde(default)]
    pub args_hint: String,
}

/// What `ghostai/commands/list` answers.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CommandList {
    /// In the order the extension declared them.
    #[serde(default)]
    pub commands: Vec<CommandEntry>,
}

/// What `ghostai/commands/run` answers.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandOutcome {
    /// Shown verbatim. Not a resource key: its copy ships with the extension
    /// and the translation layer has never seen it.
    #[serde(default)]
    pub message: String,
    /// `false` renders it as an error rather than a note.
    #[serde(default = "yes")]
    pub ok: bool,
}

fn yes() -> bool {
    true
}

impl Default for CommandOutcome {
    fn default() -> CommandOutcome {
        CommandOutcome {
            message: String::new(),
            ok: true,
        }
    }
}

/// One channel, as an extension declares it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChannelEntry {
    /// `<extensionId>` or `<extensionId>-<suffix>`.
    pub id: String,
}

/// What `ghostai/channels/list` answers.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChannelList {
    /// In the order the extension declared them.
    #[serde(default)]
    pub channels: Vec<ChannelEntry>,
}

/// An inbound message an extension published on one of its channels.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPublish {
    /// Which of its channels. An id it does not own is dropped.
    pub channel_id: String,
    /// The conversation, before the manager namespaces it.
    #[serde(default)]
    pub session_key: String,
    /// The rate-limiting identity. Per *user*, not per session.
    #[serde(default)]
    pub sender_id: String,
    /// The message.
    #[serde(default)]
    pub content: Vec<ContentPart>,
    /// Channel-specific context: message ids, topic ids, reply targets.
    #[serde(default)]
    pub metadata: serde_json::Map<String, Value>,
    /// The channel's own idempotency key, when it has one.
    #[serde(default)]
    pub id: Option<String>,
}

/// A control frame an extension sent on one of its channels.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelControlNotification {
    /// Which of its channels.
    pub channel_id: String,
    /// The conversation it is about.
    #[serde(default)]
    pub session_key: String,
    /// Where a reply this produces goes. Defaults to `session_key`.
    #[serde(default)]
    pub target: Option<String>,
    /// The frame, spelled exactly as a browser would send it.
    pub frame: ClientMessage,
}

/// What an extension may say that is not a message somebody typed.
///
/// Five of the ten client frames, and the five are the manager's own list. The
/// three session-moving ones are absent for the reason the channel contract
/// gives: a channel changes conversation by publishing a different session key,
/// so a frame that moved the connection would leave the two halves disagreeing.
pub fn control_frame_of(
    message: ClientMessage,
) -> std::result::Result<ChannelControlFrame, String> {
    match message {
        ClientMessage::ToolApprove(body) => Ok(ChannelControlFrame::ToolApprove(body)),
        ClientMessage::StopTurn(body) => Ok(ChannelControlFrame::StopTurn(body)),
        ClientMessage::Steer(body) => Ok(ChannelControlFrame::Steer(body)),
        ClientMessage::Regenerate(body) => Ok(ChannelControlFrame::Regenerate(body)),
        ClientMessage::Edit(body) => Ok(ChannelControlFrame::Edit(body)),
        other => Err(format!(
            "The frame \"{}\" is not one a channel may send.",
            client_tag(&other)
        )),
    }
}

fn client_tag(message: &ClientMessage) -> &'static str {
    match message {
        ClientMessage::Ping(_) => "ping",
        ClientMessage::UserMessage(_) => "user.message",
        ClientMessage::Regenerate(_) => "turn.regenerate",
        ClientMessage::Edit(_) => "user.edit",
        ClientMessage::StopTurn(_) => "turn.stop",
        ClientMessage::NewSession(_) => "session.new",
        ClientMessage::SwitchSession(_) => "session.switch",
        ClientMessage::ResumeSession(_) => "session.resume",
        ClientMessage::ToolApprove(_) => "tool.approve",
        ClientMessage::Steer(_) => "turn.steer",
    }
}

/// One outbound message on the wire, for an extension to render.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutboundOnWire<'a> {
    id: &'a str,
    session_key: &'a str,
    target: &'a str,
    content: &'a [ContentPart],
    kind: &'static str,
    created_at_ms: i64,
    metadata: &'a serde_json::Map<String, Value>,
}

fn kind_tag(kind: OutboundKind) -> &'static str {
    match kind {
        OutboundKind::Reply => "reply",
        OutboundKind::Progress => "progress",
        OutboundKind::Notice => "notice",
        OutboundKind::Error => "error",
    }
}

/// Where a secret comes from, injected so nothing here holds a vault.
///
/// The host closes over the credential store and hands one of these per
/// extension, already bound to that extension's id — which is what makes
/// `ghostai/secret` unable to name anyone else's.
pub type SecretFn = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// The host side of one extension's connection.
///
/// Serves the one request an extension may make and routes the two
/// notifications it may send. Channel contexts arrive as each channel is built,
/// which is why they sit behind a lock rather than being constructor arguments:
/// the connection exists before any channel does.
pub struct HostMethods {
    id: String,
    secret: Option<SecretFn>,
    channels: Mutex<HashMap<String, ChannelContext>>,
}

impl std::fmt::Debug for HostMethods {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostMethods")
            .field("id", &self.id)
            .field("channels", &self.channels.lock().len())
            .finish_non_exhaustive()
    }
}

impl HostMethods {
    /// The host side for one extension.
    pub fn new(id: impl Into<String>, secret: Option<SecretFn>) -> Arc<HostMethods> {
        Arc::new(HostMethods {
            id: id.into(),
            secret,
            channels: Mutex::new(HashMap::new()),
        })
    }

    /// Binds a channel's context, so what the extension publishes on that id
    /// reaches the bus under it.
    pub fn bind_channel(&self, context: ChannelContext) {
        self.channels.lock().insert(context.id.clone(), context);
    }

    /// Forgets every bound channel. Called when the extension is unloaded.
    pub fn clear_channels(&self) {
        self.channels.lock().clear();
    }

    fn context_for(&self, channel_id: &str) -> Option<ChannelContext> {
        self.channels.lock().get(channel_id).cloned()
    }

    fn publish(&self, params: Value) {
        let published: ChannelPublish = match serde_json::from_value(params) {
            Ok(published) => published,
            Err(error) => {
                tracing::warn!(
                    target: "extension",
                    extension = %self.id,
                    error = %error,
                    "an extension published a message this host could not read"
                );
                return;
            }
        };
        let Some(context) = self.context_for(&published.channel_id) else {
            // A channel id the extension does not own, or one that is not
            // running. Dropped rather than routed anywhere: the id is how the
            // bus decides whose message this is.
            tracing::warn!(
                target: "extension",
                extension = %self.id,
                channel = %published.channel_id,
                "an extension published on a channel it does not have running"
            );
            return;
        };
        context.publish(ChannelInbound {
            session_key: published.session_key,
            sender_id: published.sender_id,
            content: published.content,
            metadata: published.metadata,
            id: published.id,
        });
    }

    fn control(&self, params: Value) {
        let notification: ChannelControlNotification = match serde_json::from_value(params) {
            Ok(notification) => notification,
            Err(error) => {
                tracing::warn!(
                    target: "extension",
                    extension = %self.id,
                    error = %error,
                    "an extension sent a control frame this host could not read"
                );
                return;
            }
        };
        let Some(context) = self.context_for(&notification.channel_id) else {
            tracing::warn!(
                target: "extension",
                extension = %self.id,
                channel = %notification.channel_id,
                "an extension controlled a channel it does not have running"
            );
            return;
        };
        match control_frame_of(notification.frame) {
            Ok(frame) => context.control(ChannelControl {
                session_key: notification.session_key,
                target: notification.target,
                frame,
            }),
            Err(problem) => tracing::warn!(
                target: "extension",
                extension = %self.id,
                problem,
                "an extension sent a frame a channel may not send"
            ),
        }
    }

    fn log(&self, params: &Value) {
        let level = params
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("info")
            .to_owned();
        let text = params
            .get("data")
            .map(|data| match data.as_str() {
                Some(text) => text.to_owned(),
                None => data.to_string(),
            })
            .unwrap_or_default();
        // One constant target with the id as a field, because `tracing`'s
        // target must be a literal. A filter selects one extension with
        // `extension=hello`, which is what a dynamic target would have bought.
        match level.as_str() {
            "debug" | "notice" => {
                tracing::debug!(target: "extension", extension = %self.id, "{text}");
            }
            "warning" => {
                tracing::warn!(target: "extension", extension = %self.id, "{text}");
            }
            "error" | "critical" | "alert" | "emergency" => {
                tracing::error!(target: "extension", extension = %self.id, "{text}");
            }
            _ => {
                tracing::info!(target: "extension", extension = %self.id, "{text}");
            }
        }
    }
}

impl RpcHandler for HostMethods {
    fn request(
        &self,
        method: String,
        _params: Value,
    ) -> BoxFuture<'_, std::result::Result<Value, RpcError>> {
        Box::pin(async move {
            if method == SECRET {
                // No arguments, deliberately: an extension asks for *its*
                // secret. There is no shape of this call that names another.
                let value = self.secret.as_ref().and_then(|read| read());
                return Ok(json!({"value": value}));
            }
            Err(RpcError::method_not_found(&method))
        })
    }

    fn notify(&self, method: String, params: Value) {
        match method.as_str() {
            CHANNELS_PUBLISH => self.publish(params),
            CHANNELS_CONTROL => self.control(params),
            LOG_MESSAGE => self.log(&params),
            other => tracing::debug!(
                target: "extension",
                extension = %self.id,
                method = other,
                "ignoring a notification this host does not serve"
            ),
        }
    }
}

/// Asks an extension for a list, with the result parsed.
///
/// `Ok(None)` is the peer answering `-32601`: it does not serve that method,
/// which is information the host acts on rather than an error.
pub async fn list<T: serde::de::DeserializeOwned + Default>(
    client: &RpcClient,
    method: &str,
) -> std::result::Result<Option<T>, RpcFailure> {
    match client.request(method, json!({})).await {
        Ok(value) => Ok(Some(serde_json::from_value(value).unwrap_or_default())),
        Err(failure) if failure.is_method_not_found() => Ok(None),
        Err(failure) => Err(failure),
    }
}

/// Runs one command, with cancellation delivered as MCP's own notification.
///
/// A command that fails is a *result*, never a propagated error: an extension's
/// bug should read as "that did not work" in the composer, not as a 500.
pub async fn run_command(
    client: &RpcClient,
    id: &str,
    args: &str,
    session_key: Option<&str>,
    token: &CancellationToken,
) -> CommandOutcome {
    let params = json!({"id": id, "args": args, "sessionKey": session_key});
    match client
        .request_cancellable(COMMANDS_RUN, params, token)
        .await
    {
        Ok(value) => serde_json::from_value(value).unwrap_or_default(),
        Err(failure) => CommandOutcome {
            message: failure.message(),
            ok: false,
        },
    }
}

/// The prompt sections one extension contributes.
///
/// The split between the two halves is the trait's, and out of process it is
/// load-bearing rather than advisory: `runtime_section` is synchronous and runs
/// on every iteration of every turn, so an RPC there would block a worker
/// thread five or ten times a turn. Both methods are called from the async half
/// — the runtime one under a one-second cap — and the runtime answer is cached
/// per session for the iterations that follow.
///
/// A runtime call that times out places **no section** and adds a warning, and
/// never fails the turn. That is the whole of the contract: an extension that
/// has gone slow costs its own paragraph and nothing else.
pub struct ExtensionContributor {
    id: String,
    client: Arc<RpcClient>,
    cap: Duration,
    runtime: Mutex<HashMap<String, String>>,
    warnings: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for ExtensionContributor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionContributor")
            .field("id", &self.id)
            .field("cached_sessions", &self.runtime.lock().len())
            .finish_non_exhaustive()
    }
}

impl ExtensionContributor {
    /// A contributor over one extension's connection.
    pub fn new(
        id: impl Into<String>,
        client: Arc<RpcClient>,
        warnings: Arc<Mutex<Vec<String>>>,
    ) -> ExtensionContributor {
        ExtensionContributor {
            id: id.into(),
            client,
            cap: RUNTIME_CONTEXT_CAP,
            runtime: Mutex::new(HashMap::new()),
            warnings,
        }
    }

    /// Shortens the runtime cap, for a test that must not wait a second.
    #[must_use]
    pub fn with_cap(mut self, cap: Duration) -> ExtensionContributor {
        self.cap = cap;
        self
    }

    /// Drops every cached runtime section. Called when the extension unloads.
    pub fn forget(&self) {
        self.runtime.lock().clear();
    }

    async fn sections(&self, method: &str, params: Value) -> Option<String> {
        match self.client.request(method, params).await {
            Ok(value) => serde_json::from_value::<ContextSections>(value)
                .unwrap_or_default()
                .render(),
            // An extension that declares `context` and serves only one of the
            // two methods is the ordinary case, not a problem: the probe at
            // load time already warned if it served neither.
            Err(failure) if failure.is_method_not_found() => None,
            Err(failure) => {
                self.warnings.lock().push(format!(
                    "The context method \"{method}\" failed: {}",
                    failure.message()
                ));
                None
            }
        }
    }
}

impl ContextContributor for ExtensionContributor {
    fn name(&self) -> &str {
        &self.id
    }

    fn static_section<'a>(
        &'a self,
        context: &'a StaticPromptContext,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let agent_id = context.agent_id.clone().unwrap_or_default();
            let session_key = context.session_key.clone();

            // The per-turn half, fetched here and read from the cache by the
            // synchronous half below. Capped, and a cap that expires leaves the
            // cache as it was rather than clearing it: the previous section is
            // stale by one turn, which is better than a prompt that flickers.
            let runtime = tokio::time::timeout(
                self.cap,
                self.sections(
                    CONTEXT_RUNTIME,
                    json!({"agentId": agent_id, "sessionKey": session_key}),
                ),
            )
            .await;
            match runtime {
                Ok(Some(section)) => {
                    self.runtime.lock().insert(session_key.clone(), section);
                }
                Ok(None) => {
                    self.runtime.lock().remove(&session_key);
                }
                Err(_) => self.warnings.lock().push(format!(
                    "The runtime context section took longer than {} ms and was skipped.",
                    self.cap.as_millis()
                )),
            }

            self.sections(CONTEXT_STATIC, json!({"agentId": agent_id}))
                .await
        })
    }

    fn runtime_section(&self, context: &RuntimePromptContext) -> Option<String> {
        self.runtime
            .lock()
            .get(&context.static_context.session_key)
            .cloned()
    }
}

/// One channel, served over the extension's own connection.
struct ExtensionChannel {
    id: String,
    client: Arc<RpcClient>,
    settings: serde_json::Map<String, Value>,
}

impl Channel for ExtensionChannel {
    fn id(&self) -> &str {
        &self.id
    }

    fn start(&self) -> ghostai_channels::BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.client
                .request(
                    CHANNELS_START,
                    json!({"channelId": self.id, "settings": self.settings}),
                )
                .await
                .map(|_| ())
                .map_err(|failure| {
                    GhostError::from(failure).with_detail("channel", self.id.as_str())
                })
        })
    }

    fn send(&self, message: OutboundMessage) -> ghostai_channels::BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let wire = OutboundOnWire {
                id: &message.id,
                session_key: &message.session_key,
                target: &message.target,
                content: &message.content,
                kind: kind_tag(message.kind),
                created_at_ms: message.created_at_ms,
                metadata: &message.metadata,
            };
            let params = json!({"channelId": self.id, "message": wire});
            self.client
                .request(CHANNELS_SEND, params)
                .await
                .map(|_| ())
                .map_err(|failure| {
                    GhostError::from(failure).with_detail("channel", self.id.as_str())
                })
        })
    }
}

/// A factory for one of an extension's channels.
///
/// The context the manager hands over is bound into [`HostMethods`] as the
/// channel is built, which is what lets an inbound `ghostai/channels/publish`
/// find the bus. It is bound at *build* time rather than at start time because
/// an extension may publish the moment it is started.
pub fn channel_factory(id: &str, client: Arc<RpcClient>, host: Arc<HostMethods>) -> ChannelFactory {
    let channel_id = id.to_owned();
    ChannelFactory::new(
        id,
        Arc::new(move |context: ChannelContext| {
            host.bind_channel(context.clone());
            Ok(Arc::new(ExtensionChannel {
                id: channel_id.clone(),
                client: Arc::clone(&client),
                settings: context
                    .settings
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            }) as Arc<dyn Channel>)
        }),
    )
}

/// The error a caller gets when no extension serves the command they named.
pub fn no_such_command(id: &str) -> GhostError {
    GhostError::new(ErrorKind::NotFound, format!("No command called \"{id}\""))
        .with_detail("id", id)
}
