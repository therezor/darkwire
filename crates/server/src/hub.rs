//! `SessionHub` — one session, many connections, one turn at a time.
//!
//! The agent loop runs a turn. It does not know that two browser tabs are open
//! on the same conversation, that the user sent a second message while the
//! first was still running, or that a tab reloaded halfway through a tool call.
//! Those are transport problems, and this is where they are solved — once, for
//! every transport, because a channel that bridges through the hub inherits the
//! same queueing, the same approval gate and the same event stream as the web
//! UI.
//!
//! The design in five decisions:
//!
//!  - **A session runs one turn at a time, and the rest queue.** The loop will
//!    happily run two turns on one session key, and the result is two provider
//!    requests interleaving their writes into one history — which produces a
//!    transcript no model can read and no user can explain. The queue is FIFO,
//!    bounded, and the depth is reported so the UI can say what it is doing.
//!  - **Fanout belongs here, not to the message bus.** That queue is
//!    competing-consumer by design and explicitly does not broadcast; handing
//!    one session's events to it would deliver each event to exactly one of
//!    three open tabs.
//!  - **Sequenced means broadcast.** Every event carrying a `seq` goes to every
//!    subscriber of that session and into the replay ring. There is one `seq`
//!    stream per session and it means the same thing on every connection —
//!    otherwise a client's `last_seq` addresses a different event on reconnect
//!    than it did on the connection that produced it. `connected`, `pong` and
//!    `error` carry no `seq` and are the only frames sent to one client.
//!  - **`AgentEvent` + `seq` *is* `ServerMessage`.** Forwarding a turn is a
//!    counter and a broadcast, not a mapping table: the hub reaches
//!    [`ghostai_agent::AgentEvent::sequenced`] and adds nothing of its own.
//!  - **Nothing an inbound frame contains can escape from here.** Every frame is
//!    parsed fallibly and every failure is an `error` event on the socket that
//!    sent it. A hub that fails on a malformed frame is a hub a client can kill.
//!
//! The turn's cancellation is the one the turn already carries: `turn.stop`
//! cancels the token, and the same token reaches the provider request, the
//! running tool and its child process. There is no second cancellation path.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ghostai_agent::{AgentEvent, TurnInput, TurnResult};
use ghostai_core::ids::DEFAULT_WORKSPACE_ID;
use ghostai_core::messages::Content;
use ghostai_core::messages::{FileDetails, file_part, text_part};
use ghostai_core::session_store::{ReadMessages, StoredMessageRecord, to_stored_message};
use ghostai_core::{Clock, GhostError, Result, SessionStore, SystemClock};
use ghostai_protocol::config::Config;
use ghostai_protocol::messages::{ChatMessage, ContentPart, StopReason};
use ghostai_protocol::uuid::new_uuid;
use ghostai_protocol::ws::{
    Attachment, ClientMessage, ConnectedEvent, ConnectedTag, EditMessage, ErrorCode, ErrorEvent,
    ErrorTag, MessageAck, MessageAckTag, MessageQueued, MessageQueuedTag, Notice, NoticeKind,
    NoticeTag, NotificationBody, PongEvent, PongTag, ProtocolVersion, RegenerateMessage, Sequenced,
    ServerMessage, SessionReplay, SessionReplayTag, SessionReset, SessionResetTag, SessionStatus,
    SessionStatusTag, SessionTruncated, SessionTruncatedTag, Steer, SteerEventTag, ToolsChanged,
    TurnEnd, TurnEndTag, UserMessageRequest,
};
use ghostai_providers::BoxFuture;
use ghostai_security::random::{OsRandom, RandomSource};
use indexmap::IndexMap;
use lru::LruCache;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent_binding::agent_for_turn;
use crate::approvals::HubApprovalGate;
use crate::errors::{code_str, resolve_error};
use crate::replay::ReplayBuffer;
use crate::turn_log::TurnLog;

/// How many messages one session may hold while a turn runs.
///
/// A bound rather than a courtesy: the queue fills from a socket and drains
/// from a loop that may be blocked in a slow tool. Past the cap the hub answers
/// `session_busy` — the one error code in the protocol that exists for exactly
/// this, and the one honest answer to "I cannot hold any more of these".
pub const DEFAULT_MAX_QUEUE_DEPTH: usize = 8;

/// How many sessions keep their replay ring in memory.
///
/// Idle sessions are evicted least-recently-used first past this, and a client
/// that reconnects to an evicted session lands on the same path as one that fell
/// out of the ring: `complete: false`, and a rebuild from storage. A session
/// with a client attached, a running turn or a queue is never evicted — the cap
/// yields to anything live rather than dropping work to satisfy a number, so a
/// hub whose every session is busy holds more than the cap until one goes idle.
pub const DEFAULT_MAX_SESSIONS: usize = 64;

/// How much may sit queued for one connection before it is hung up on.
///
/// A socket that has stopped draining is indistinguishable from one that is
/// merely slow, and the difference does not matter: either way the frames pile
/// up in this process. 1013 — "try again later" — is the honest close code,
/// because the session is fine and the connection is not.
pub const DEFAULT_MAX_BUFFERED_BYTES: usize = 4 * 1024 * 1024;

/// The WebSocket close code for a connection that fell too far behind.
pub const CLOSE_TRY_AGAIN_LATER: u16 = 1013;

/// Stored messages returned when a replay could not cover the gap.
///
/// Enough to rebuild a conversation a user is actually looking at, bounded
/// because this runs on a socket rather than on a paginated route.
/// `complete: false` still says "refetch from REST" — this is what saves the
/// round trip in the common case, not a replacement for the route.
const RESUME_MESSAGE_LIMIT: usize = 200;

/// What an install with no model says, in the one place that says it.
///
/// Two paths report it: a turn that reached the runner and found none, and a
/// regenerate that checks *before* truncating. Both must say the same thing, or
/// the same install describes itself two ways.
const NO_MODEL_MESSAGE: &str = "No model is configured. Add a provider and choose a model in \
     Settings, or run `ghostai init` from a terminal.";

/// Idempotency keys remembered per session.
///
/// A client retrying after a dropped socket resends the last message or two,
/// not the last hundred. The bound is what stops a long-lived session
/// accumulating one entry per message it has ever received.
const MAX_TRACKED_CLIENT_MESSAGE_IDS: usize = 64;

// Seams

/// What the hub needs from a turn that is running.
///
/// Three methods, which is all the hub ever reaches for: read the next event,
/// hold the cancellation a stop frame fires, and collect the outcome.
/// [`ghostai_agent::Turn`] satisfies it as written, and a test drives the hub
/// with a scripted double instead of a provider, a jail, a registry and a store.
pub trait TurnHandle: Send {
    /// The next event, or `None` once the turn has emitted its last.
    fn next_event(&mut self) -> BoxFuture<'_, Option<AgentEvent>>;
    /// The turn's cancellation. Cancelling it stops the turn as a stop frame
    /// would, and the turn still reports its end.
    fn token(&self) -> &CancellationToken;
    /// Drains whatever is left and returns the outcome.
    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<TurnResult>>;
}

impl TurnHandle for ghostai_agent::Turn {
    fn next_event(&mut self) -> BoxFuture<'_, Option<AgentEvent>> {
        Box::pin(ghostai_agent::Turn::next_event(self))
    }

    fn token(&self) -> &CancellationToken {
        ghostai_agent::Turn::token(self)
    }

    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<TurnResult>> {
        Box::pin(ghostai_agent::Turn::finish(*self))
    }
}

/// What the hub needs from an agent loop.
///
/// A structural trait rather than `AgentLoop` itself: the hub uses two of its
/// methods, and stating which two is what keeps the transport testable on its
/// own.
pub trait TurnRunner: Send + Sync {
    /// Starts a turn, whose events the hub forwards and whose cancellation is
    /// the one `turn.stop` fires.
    fn run(&self, input: TurnInput, parent: &CancellationToken) -> Box<dyn TurnHandle>;
    /// Adds a message to the running turn without starting another.
    fn steer(&self, session_key: &str, content: &str);
}

/// Why an id did not name an agent that could run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMissReason {
    /// No agent by that id.
    Unknown,
    /// The agent exists and is switched off.
    Disabled,
}

/// Which agent an id actually names, and whether it is the one asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResolution {
    /// The agent a turn will run on.
    pub agent_id: String,
    /// Set when that is not the agent that was asked for.
    pub miss: Option<AgentMissReason>,
}

/// Resolves the loop a turn runs on.
///
/// A function rather than an instance for two reasons. A reconfigure replaces
/// the loops: a settings save has to move the *next* turn onto the new
/// provider, while the running one keeps the loop it started on — its request
/// is in flight and its tool definitions are already in the model's context.
/// And a turn names an agent, so which loop runs it is a per-turn question.
///
/// `Err` for an id that names an agent which exists and cannot be built, which
/// the hub reports on the one frame that asked for it. `Ok(None)` when no
/// provider and model are configured: the socket stays open and every other
/// frame keeps working, because the client's answer to that is to offer setup
/// rather than to reconnect.
pub type LoopResolver =
    Arc<dyn Fn(Option<&str>) -> Result<Option<Arc<dyn TurnRunner>>> + Send + Sync>;

/// Resolves an agent id to the agent that will actually run.
///
/// A function rather than a snapshot, for the reason [`LoopResolver`] is one: a
/// settings save has to move the *next* turn, and an agent deleted a moment ago
/// must not still resolve because the hub was built before it went.
///
/// Every id reaching this came from somewhere that could not check it — a
/// session row written months ago, a frame from a tab that has been open since
/// before the delete, a channel's configured default. Refusing them would make
/// one settings edit stop conversations that have nothing to do with it, so a
/// miss falls back and is *reported* rather than refused.
pub type AgentResolver = Arc<dyn Fn(Option<&str>) -> AgentResolution + Send + Sync>;

/// Mints turn and connection ids. Injected so a test asserts on stable values.
pub type IdSource = Arc<dyn Fn() -> String + Send + Sync>;

// The outbound side

/// One instruction for the transport that owns the socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    /// A frame to write, already JSON.
    ///
    /// The hub serialises rather than handing over a value, because the byte
    /// budget below has to count something and the transport would encode the
    /// same bytes a moment later anyway.
    Text(String),
    /// Close the socket with this code and stop reading it.
    Close(u16),
}

/// The transport's half of a connection: what to write, in order.
///
/// Handing a frame over is what releases its bytes from the connection's
/// budget, so "buffered" means "accepted by the hub and not yet given to the
/// socket" — which is the quantity that grows without bound when a client stops
/// draining, and the only one the hub can measure on its own.
#[derive(Debug)]
pub struct OutboundStream {
    rx: mpsc::UnboundedReceiver<(Outbound, usize)>,
    buffered: Arc<AtomicUsize>,
}

impl OutboundStream {
    /// The next instruction, or `None` once the hub has detached.
    pub async fn next(&mut self) -> Option<Outbound> {
        let (outbound, bytes) = self.rx.recv().await?;
        self.buffered.fetch_sub(bytes, Ordering::Relaxed);
        Some(outbound)
    }
}

/// One inbound frame, in whichever shape the transport has it.
#[derive(Debug, Clone)]
pub enum Frame {
    /// A JSON string, as a WebSocket text frame carries.
    Text(String),
    /// Bytes, as a WebSocket binary frame carries. Decoded as UTF-8.
    Binary(Vec<u8>),
    /// A value someone already parsed.
    Value(serde_json::Value),
}

// Events

/// A sequenced event as the hub builds it, before the counter is stamped on.
///
/// Two sources: a turn's own events, which arrive as [`AgentEvent`] and are
/// stamped by the agent crate, and the transport's own — an ack, a status, a
/// replay — which exist only here. The `Agent` arm forwards to
/// [`AgentEvent::sequenced`] and adds nothing, which is the whole of "the hub
/// only stamps".
#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "built and consumed in one call; never stored in a collection"
)]
pub enum HubEvent {
    /// A turn's event, forwarded untouched.
    Agent(AgentEvent),
    /// A user message was accepted.
    MessageAck(MessageAck),
    /// A user message is waiting behind a running turn.
    MessageQueued(MessageQueued),
    /// Where a session stands.
    SessionStatus(SessionStatus),
    /// The conversation was cleared.
    SessionReset(SessionReset),
    /// Replayed history for a reconnecting client.
    SessionReplay(SessionReplay),
    /// A suffix of the conversation was dropped.
    SessionTruncated(SessionTruncated),
    /// A notification, addressed to whoever is looking.
    Notification(NotificationBody),
    /// The tool list changed.
    ToolsChanged(ToolsChanged),
    /// Steering, echoed to every tab.
    Steer(Steer),
}

impl HubEvent {
    /// The wire frame this becomes once the session's counter is stamped on.
    pub fn sequenced(self, seq: u64) -> ServerMessage {
        match self {
            // The agent crate owns this conversion, and the hub reaches it
            // rather than repeating it: there is no mapping table here, so
            // there is nowhere for the two shapes to drift apart.
            HubEvent::Agent(event) => event.sequenced(seq),
            HubEvent::MessageAck(event) => ServerMessage::MessageAck(Sequenced { seq, event }),
            HubEvent::MessageQueued(event) => {
                ServerMessage::MessageQueued(Sequenced { seq, event })
            }
            HubEvent::SessionStatus(event) => {
                ServerMessage::SessionStatus(Sequenced { seq, event })
            }
            HubEvent::SessionReset(event) => ServerMessage::SessionReset(Sequenced { seq, event }),
            HubEvent::SessionReplay(event) => {
                ServerMessage::SessionReplay(Sequenced { seq, event })
            }
            HubEvent::SessionTruncated(event) => {
                ServerMessage::SessionTruncated(Sequenced { seq, event })
            }
            HubEvent::Notification(event) => ServerMessage::Notification(Sequenced { seq, event }),
            HubEvent::ToolsChanged(event) => ServerMessage::ToolsChanged(Sequenced { seq, event }),
            HubEvent::Steer(event) => ServerMessage::Steer(Sequenced { seq, event }),
        }
    }
}

// State

/// A connection, as the hub tracks it.
struct Connection {
    tx: mpsc::UnboundedSender<(Outbound, usize)>,
    buffered: Arc<AtomicUsize>,
    max_buffered_bytes: usize,
    channel: String,
    /// Mutable: a `session.new` naming an agent re-points the connection.
    agent_id: Option<String>,
    /// Mutable: a `session.new` naming a workspace re-points the connection.
    workspace_id: Option<String>,
    /// Shared with the [`HubClient`], which reports where the connection is.
    session_key: Arc<Mutex<String>>,
    /// No human on the other end — see [`ConnectOptions::unattended`].
    unattended: bool,
}

impl Connection {
    fn session_key(&self) -> String {
        self.session_key.lock().clone()
    }
}

/// A message accepted but not yet started.
#[derive(Debug, Clone)]
struct QueuedTurn {
    /// Acked to the client, and the `turn_id` the turn will run under.
    id: String,
    content: Content,
    agent_id: Option<String>,
    channel: String,
    /// Only ever creates; an existing session keeps the workspace it is bound
    /// to.
    workspace_id: Option<String>,
}

/// The turn that is running, and how to stop it.
struct RunningTurn {
    turn_id: String,
    token: CancellationToken,
    /// The loop this turn started on, so a steer reaches the loop that is
    /// running it.
    runner: Arc<dyn TurnRunner>,
}

struct SessionState {
    key: String,
    /// Last `seq` emitted. Monotonic for the session's lifetime in this
    /// process.
    seq: u64,
    ring: ReplayBuffer,
    /// The turn that is running, kept whole — the ring answers across turns,
    /// this answers within one. See [`crate::turn_log`] for why one structure
    /// cannot do both.
    turn_log: TurnLog,
    /// Connection ids, in attachment order.
    clients: Vec<String>,
    queue: VecDeque<QueuedTurn>,
    running: Option<RunningTurn>,
    /// `client_message_id` to the id it was acked with, for a retry after a
    /// dropped socket. Insertion-ordered, so the bound drops the oldest.
    acked: IndexMap<String, String>,
}

/// Everything the hub mutates, behind one lock.
struct HubInner {
    sessions: LruCache<String, SessionState>,
    connections: HashMap<String, Connection>,
}

// Options

/// How a connection attaches.
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// The session this connection starts on. A fresh key is minted when
    /// absent.
    pub session_key: Option<String>,
    /// Recorded as the session's origin, so a bridged channel is not labelled
    /// `web`.
    pub channel: Option<String>,
    /// Default agent for turns from this connection; a frame may override it.
    pub agent_id: Option<String>,
    /// There is no human on the other end of this one.
    ///
    /// The scheduler drives its turns *through* the hub — deliberately, because
    /// the hub is the only thing that serialises a session — so a scheduled run
    /// has a connection attached to it like any browser tab. It is not a
    /// watcher though: it forwards events into a collector and has no way to
    /// answer anything. Counting it as one made the watcher count return 1 for
    /// every unattended run, which is precisely the case the approval gate uses
    /// it to detect, so the notification it exists to raise was never raised.
    ///
    /// A flag rather than a channel check: `telegram` is also not a browser and
    /// *can* answer an approval, so "which transport" is the wrong question.
    /// The right one is whether anybody is there, and only the caller knows.
    pub unattended: bool,
    /// The workspace a session *created* by this connection lands in.
    ///
    /// Never applied to a session that already exists — the loop reads the
    /// stored row and ignores this. A tab connects before it has sent anything,
    /// and the store holds no row until the first message lands, so this is
    /// what carries the user's chosen workspace across that gap. Moving a
    /// session that does exist is `PATCH /api/sessions/:key`, never a frame.
    pub workspace_id: Option<String>,
    /// How much may sit queued for this connection before it is hung up on.
    pub max_buffered_bytes: Option<usize>,
}

/// How the hub is built.
#[derive(Clone)]
pub struct SessionHubOptions {
    /// Read for the replay ring and turn-log sizes when a session is first
    /// seen.
    pub config: Config,
    /// The loop to start the next turn on, resolved once per turn.
    pub loop_for: LoopResolver,
    /// Which agent an id actually names.
    pub resolve_agent_id: AgentResolver,
    /// Read only to rebuild a transcript a replay could not cover.
    pub store: Arc<SessionStore>,
    /// Where `tool.approve` lands.
    ///
    /// Constructed by the caller rather than here, because the runtime needs it
    /// at construction and the hub needs the runtime's loop and store. Building
    /// the gate first is what unties that knot.
    pub approvals: Arc<HubApprovalGate>,
    /// The clock the `connected` and `pong` frames report.
    pub clock: Option<Arc<dyn Clock>>,
    /// Turn and connection ids.
    pub new_id: Option<IdSource>,
    /// How many messages one session may hold while a turn runs.
    pub max_queue_depth: Option<usize>,
    /// How many sessions keep their replay ring in memory.
    pub max_sessions: Option<usize>,
}

// The hub

/// Why a turn ended early, and whether anyone saw it open.
///
/// `first_seq` is what the loop reported on `turn.start`, when it got that far:
/// "did anyone see this turn open", which is what decides whether a failure can
/// be closed at an address the client already holds.
struct TurnFailure {
    error: GhostError,
    first_seq: Option<u64>,
}

/// One session, many connections, one turn at a time.
pub struct SessionHub {
    inner: Mutex<HubInner>,
    replay_buffer_size: usize,
    turn_log_max_bytes: usize,
    loop_for: LoopResolver,
    resolve_agent_id: AgentResolver,
    store: Arc<SessionStore>,
    approvals: Arc<HubApprovalGate>,
    clock: Arc<dyn Clock>,
    new_id: IdSource,
    max_queue_depth: usize,
    max_sessions: usize,
}

impl std::fmt::Debug for SessionHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionHub")
            .field("sessions", &self.session_count())
            .finish_non_exhaustive()
    }
}

/// A handle on one attached connection.
pub struct HubClient {
    hub: Arc<SessionHub>,
    id: String,
    session_key: Arc<Mutex<String>>,
}

impl std::fmt::Debug for HubClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubClient")
            .field("id", &self.id)
            .field("session_key", &self.session_key())
            .finish_non_exhaustive()
    }
}

impl HubClient {
    /// The connection's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The session this connection is watching. Moves on `session.switch`.
    pub fn session_key(&self) -> String {
        self.session_key.lock().clone()
    }

    /// Handles one inbound frame. Never fails: a frame this cannot read
    /// becomes an `error` event on the socket that sent it.
    pub fn receive(&self, frame: Frame) {
        self.hub.clone().receive(&self.id, frame);
    }

    /// Detaches. Idempotent, so a socket's close and error can both call it.
    pub fn close(&self) {
        self.hub.disconnect(&self.id);
    }
}

impl SessionHub {
    /// Builds a hub over the loop, store and approval gate it is handed.
    pub fn new(options: SessionHubOptions) -> Arc<SessionHub> {
        let clock = options.clock.unwrap_or_else(|| Arc::new(SystemClock));
        let new_id = options.new_id.unwrap_or_else(|| {
            let clock = Arc::clone(&clock);
            Arc::new(move || {
                let mut random = [0u8; 10];
                OsRandom.fill(&mut random);
                new_uuid(u64::try_from(clock.now_ms()).unwrap_or(0), &random)
            })
        });
        Arc::new(SessionHub {
            inner: Mutex::new(HubInner {
                // Unbounded on purpose: the cap is enforced by the eviction pass
                // below, which refuses to drop a session that is doing
                // something. A bounded cache would evict on insert with no say
                // in which entry goes.
                sessions: LruCache::unbounded(),
                connections: HashMap::new(),
            }),
            replay_buffer_size: usize::try_from(options.config.server.replay_buffer_size)
                .unwrap_or(usize::MAX),
            turn_log_max_bytes: usize::try_from(options.config.server.turn_log_max_bytes)
                .unwrap_or(usize::MAX),
            loop_for: options.loop_for,
            resolve_agent_id: options.resolve_agent_id,
            store: options.store,
            approvals: options.approvals,
            clock,
            new_id,
            max_queue_depth: options.max_queue_depth.unwrap_or(DEFAULT_MAX_QUEUE_DEPTH),
            max_sessions: options.max_sessions.unwrap_or(DEFAULT_MAX_SESSIONS),
        })
    }

    /// Sessions holding state in this process. Includes idle ones, for their
    /// rings.
    pub fn session_count(&self) -> usize {
        self.inner.lock().sessions.len()
    }

    /// Whether a turn is running on this session.
    pub fn busy(&self, session_key: &str) -> bool {
        self.inner
            .lock()
            .sessions
            .peek(session_key)
            .is_some_and(|state| state.running.is_some())
    }

    /// How many clients are looking at this session right now.
    ///
    /// Exists for the approval gate, and the question it really answers is "can
    /// anyone answer a prompt on this session". Zero is the unattended case: a
    /// scheduled run, or a conversation whose tab was closed mid-turn. The gate
    /// turns that into a notification, because a request parked against nobody
    /// is a five-minute wait for a denial that was certain from the start.
    ///
    /// A count rather than a boolean so a caller can tell "nobody" from "one
    /// tab" without a second method.
    pub fn watchers(&self, session_key: &str) -> usize {
        let inner = self.inner.lock();
        let Some(state) = inner.sessions.peek(session_key) else {
            return 0;
        };
        // Not the client count. The scheduler's own connection is in this list
        // for the whole of a run, so counting it made every unattended turn look
        // watched.
        state
            .clients
            .iter()
            .filter(|id| {
                inner
                    .connections
                    .get(*id)
                    .is_some_and(|connection| !connection.unattended)
            })
            .count()
    }

    /// One frame to every attached client, on every session.
    ///
    /// What this exists for: the scheduler raises notifications about turns
    /// nobody started, and there is no session they belong to — a nightly job's
    /// result is addressed to whoever is looking, not to a conversation.
    ///
    /// `seq` is per session, so this stamps each session's own counter rather
    /// than inventing a second sequence space the replay ring would not
    /// understand.
    ///
    /// **Sessions with no client attached are skipped**, and that is the part
    /// that keeps the `seq` contract honest rather than being an optimisation:
    /// bumping a counter nobody is reading would leave a session that reconnects
    /// later resuming at a `last_seq` accounting for an event it was never sent,
    /// which is exactly the gap the replay buffer reports as incomplete.
    ///
    /// The accepted cost is the mirror of that: a tab reconnecting mid-turn
    /// replays the ring and sees the notification a second time. A duplicate
    /// toast is worth less than a false gap.
    pub fn broadcast(&self, event: &HubEvent) {
        let mut inner = self.inner.lock();
        let keys: Vec<String> = inner
            .sessions
            .iter()
            .filter(|(_, state)| !state.clients.is_empty())
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            inner.emit(&key, event.clone());
        }
    }

    /// Attaches a connection and greets it.
    ///
    /// The `connected` frame carries `last_seq` so a fresh client knows where
    /// the session is before it has seen anything, and a reconnecting one can
    /// tell immediately whether it missed anything at all.
    pub fn connect(self: &Arc<Self>, options: ConnectOptions) -> (HubClient, OutboundStream) {
        let (tx, rx) = mpsc::unbounded_channel();
        let buffered = Arc::new(AtomicUsize::new(0));
        let id = (self.new_id)();
        // A fresh tab with no `?session=` gets its key here.
        let session_key = Arc::new(Mutex::new(
            options.session_key.unwrap_or_else(|| (self.new_id)()),
        ));
        let key = session_key.lock().clone();

        let connection = Connection {
            tx,
            buffered: Arc::clone(&buffered),
            max_buffered_bytes: options
                .max_buffered_bytes
                .unwrap_or(DEFAULT_MAX_BUFFERED_BYTES),
            channel: options.channel.unwrap_or_else(|| "web".to_owned()),
            agent_id: options.agent_id,
            workspace_id: options.workspace_id,
            session_key: Arc::clone(&session_key),
            unattended: options.unattended,
        };

        let workspace_id = self
            .stored_workspace(&key)
            .or_else(|| connection.workspace_id.clone())
            .unwrap_or_else(|| DEFAULT_WORKSPACE_ID.to_owned());
        let last_seq = {
            let mut inner = self.inner.lock();
            inner.connections.insert(id.clone(), connection);
            self.session(&mut inner, &key);
            if let Some(state) = inner.sessions.get_mut(&key) {
                state.clients.push(id.clone());
            }
            inner.sessions.peek(&key).map_or(0, |state| state.seq)
        };

        tracing::debug!(connection_id = %id, session_key = %key, "hub connection opened");

        self.inner.lock().deliver(
            &id,
            &ServerMessage::Connected(ConnectedEvent {
                tag: ConnectedTag,
                protocol_version: ProtocolVersion,
                session_key: key,
                server_time_ms: u64::try_from(self.clock.now_ms()).unwrap_or(0),
                last_seq,
                workspace_id,
            }),
        );

        (
            HubClient {
                hub: Arc::clone(self),
                id,
                session_key,
            },
            OutboundStream { rx, buffered },
        )
    }

    /// Stops every turn and drops every session.
    ///
    /// Sockets are not closed here — the transport that opened them owns that,
    /// and a hub that closed them would race the server's own shutdown.
    pub fn close(&self) {
        let keys: Vec<String> = {
            let mut inner = self.inner.lock();
            let keys: Vec<String> = inner.sessions.iter().map(|(key, _)| key.clone()).collect();
            for key in &keys {
                if let Some(state) = inner.sessions.get_mut(key) {
                    if let Some(running) = state.running.take() {
                        running.token.cancel();
                    }
                    state.queue.clear();
                }
            }
            inner.sessions.clear();
            keys
        };
        for key in keys {
            self.approvals.clear_session(&key);
        }
    }

    /// Forgets standing tool approvals for agents that are no longer
    /// configured.
    ///
    /// Called after a settings write, because that is the only moment an agent
    /// can stop existing. The gate is the hub's, so the route reaches it through
    /// here rather than being handed the gate as a second dependency.
    pub fn retain_agents(&self, agent_ids: &[String]) {
        self.approvals.retain_agents(agent_ids);
    }

    /// Carries one agent's standing tool approvals to its new id.
    pub fn rename_agent(&self, from: &str, to: &str) {
        self.approvals.rename_agent(from, to);
    }

    /// Re-announces a session's workspace after something moved it.
    ///
    /// Called by `PATCH /api/sessions/:key`, so a second tab — or the Files page
    /// beside the conversation — learns about the move now rather than at the
    /// next turn. The route performs the write and then says so in one verb; the
    /// hub is not handed the new id, because the status frame re-reads the
    /// stored row itself. Route and hub therefore cannot disagree about what was
    /// written.
    ///
    /// A peek rather than the allocating lookup: the latter creates state with a
    /// replay ring and runs an eviction pass, and a PATCH for a conversation
    /// nobody has open must not bring hub state into existence for it.
    ///
    /// Sessions with no client attached are skipped for the reason
    /// [`SessionHub::broadcast`] gives at length.
    pub fn session_moved(&self, session_key: &str) {
        let status = {
            let inner = self.inner.lock();
            match inner.sessions.peek(session_key) {
                Some(state) if !state.clients.is_empty() => Some(self.status_event(state)),
                _ => return,
            }
        };
        if let Some(event) = status {
            self.inner.lock().emit(session_key, event);
        }
    }

    /// Announces that a session's history is gone.
    ///
    /// Called by `DELETE /api/sessions/:key/messages`, and the same shape as
    /// [`SessionHub::session_moved`] for the same reason: the route performs the
    /// write and then says so in one verb, so route and hub cannot disagree
    /// about what happened. Without it a tab that cleared its own conversation
    /// carries on rendering it, and a second tab never finds out at all.
    ///
    /// `session.reset` rather than a status: a transcript that is now empty is
    /// not something `busy` and `queue_depth` can express, and every client
    /// already knows what the event means.
    pub fn session_cleared(&self, session_key: &str) {
        let mut inner = self.inner.lock();
        let Some(state) = inner.sessions.get_mut(session_key) else {
            return;
        };
        if state.clients.is_empty() {
            return;
        }
        // Before the event, so a resume racing it cannot be handed the frames of
        // a turn whose conversation no longer exists.
        state.turn_log.clear();
        inner.emit(
            session_key,
            HubEvent::SessionReset(SessionReset {
                tag: SessionResetTag,
                session_key: session_key.to_owned(),
            }),
        );
    }

    // Inbound

    fn receive(self: Arc<Self>, connection_id: &str, frame: Frame) {
        let value = match decode_frame(frame) {
            Ok(value) => value,
            Err(message) => {
                self.error(connection_id, ErrorCode::BadRequest, &message, false);
                return;
            }
        };

        let message = match parse_client_message(value) {
            Ok(message) => message,
            Err(message) => {
                self.error(connection_id, ErrorCode::BadRequest, &message, false);
                return;
            }
        };

        self.dispatch(connection_id, message);
    }

    /// Exhaustive on purpose: a client message added without a handler is a
    /// compile error.
    fn dispatch(self: &Arc<Self>, connection_id: &str, message: ClientMessage) {
        match message {
            ClientMessage::Ping(_) => {
                self.inner.lock().deliver(
                    connection_id,
                    &ServerMessage::Pong(PongEvent {
                        tag: PongTag,
                        server_time_ms: u64::try_from(self.clock.now_ms()).unwrap_or(0),
                    }),
                );
            }
            ClientMessage::UserMessage(message) => self.submit(connection_id, &message),
            ClientMessage::Regenerate(message) => self.regenerate(connection_id, &message),
            ClientMessage::Edit(message) => self.edit(connection_id, &message),
            ClientMessage::StopTurn(message) => {
                let mut inner = self.inner.lock();
                // A stop with nothing running is the user clicking as the turn
                // ends. Answering it with an error would be reporting a race as
                // a mistake.
                let Some(state) = inner.sessions.get_mut(&message.session_key) else {
                    return;
                };
                let Some(running) = state.running.as_ref() else {
                    return;
                };
                tracing::info!(
                    session_key = %state.key,
                    turn_id = %running.turn_id,
                    "turn stopped by client"
                );
                running.token.cancel();
            }
            ClientMessage::Steer(message) => {
                let runner = {
                    let mut inner = self.inner.lock();
                    inner
                        .sessions
                        .get_mut(&message.session_key)
                        .and_then(|state| state.running.as_ref())
                        .map(|running| Arc::clone(&running.runner))
                };
                let Some(runner) = runner else {
                    self.error(
                        connection_id,
                        ErrorCode::BadRequest,
                        "No turn is running on this session to steer",
                        false,
                    );
                    return;
                };
                // The loop the turn started on, not the current one: after a
                // reconfigure those differ, and the queue the running loop
                // drains is the only one it will ever read.
                runner.steer(&message.session_key, &message.content);
                self.inner.lock().emit(
                    &message.session_key,
                    HubEvent::Steer(Steer {
                        tag: SteerEventTag,
                        session_key: message.session_key.clone(),
                        content: message.content,
                    }),
                );
            }
            ClientMessage::NewSession(message) => {
                {
                    let mut inner = self.inner.lock();
                    if let Some(connection) = inner.connections.get_mut(connection_id) {
                        // A `session.new` naming a workspace re-points this
                        // connection, so the conversation it is about to start
                        // lands there rather than in whatever the tab was opened
                        // with.
                        if message.workspace_id.is_some() {
                            connection.workspace_id.clone_from(&message.workspace_id);
                        }
                        // And the same for the agent it names. Without this the
                        // field was read off the frame and dropped: the
                        // connection's agent was only ever set at connect time,
                        // so the fallback when a turn is submitted could never
                        // see anything a client chose later. The web UI happens
                        // to resend it on every message, which is what hid it —
                        // a channel does not.
                        if message.agent_id.is_some() {
                            connection.agent_id.clone_from(&message.agent_id);
                        }
                    }
                }
                let key = message.session_key.unwrap_or_else(|| (self.new_id)());
                self.move_to(connection_id, &key);
            }
            ClientMessage::SwitchSession(message) => {
                self.move_to(connection_id, &message.session_key);
            }
            ClientMessage::ResumeSession(message) => {
                self.resume(connection_id, &message.session_key, message.last_seq);
            }
            ClientMessage::ToolApprove(message) => {
                let answered =
                    self.approvals
                        .resolve(&message.call_id, message.approved, message.scope);
                // An unanswered `call_id` is the normal two-tab race, or an
                // answer to a call whose turn was stopped. Neither is worth an
                // error frame.
                if !answered {
                    tracing::debug!(call_id = %message.call_id, "approval answered too late");
                }
            }
        }
    }

    /// Accepts a user message: ack, queue, and start it if nothing is running.
    ///
    /// The ack carries the id the turn will run under. It is deliberately not
    /// the stored message's row id — a queued message has not been persisted
    /// yet, and an ack that waited for persistence would be an ack that waits
    /// for the turn in front of it to finish, which is the one moment the client
    /// needs it.
    fn submit(self: &Arc<Self>, connection_id: &str, message: &UserMessageRequest) {
        {
            let mut inner = self.inner.lock();
            self.session(&mut inner, &message.session_key);
            if let Some(client_message_id) = message.client_message_id.as_deref() {
                let known = inner
                    .sessions
                    .peek(&message.session_key)
                    .and_then(|state| state.acked.get(client_message_id))
                    .cloned();
                if let Some(known) = known {
                    // A retry after a dropped socket. It is acked again, with
                    // the id the first attempt got and no second turn queued —
                    // the ack is idempotent because the client keys on the
                    // message id, and re-acking is what tells a reconnecting tab
                    // its message did land.
                    tracing::debug!(
                        session_key = %message.session_key,
                        client_message_id,
                        "duplicate client message id, not re-queued"
                    );
                    inner.emit(
                        &message.session_key,
                        HubEvent::MessageAck(MessageAck {
                            tag: MessageAckTag,
                            session_key: message.session_key.clone(),
                            message_id: known,
                            client_message_id: Some(client_message_id.to_owned()),
                        }),
                    );
                    return;
                }
            }
        }

        if message.content.is_empty() && message.attachments.is_empty() {
            self.error(
                connection_id,
                ErrorCode::BadRequest,
                "Message is empty",
                false,
            );
            return;
        }

        self.enqueue(
            connection_id,
            &message.session_key,
            to_content(&message.content, &message.attachments),
            message.client_message_id.clone(),
            message.agent_id.clone(),
        );
    }

    /// The queue rules, in the one place that has them.
    ///
    /// Submit, regenerate and edit all end here. Three callers is exactly why
    /// this is one method: the depth cap, the ack and the drain-or-queue
    /// decision are the contract a client renders against, and three copies of
    /// it would be three chances for a retry path to behave unlike the path it
    /// retries.
    fn enqueue(
        self: &Arc<Self>,
        connection_id: &str,
        session_key: &str,
        content: Content,
        client_message_id: Option<String>,
        agent_id: Option<String>,
    ) {
        let queued = {
            let mut inner = self.inner.lock();
            self.session(&mut inner, session_key);
            let depth = inner
                .sessions
                .peek(session_key)
                .map_or(0, |state| state.queue.len());
            if depth >= self.max_queue_depth {
                drop(inner);
                self.error(
                    connection_id,
                    ErrorCode::SessionBusy,
                    &format!("This session already has {depth} messages waiting. Let it catch up."),
                    true,
                );
                return;
            }

            let id = (self.new_id)();
            let (channel, connection_agent, workspace_id) =
                inner.connections.get(connection_id).map_or_else(
                    || ("web".to_owned(), None, None),
                    |connection| {
                        (
                            connection.channel.clone(),
                            connection.agent_id.clone(),
                            connection.workspace_id.clone(),
                        )
                    },
                );

            if let Some(state) = inner.sessions.get_mut(session_key) {
                if let Some(client_message_id) = client_message_id.as_ref() {
                    state.acked.insert(client_message_id.clone(), id.clone());
                    while state.acked.len() > MAX_TRACKED_CLIENT_MESSAGE_IDS {
                        state.acked.shift_remove_index(0);
                    }
                }
                state.queue.push_back(QueuedTurn {
                    id: id.clone(),
                    content,
                    agent_id: agent_id.or(connection_agent),
                    channel,
                    workspace_id,
                });
            }

            inner.emit(
                session_key,
                HubEvent::MessageAck(MessageAck {
                    tag: MessageAckTag,
                    session_key: session_key.to_owned(),
                    message_id: id,
                    client_message_id,
                }),
            );

            let running = inner
                .sessions
                .peek(session_key)
                .is_some_and(|state| state.running.is_some());
            if running {
                let depth = inner
                    .sessions
                    .peek(session_key)
                    .map_or(0, |state| state.queue.len());
                inner.emit(
                    session_key,
                    HubEvent::MessageQueued(MessageQueued {
                        tag: MessageQueuedTag,
                        session_key: session_key.to_owned(),
                        queue_depth: depth as u64,
                    }),
                );
                true
            } else {
                false
            }
        };

        if queued {
            self.announce_status(session_key);
        } else {
            self.drain(session_key);
        }
    }

    /// Re-runs a turn, discarding the answer it produced.
    ///
    /// The guard order is the design, not decoration. **The unconfigured check
    /// comes before the truncation**: discovering there is no model *after*
    /// deleting the answer would destroy what the user had and give nothing
    /// back, and it is the one ordering mistake here that is not recoverable.
    fn regenerate(self: &Arc<Self>, connection_id: &str, message: &RegenerateMessage) {
        if matches!((self.loop_for)(None), Ok(None)) {
            self.error(
                connection_id,
                ErrorCode::NotConfigured,
                NO_MODEL_MESSAGE,
                false,
            );
            return;
        }

        if self.busy_or_queued(&message.session_key) {
            self.error(
                connection_id,
                ErrorCode::SessionBusy,
                "A turn is running on this session. Stop it, then try again.",
                true,
            );
            return;
        }

        let target = match message.seq {
            None => self.last_question(&message.session_key),
            Some(seq) => self.user_message_at(&message.session_key, seq),
        };
        let Some(target) = target else {
            self.error(
                connection_id,
                ErrorCode::BadRequest,
                "There is nothing to regenerate on this session.",
                false,
            );
            return;
        };

        // Read before the delete, because the delete is what removes it.
        let content = match &target.message {
            ChatMessage::User(user) => user.content.clone(),
            _ => Vec::new(),
        };
        self.rewind(&message.session_key, target.seq);
        self.enqueue(
            connection_id,
            &message.session_key,
            Content::Parts(content),
            // Forwarded exactly as an edit does. The rewind above deletes the
            // question and the loop appends it again, so the asking client is
            // showing an optimistic bubble in the gap; this is what the ack uses
            // to claim it.
            message.client_message_id.clone(),
            None,
        );
    }

    /// Replaces a message and re-runs from it. Same guards as a regenerate.
    fn edit(self: &Arc<Self>, connection_id: &str, message: &EditMessage) {
        if matches!((self.loop_for)(None), Ok(None)) {
            self.error(
                connection_id,
                ErrorCode::NotConfigured,
                NO_MODEL_MESSAGE,
                false,
            );
            return;
        }

        if self.busy_or_queued(&message.session_key) {
            self.error(
                connection_id,
                ErrorCode::SessionBusy,
                "A turn is running on this session. Stop it, then try again.",
                true,
            );
            return;
        }

        let seq = i64::try_from(message.seq).unwrap_or(i64::MAX);
        if self
            .user_message_at(&message.session_key, message.seq)
            .is_none()
        {
            self.error(
                connection_id,
                ErrorCode::BadRequest,
                "That message cannot be edited.",
                false,
            );
            return;
        }

        if message.content.is_empty() && message.attachments.is_empty() {
            self.error(
                connection_id,
                ErrorCode::BadRequest,
                "Message is empty",
                false,
            );
            return;
        }

        self.rewind(&message.session_key, seq);
        self.enqueue(
            connection_id,
            &message.session_key,
            to_content(&message.content, &message.attachments),
            message.client_message_id.clone(),
            message.agent_id.clone(),
        );
    }

    /// Drops the question at `seq` and everything after it, then says so.
    ///
    /// **Minus one is load-bearing.** The loop appends the user message
    /// unconditionally at the top of every turn, so truncating *to* `seq` and
    /// then re-running would write the same question twice — once from history
    /// and once from the loop. The question is deleted here and rewritten there.
    fn rewind(&self, session_key: &str, seq: i64) {
        let Ok(result) = self.store.truncate_after(session_key, seq - 1) else {
            return;
        };
        let tail = self.tail(session_key);

        let mut inner = self.inner.lock();
        // The turn the log was holding is part of what was just deleted. Cleared
        // before the event, so a resume racing it cannot be sent frames
        // describing messages that are no longer in the store.
        if let Some(state) = inner.sessions.get_mut(session_key) {
            state.turn_log.clear();
        }
        inner.emit(
            session_key,
            HubEvent::SessionTruncated(SessionTruncated {
                tag: SessionTruncatedTag,
                session_key: session_key.to_owned(),
                up_to_seq: u64::try_from(result.seq).unwrap_or(0),
                // The surviving tail, so a client rebuilds from this frame
                // rather than racing a refetch against the turn that is about to
                // start. Sequenced, so every other attached tab corrects itself
                // with no code of its own.
                messages: tail,
            }),
        );
    }

    /// The stored row at `seq`, if it is a message the user wrote.
    fn user_message_at(&self, session_key: &str, seq: u64) -> Option<StoredMessageRecord> {
        let seq = i64::try_from(seq).unwrap_or(i64::MAX);
        let records = self
            .store
            .messages(
                session_key,
                &ReadMessages {
                    after_seq: Some(seq - 1),
                    before_seq: Some(seq + 1),
                    ..ReadMessages::default()
                },
            )
            .ok()?;
        records
            .into_iter()
            .next()
            .filter(|record| matches!(record.message, ChatMessage::User(_)))
    }

    /// The question that started the most recent turn.
    ///
    /// The *earliest* user row of that turn, not the latest: steering appends
    /// user rows mid-turn under the same turn id, and the last of those is a
    /// correction to the answer rather than the question that asked for it.
    fn last_question(&self, session_key: &str) -> Option<StoredMessageRecord> {
        let tail = self
            .store
            .messages(
                session_key,
                &ReadMessages {
                    limit: Some(RESUME_MESSAGE_LIMIT),
                    from_end: true,
                    ..ReadMessages::default()
                },
            )
            .ok()?;
        let turn_id = tail.last()?.turn_id.clone()?;
        tail.into_iter().find(|record| {
            record.turn_id.as_deref() == Some(turn_id.as_str())
                && matches!(record.message, ChatMessage::User(_))
        })
    }

    fn busy_or_queued(&self, session_key: &str) -> bool {
        // A queued message would otherwise run against a history that is about
        // to change underneath it.
        self.inner
            .lock()
            .sessions
            .peek(session_key)
            .is_some_and(|state| state.running.is_some() || !state.queue.is_empty())
    }

    // Turns

    /// Starts the next queued turn if the session is free.
    fn drain(self: &Arc<Self>, session_key: &str) -> bool {
        let next = {
            let mut inner = self.inner.lock();
            let Some(state) = inner.sessions.get_mut(session_key) else {
                return false;
            };
            if state.running.is_some() {
                return false;
            }
            state.queue.pop_front()
        };
        let Some(next) = next else {
            return false;
        };
        let hub = Arc::clone(self);
        let key = session_key.to_owned();
        tokio::spawn(async move { hub.run_turn(key, next).await });
        true
    }

    /// Runs one turn to completion. Never fails outward.
    ///
    /// A turn this hub started is a turn this hub closes: if the loop fails
    /// instead of emitting `turn.end`, the client is holding an open turn it
    /// will render as a spinner forever, so the failure path emits both the
    /// error and the close.
    async fn run_turn(self: Arc<Self>, session_key: String, turn: QueuedTurn) {
        let outcome = self.open_turn(&session_key, &turn).await;

        {
            let mut inner = self.inner.lock();
            if let Some(state) = inner.sessions.get_mut(&session_key) {
                state.running = None;
            }
        }
        if let Err(failure) = outcome {
            self.fail_turn(&session_key, &turn.id, &failure);
        }
        // The next turn's own `turn.start` and status say the session is busy
        // again; announcing idle first would make a queue look like a gap.
        if !self.drain(&session_key) {
            self.announce_status(&session_key);
        }
    }

    /// The body of a turn, up to the point something goes wrong.
    ///
    /// The `Option<u64>` in the error is the `first_seq` the loop reported on
    /// `turn.start`, when it got that far: "did anyone see this turn open", which
    /// is what decides whether a failure can be closed at an address the client
    /// already holds.
    async fn open_turn(
        self: &Arc<Self>,
        session_key: &str,
        turn: &QueuedTurn,
    ) -> std::result::Result<(), TurnFailure> {
        // An id naming no runnable agent — deleted, switched off, or never real
        // — becomes the default agent rather than a refusal. A conversation must
        // not stop working because an agent it was bound to was deleted, and the
        // binding is left alone, so re-creating that agent silently restores it.
        let stored = self
            .store
            .get_session(session_key)
            .ok()
            .flatten()
            .and_then(|record| record.agent_id);
        let resolver = Arc::clone(&self.resolve_agent_id);
        let requested = agent_for_turn(stored.as_deref(), turn.agent_id.as_deref(), &|agent_id| {
            resolver(Some(agent_id)).miss.is_none()
        });
        let resolution = (self.resolve_agent_id)(requested.as_deref());
        self.announce_fallback(session_key, requested.as_deref(), &resolution);

        let runner = match (self.loop_for)(Some(&resolution.agent_id)) {
            // Not a missing agent any more — the resolver just ruled that out —
            // but an agent that exists and cannot be built, which is a real
            // fault. Reported on the frame that asked for it: the connection is
            // fine, and every other session on it keeps working.
            Err(error) => {
                return Err(TurnFailure {
                    error,
                    first_seq: None,
                });
            }
            Ok(None) => {
                // Not a failure of this turn so much as of the install. It is
                // reported where the turn would have been, and `turn.end` is
                // *not* emitted, because no turn ever started — a client that
                // saw one close would render an empty assistant message for a
                // request nothing ran.
                self.inner.lock().broadcast_to_session(
                    session_key,
                    &ServerMessage::Error(ErrorEvent {
                        tag: ErrorTag,
                        code: ErrorCode::NotConfigured,
                        message: NO_MODEL_MESSAGE.to_owned(),
                        retryable: false,
                        turn_id: Some(turn.id.clone()),
                    }),
                );
                return Ok(());
            }
            Ok(Some(runner)) => runner,
        };

        let token = CancellationToken::new();
        let mut running_turn = runner.run(
            TurnInput {
                session_key: session_key.to_owned(),
                content: turn.content.clone(),
                channel: Some(turn.channel.clone()),
                // The *resolved* id, so the loop that runs and the binding it
                // writes agree. Passing the frame's raw id would let a turn run
                // on the default while the loop recorded a session bound to an
                // agent that does not exist — the exact disagreement
                // `agent_for_turn` exists to prevent.
                //
                // Still conditional on the frame having named one at all: an
                // absent agent means "do not bind", and substituting the default
                // here would turn every unbound conversation into an
                // explicitly-bound one.
                agent_id: turn.agent_id.as_ref().map(|_| resolution.agent_id.clone()),
                workspace_id: turn.workspace_id.clone(),
                turn_id: Some(turn.id.clone()),
                chain: Vec::new(),
                root_session_key: None,
                // A turn a person started has no caller to inherit from.
                inherited_container: None,
            },
            &token,
        );

        {
            let mut inner = self.inner.lock();
            if let Some(state) = inner.sessions.get_mut(session_key) {
                state.running = Some(RunningTurn {
                    turn_id: turn.id.clone(),
                    token: running_turn.token().clone(),
                    runner: Arc::clone(&runner),
                });
            }
            let status = inner
                .sessions
                .peek(session_key)
                .map(|state| self.status_event(state));
            if let Some(status) = status {
                inner.emit(session_key, status);
            }
        }

        // Remembered so a failure below can close the turn at the same address
        // the client already has. The loop opens the turn before anything that
        // can fail, so in practice this is set whenever the loop ran at all.
        let mut opened: Option<u64> = None;
        while let Some(event) = running_turn.next_event().await {
            if let AgentEvent::Nested(ghostai_protocol::ws::NestedAgentEvent::TurnStart(start)) =
                &event
            {
                opened = start.first_seq;
            }
            self.forward(session_key, event);
        }

        match running_turn.finish().await {
            Ok(_) => Ok(()),
            Err(error) => Err(TurnFailure {
                error,
                first_seq: opened,
            }),
        }
    }

    /// Says out loud that the agent a session names is not the one running it.
    ///
    /// Every turn, not once. The fallback is re-decided each time — nothing is
    /// written to make it stick — so a notice that fired once would describe a
    /// state the operator could no longer see. It also matters that they see it:
    /// the default agent may allow tools the departed one did not, so this
    /// widens what the turn can do.
    ///
    /// Deliberately carries **no `turn_id`**. This is a statement about the
    /// conversation's binding rather than about anything the turn did, and the
    /// turn it would name has not started yet — a notice addressed to a turn the
    /// transcript has no item for is one the client silently drops.
    fn announce_fallback(
        &self,
        session_key: &str,
        requested: Option<&str>,
        resolution: &AgentResolution,
    ) {
        let Some(miss) = resolution.miss else {
            return;
        };
        let named = requested.unwrap_or(&resolution.agent_id);
        let running = &resolution.agent_id;
        let message = match miss {
            AgentMissReason::Disabled => format!(
                "This session runs on \"{named}\", which is switched off. Using \"{running}\" \
                 instead."
            ),
            AgentMissReason::Unknown => format!(
                "This session runs on \"{named}\", which no longer exists. Using \"{running}\" \
                 instead."
            ),
        };
        self.inner.lock().emit(
            session_key,
            HubEvent::Agent(AgentEvent::from(Notice {
                tag: NoticeTag,
                kind: NoticeKind::AgentFallback,
                message,
                turn_id: None,
                call_id: None,
            })),
        );
    }

    /// Closes a turn that failed.
    ///
    /// `opened` carries the `first_seq` the loop reported on `turn.start`, when
    /// it got that far. Restating it here is what keeps a failed turn re-runnable
    /// through a reconnect: the client reads the message's own `first_seq` or the
    /// turn's, so a replay that has lost the original `turn.start` out of the
    /// ring buffer would otherwise rebuild a turn with no address and offer no
    /// Regenerate.
    fn fail_turn(&self, session_key: &str, turn_id: &str, failure: &TurnFailure) {
        let TurnFailure { error, first_seq } = failure;
        let first_seq = *first_seq;

        if error.is_aborted() {
            // The loop normally emits `turn.end` with `aborted` itself; a
            // failure here means it unwound before it could, and the turn still
            // has to close.
            self.inner.lock().emit(
                session_key,
                HubEvent::Agent(AgentEvent::from(turn_end(
                    turn_id,
                    StopReason::Aborted,
                    first_seq,
                ))),
            );
            return;
        }

        // The same mapping the REST error handler uses, so one failure cannot be
        // a `provider_error` on a socket and a 500 with a different code on a
        // route. It also decides what is safe to say: an unexpected failure's
        // message was written for a backtrace, not for whoever is connected.
        let retryable = error.retryable;
        let resolved = resolve_error(clone_error(error), true);
        tracing::error!(session_key, turn_id, "turn failed");
        self.inner.lock().broadcast_to_session(
            session_key,
            &ServerMessage::Error(ErrorEvent {
                tag: ErrorTag,
                code: resolved.code,
                message: resolved.message.clone(),
                retryable,
                turn_id: Some(turn_id.to_owned()),
            }),
        );
        self.inner.lock().emit(
            session_key,
            HubEvent::Agent(AgentEvent::from(turn_end(
                turn_id,
                StopReason::Error,
                first_seq,
            ))),
        );
    }

    /// One turn event onto the wire.
    ///
    /// `error` is the only event the protocol leaves unsequenced — it is scoped
    /// to a connection or a turn rather than to a session's replayable history —
    /// so it broadcasts without a counter and never enters the ring.
    fn forward(&self, session_key: &str, event: AgentEvent) {
        if let AgentEvent::Nested(ghostai_protocol::ws::NestedAgentEvent::Error(body)) = event {
            self.inner
                .lock()
                .broadcast_to_session(session_key, &ServerMessage::Error(body));
            return;
        }
        self.inner.lock().emit(session_key, HubEvent::Agent(event));
    }

    // Sessions and replay

    /// Finds or creates a session's state, promoting it to most-recently-used.
    fn session(&self, inner: &mut HubInner, key: &str) {
        if inner.sessions.get_mut(key).is_some() {
            return;
        }
        inner.sessions.put(
            key.to_owned(),
            SessionState {
                key: key.to_owned(),
                seq: 0,
                ring: ReplayBuffer::new(self.replay_buffer_size),
                turn_log: TurnLog::new(self.turn_log_max_bytes),
                clients: Vec::new(),
                queue: VecDeque::new(),
                running: None,
                acked: IndexMap::new(),
            },
        );
        // Excluded from its own eviction: nothing has attached to it yet, so by
        // every measure of "idle" it is the best victim in the map — and
        // evicting the session a client is in the middle of opening would drop
        // the state that call is about to use.
        self.evict(inner, key);
    }

    /// Drops the least-recently-used idle sessions until the cap holds.
    ///
    /// Stops as soon as nothing is idle, which leaves the map over its cap: the
    /// alternative is dropping the replay ring of a conversation someone is
    /// watching, or killing a turn, to satisfy a number that exists to bound
    /// *idle* memory.
    fn evict(&self, inner: &mut HubInner, exclude: &str) {
        while inner.sessions.len() > self.max_sessions {
            let victim = inner
                .sessions
                .iter()
                // LRU end first, which is the same order the old
                // last-touched scan produced.
                .rev()
                .find(|(key, state)| {
                    key.as_str() != exclude
                        && state.clients.is_empty()
                        && state.running.is_none()
                        && state.queue.is_empty()
                })
                .map(|(key, _)| key.clone());
            let Some(victim) = victim else {
                return;
            };
            inner.sessions.pop(&victim);
            self.approvals.clear_session(&victim);
            tracing::debug!(session_key = %victim, "evicted idle session state");
        }
    }

    /// Moves a connection onto another session and reports where it landed.
    fn move_to(self: &Arc<Self>, connection_id: &str, session_key: &str) {
        {
            let mut inner = self.inner.lock();
            self.session(&mut inner, session_key);
            let previous = inner
                .connections
                .get(connection_id)
                .map(Connection::session_key);
            if let Some(previous) = previous
                && previous != session_key
            {
                if let Some(state) = inner.sessions.get_mut(&previous) {
                    state.clients.retain(|id| id != connection_id);
                }
                if let Some(connection) = inner.connections.get(connection_id) {
                    session_key.clone_into(&mut connection.session_key.lock());
                }
                if let Some(state) = inner.sessions.get_mut(session_key) {
                    state.clients.push(connection_id.to_owned());
                }
            }
        }
        self.announce_status(session_key);
    }

    /// Rebuilds a reconnecting client.
    ///
    /// Three answers, and the first two are the original pair. Covered by the
    /// ring: the events after `last_seq` verbatim, which is strictly more than
    /// storage holds — it includes the deltas of a turn still running. Past the
    /// ring with nothing running: the stored tail instead, `complete: false`,
    /// and the client refetches the rest from REST. Never both, because a stored
    /// assistant message and the deltas that produced it are the same text
    /// twice.
    ///
    /// The third is past the ring *while a turn is running*, which is the
    /// ordinary outcome of reloading during a delegation — a subagent spends a
    /// frame per token and the ring is counted in frames. Here the answer is
    /// both, and it is legal because the log names the one turn the two sources
    /// overlap on: `resuming_turn_id` tells the client to drop that turn from the
    /// tail it was just handed and rebuild it from the frames, which are the
    /// whole of it. The rest of the tail is history the frames say nothing about.
    fn resume(self: &Arc<Self>, connection_id: &str, session_key: &str, last_seq: u64) {
        self.move_to(connection_id, session_key);

        let (complete, resuming) = {
            let inner = self.inner.lock();
            let Some(state) = inner.sessions.peek(session_key) else {
                return;
            };
            let complete = state.ring.after(last_seq).complete;
            // Only consulted when the ring falls short: while the ring covers the
            // gap it is already sending these frames, and sending them twice
            // from two places would be the duplicate this whole path exists to
            // avoid.
            //
            // `complete` is the whole question: a log that overran its byte
            // budget holds a *middle*, and replaying a middle over a stored tail
            // would render a turn that began halfway through.
            let resuming = if complete {
                None
            } else if state.turn_log.complete() {
                state.turn_log.open_turn_id().map(str::to_owned)
            } else {
                None
            };
            (complete, resuming)
        };

        let messages = if complete {
            Vec::new()
        } else {
            self.tail(session_key)
        };

        self.inner.lock().emit(
            session_key,
            HubEvent::SessionReplay(SessionReplay {
                tag: SessionReplayTag,
                session_key: session_key.to_owned(),
                messages,
                complete,
                // Only ever alongside `complete: false`: a client told the
                // replay was whole has nothing to rebuild from a second source.
                resuming_turn_id: resuming.clone(),
            }),
        );

        if !complete {
            tracing::info!(
                session_key,
                last_seq,
                resuming_turn_id = ?resuming,
                "resume fell outside the replay buffer"
            );
            if resuming.is_none() {
                return;
            }
        }

        // Re-sends, not new events: the same frames with the same `seq`, to the
        // one connection that missed them. They therefore arrive *after* an
        // envelope carrying a higher number, which is why a client tracks the
        // maximum `seq` it has seen rather than the last one it was handed.
        let frames: Vec<ServerMessage> = {
            let inner = self.inner.lock();
            let Some(state) = inner.sessions.peek(session_key) else {
                return;
            };
            if resuming.is_none() {
                state.ring.after(last_seq).messages
            } else {
                state.turn_log.frames().to_vec()
            }
        };
        let mut inner = self.inner.lock();
        for message in &frames {
            inner.deliver(connection_id, message);
        }
    }

    /// The status frame for a session, read fresh from the stored row.
    fn status_event(&self, state: &SessionState) -> HubEvent {
        HubEvent::SessionStatus(SessionStatus {
            tag: SessionStatusTag,
            session_key: state.key.clone(),
            busy: state.running.is_some(),
            queue_depth: state.queue.len() as u64,
            workspace_id: self
                .stored_workspace(&state.key)
                .unwrap_or_else(|| DEFAULT_WORKSPACE_ID.to_owned()),
            turn_id: state
                .running
                .as_ref()
                .map(|running| running.turn_id.clone()),
        })
    }

    fn announce_status(&self, session_key: &str) {
        let event = {
            let inner = self.inner.lock();
            inner
                .sessions
                .peek(session_key)
                .map(|state| self.status_event(state))
        };
        if let Some(event) = event {
            self.inner.lock().emit(session_key, event);
        }
    }

    /// The workspace a session is bound to, or `None` before its first turn.
    fn stored_workspace(&self, session_key: &str) -> Option<String> {
        self.store
            .get_session(session_key)
            .ok()
            .flatten()
            .map(|record| record.workspace_id)
    }

    /// The last messages of a conversation, as a replay or a truncation reports
    /// them.
    fn tail(&self, session_key: &str) -> Vec<ghostai_protocol::messages::StoredMessage> {
        self.store
            .messages(
                session_key,
                &ReadMessages {
                    limit: Some(RESUME_MESSAGE_LIMIT),
                    from_end: true,
                    ..ReadMessages::default()
                },
            )
            .unwrap_or_default()
            .iter()
            .map(to_stored_message)
            .collect()
    }

    // Outbound

    fn error(&self, connection_id: &str, code: ErrorCode, message: &str, retryable: bool) {
        self.inner.lock().deliver(
            connection_id,
            &ServerMessage::Error(ErrorEvent {
                tag: ErrorTag,
                code,
                message: message.to_owned(),
                retryable,
                turn_id: None,
            }),
        );
    }

    fn disconnect(&self, connection_id: &str) {
        let mut inner = self.inner.lock();
        inner.detach(connection_id);
    }
}

impl HubInner {
    /// Stamps the session's counter on an event and sends it everywhere it
    /// belongs.
    fn emit(&mut self, session_key: &str, event: HubEvent) {
        let Some(state) = self.sessions.get_mut(session_key) else {
            return;
        };
        state.seq += 1;
        let message = event.sequenced(state.seq);
        state.ring.push(message.clone());
        state.turn_log.push(&message);
        self.broadcast_to_session(session_key, &message);
    }

    /// A copy of the list, because a failing send detaches the connection
    /// mid-loop.
    fn broadcast_to_session(&mut self, session_key: &str, message: &ServerMessage) {
        let Some(state) = self.sessions.peek(session_key) else {
            return;
        };
        let clients = state.clients.clone();
        for connection_id in clients {
            self.deliver(&connection_id, message);
        }
    }

    fn deliver(&mut self, connection_id: &str, message: &ServerMessage) {
        let Some(connection) = self.connections.get(connection_id) else {
            return;
        };
        let Ok(text) = serde_json::to_string(message) else {
            // A frame this crate built that will not serialise is an invariant
            // failure, not a client problem; dropping it beats taking down the
            // socket that was about to receive it.
            tracing::error!(connection_id, "could not serialise a server frame");
            return;
        };
        let bytes = text.len();
        let buffered = connection.buffered.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if buffered > connection.max_buffered_bytes {
            tracing::warn!(
                connection_id,
                buffered,
                "connection fell too far behind, closing"
            );
            let _ = connection
                .tx
                .send((Outbound::Close(CLOSE_TRY_AGAIN_LATER), 0));
            self.detach(connection_id);
            return;
        }
        if connection.tx.send((Outbound::Text(text), bytes)).is_err() {
            self.detach(connection_id);
        }
    }

    /// Drops a connection. Idempotent.
    ///
    /// The session state stays: the case a replay buffer exists for is a tab
    /// that reloads, which is a disconnect followed by a reconnect a second
    /// later. Eviction is what eventually reclaims it.
    fn detach(&mut self, connection_id: &str) {
        let Some(connection) = self.connections.remove(connection_id) else {
            return;
        };
        let key = connection.session_key();
        if let Some(state) = self.sessions.get_mut(&key) {
            state.clients.retain(|id| id != connection_id);
        }
        tracing::debug!(connection_id, session_key = %key, "hub connection closed");
    }
}

// Helpers

/// Bytes from a socket, a JSON string, or a value someone already parsed.
fn decode_frame(frame: Frame) -> std::result::Result<serde_json::Value, String> {
    let text = match frame {
        Frame::Value(value) => return Ok(value),
        Frame::Text(text) => text,
        Frame::Binary(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => return Err("Frame is not valid UTF-8".to_owned()),
        },
    };
    serde_json::from_str(&text).map_err(|_| "Frame is not valid JSON".to_owned())
}

/// A client message, or the first complaint short enough to put on a wire.
fn parse_client_message(value: serde_json::Value) -> std::result::Result<ClientMessage, String> {
    let message: ClientMessage = serde_path_to_error::deserialize(value).map_err(|error| {
        let path = error.path().to_string();
        if path.is_empty() || path == "." {
            "Frame did not match any client message".to_owned()
        } else {
            format!("{path}: {}", error.inner())
        }
    })?;
    // The schema's own bounds, which deserialisation does not check: an empty
    // session key parses as a string and addresses no conversation.
    if let Err(report) = garde::Validate::validate(&message) {
        if let Some((path, error)) = report.iter().next() {
            return Err(format!("{path}: {error}"));
        }
        return Err("Frame did not match any client message".to_owned());
    }
    Ok(message)
}

/// What the user sent, as the loop wants it.
///
/// A plain string when there is nothing but text — the common case.
///
/// Every attachment becomes a file part, with no branch on the MIME type. This
/// layer's job is to record *what was attached*, and a path does that for a
/// screenshot and a 200 MB archive alike; deciding what a model can be shown of
/// it needs bytes off the disk and belongs at request time, where the jail is.
/// Branching here is how images ended up carrying a signed URL that expired ten
/// minutes later and that no provider could resolve in the first place.
fn to_content(text: &str, attachments: &[Attachment]) -> Content {
    if attachments.is_empty() {
        return Content::Text(text.to_owned());
    }

    let mut parts: Vec<ContentPart> = Vec::new();
    if !text.is_empty() {
        parts.push(text_part(text));
    }
    for attachment in attachments {
        parts.push(file_part(
            attachment.path.clone(),
            attachment.mime_type.clone(),
            FileDetails {
                name: attachment.name.clone(),
                size_bytes: attachment.size_bytes,
            },
        ));
    }
    Content::Parts(parts)
}

/// A `turn.end` for a turn that did not emit its own.
///
/// `first_seq` is restated when the loop got far enough to report it, which is
/// what keeps a failed turn re-runnable through a reconnect: a replay that has
/// lost the original `turn.start` out of the ring would otherwise rebuild a turn
/// with no storage address and offer no Regenerate.
fn turn_end(turn_id: &str, stop_reason: StopReason, first_seq: Option<u64>) -> TurnEnd {
    TurnEnd {
        tag: TurnEndTag,
        turn_id: turn_id.to_owned(),
        stop_reason,
        usage: None,
        iterations: 0,
        elapsed_ms: None,
        generation_ms: None,
        generation_tokens: None,
        first_token_ms: None,
        first_seq,
        last_seq: None,
    }
}

/// A shallow copy of an error, for the one place that needs to both log it and
/// map it.
fn clone_error(error: &GhostError) -> GhostError {
    GhostError::new(error.kind, error.message.clone())
        .with_retryable(error.retryable)
        .with_details(error.details.clone())
}

/// The wire spelling of an error code, re-exported for the transport's use.
pub fn error_code_str(code: ErrorCode) -> &'static str {
    code_str(code)
}
