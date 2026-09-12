//! `SessionHub` — one session, many connections, one turn at a time.
//!
//! The hub's whole job is what happens *between* a turn's events — a second
//! message arriving mid-turn, a stop, a reload — so the turn here is
//! suspendable at any point rather than replayed from a fixed script.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ghostai_agent::{AgentEvent, TurnInput, TurnResult};
use ghostai_core::messages::Content;
use ghostai_core::messages::{AssistantOptions, assistant_message, user_message};
use ghostai_core::session_store::AppendOptions;
use ghostai_core::{Database, ErrorKind, GhostError, Result, SessionStore, SystemClock};
use ghostai_protocol::config::Config;
use ghostai_protocol::messages::{ChatMessage, StopReason, Usage};
use ghostai_protocol::tools::{ApprovalScope, ToolRisk};
use ghostai_protocol::ws::{
    AssistantDelta, AssistantDeltaTag, ErrorCode, ErrorEvent, ErrorTag, NoticeKind, NoticeTag,
    NotificationTag, PROTOCOL_VERSION, ServerMessage, ToolApprovalRequest, ToolApprovalRequestTag,
    TurnEnd, TurnEndTag, TurnStart, TurnStartTag,
};
use ghostai_providers::BoxFuture;
use ghostai_server::approvals::{HubApprovalGate, HubApprovalGateOptions};
use ghostai_server::hub::{
    AgentMissReason, AgentResolution, ConnectOptions, Frame, HubClient, HubEvent, Outbound,
    SessionHub, SessionHubOptions, TurnHandle, TurnRunner,
};
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

const SESSION: &str = "web:1";

// A scripted turn

/// One `run()` call, driven from the test.
struct TurnControl {
    input: TurnInput,
    events: Mutex<Option<mpsc::UnboundedSender<AgentEvent>>>,
    result: Mutex<Option<oneshot::Sender<Result<TurnResult>>>>,
    token: CancellationToken,
}

impl TurnControl {
    fn turn_id(&self) -> String {
        self.input
            .turn_id
            .clone()
            .unwrap_or_else(|| "unknown".to_owned())
    }

    fn emit(&self, event: impl Into<AgentEvent>) {
        if let Some(tx) = self.events.lock().as_ref() {
            let _ = tx.send(event.into());
        }
    }

    fn delta(&self, text: &str) {
        self.emit(AssistantDelta {
            tag: AssistantDeltaTag,
            turn_id: self.turn_id(),
            text: text.to_owned(),
        });
    }

    fn start(&self) {
        self.emit(TurnStart {
            tag: TurnStartTag,
            session_key: self.input.session_key.clone(),
            turn_id: self.turn_id(),
            first_seq: None,
            agent_id: "default".to_owned(),
            model: "test-model".to_owned(),
            provider: "test".to_owned(),
        });
    }

    /// Ends the turn the way the loop does: a `turn.end`, then the outcome.
    fn end(&self) {
        self.end_with(StopReason::Complete);
    }

    fn end_with(&self, stop_reason: StopReason) {
        self.emit(TurnEnd {
            tag: TurnEndTag,
            turn_id: self.turn_id(),
            stop_reason,
            usage: None,
            iterations: 1,
            elapsed_ms: None,
            generation_ms: None,
            generation_tokens: None,
            first_token_ms: None,
            first_seq: None,
            last_seq: None,
        });
        self.settle(Ok(TurnResult {
            turn_id: self.turn_id(),
            stop_reason,
            iterations: 1,
            usage: Usage::default(),
            text: String::new(),
        }));
    }

    /// Unwinds without a `turn.end`, which is what a loop that fails does.
    fn fail(&self, error: GhostError) {
        self.settle(Err(error));
    }

    fn settle(&self, outcome: Result<TurnResult>) {
        *self.events.lock() = None;
        if let Some(tx) = self.result.lock().take() {
            let _ = tx.send(outcome);
        }
    }
}

struct ScriptedHandle {
    events: mpsc::UnboundedReceiver<AgentEvent>,
    result: Option<oneshot::Receiver<Result<TurnResult>>>,
    token: CancellationToken,
}

impl TurnHandle for ScriptedHandle {
    fn next_event(&mut self) -> BoxFuture<'_, Option<AgentEvent>> {
        Box::pin(self.events.recv())
    }

    fn token(&self) -> &CancellationToken {
        &self.token
    }

    fn finish(mut self: Box<Self>) -> BoxFuture<'static, Result<TurnResult>> {
        Box::pin(async move {
            while self.events.recv().await.is_some() {}
            match self.result.take() {
                Some(rx) => rx
                    .await
                    .unwrap_or_else(|_| Err(GhostError::aborted("Turn"))),
                None => Err(GhostError::aborted("Turn")),
            }
        })
    }
}

#[derive(Default)]
struct ScriptedRunner {
    turns: Mutex<Vec<Arc<TurnControl>>>,
    steers: Mutex<Vec<(String, String)>>,
}

impl ScriptedRunner {
    fn turn(&self, index: usize) -> Arc<TurnControl> {
        self.turns
            .lock()
            .get(index)
            .cloned()
            .unwrap_or_else(|| panic!("no turn {index} has started"))
    }

    fn count(&self) -> usize {
        self.turns.lock().len()
    }
}

impl TurnRunner for ScriptedRunner {
    fn run(&self, input: TurnInput, _parent: &CancellationToken) -> Box<dyn TurnHandle> {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (result_tx, result_rx) = oneshot::channel();
        let token = CancellationToken::new();
        self.turns.lock().push(Arc::new(TurnControl {
            input,
            events: Mutex::new(Some(events_tx)),
            result: Mutex::new(Some(result_tx)),
            token: token.clone(),
        }));
        Box::new(ScriptedHandle {
            events: events_rx,
            result: Some(result_rx),
            token,
        })
    }

    fn steer(&self, session_key: &str, content: &str) {
        self.steers
            .lock()
            .push((session_key.to_owned(), content.to_owned()));
    }
}

// Harness

/// Every frame one connection was sent, plus the close code if it got one.
struct TestClient {
    client: HubClient,
    frames: Arc<Mutex<Vec<ServerMessage>>>,
    closed: Arc<Mutex<Option<u16>>>,
}

impl TestClient {
    fn frames(&self) -> Vec<ServerMessage> {
        self.frames.lock().clone()
    }

    fn types(&self) -> Vec<&'static str> {
        self.frames().iter().map(ServerMessage::tag).collect()
    }

    /// Frames of one type, which is what most assertions actually want.
    fn of(&self, tag: &str) -> Vec<ServerMessage> {
        self.frames()
            .into_iter()
            .filter(|frame| frame.tag() == tag)
            .collect()
    }

    /// Drops what has been asserted, so the next assertion reads a clean slate.
    fn reset(&self) {
        self.frames.lock().clear();
    }

    fn closed(&self) -> Option<u16> {
        *self.closed.lock()
    }

    fn send(&self, frame: serde_json::Value) {
        self.client.receive(Frame::Value(frame));
    }

    /// Waits until the frames satisfy `predicate`.
    ///
    /// A condition rather than a fixed number of yields: a turn crosses several
    /// tasks, and a count that passes on an idle machine fails on a loaded one.
    async fn until(&self, what: &str, predicate: impl Fn(&[ServerMessage]) -> bool) {
        for _ in 0..2_000 {
            if predicate(&self.frames()) {
                return;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("timed out waiting for {what}; saw {:?}", self.types());
    }

    async fn until_seen(&self, tag: &'static str) {
        self.until(tag, |frames| frames.iter().any(|f| f.tag() == tag))
            .await;
    }

    async fn until_count(&self, tag: &'static str, count: usize) {
        self.until(tag, |frames| {
            frames.iter().filter(|f| f.tag() == tag).count() >= count
        })
        .await;
    }
}

struct Harness {
    hub: Arc<SessionHub>,
    runner: Arc<ScriptedRunner>,
    store: Arc<SessionStore>,
    approvals: Arc<HubApprovalGate>,
    ids: Arc<AtomicU64>,
}

#[derive(Default)]
struct HarnessOptions {
    replay_buffer_size: Option<u64>,
    turn_log_max_bytes: Option<u64>,
    /// Agent ids an operator has configured, and whether each is enabled.
    agents: Vec<(&'static str, bool)>,
    max_queue_depth: Option<usize>,
    max_sessions: Option<usize>,
    /// Overrides the loop resolver; `None` uses the scripted runner for every
    /// agent.
    unconfigured: bool,
    /// An agent id whose loop cannot be built.
    unbuildable: Option<&'static str>,
}

fn harness(options: &HarnessOptions) -> Harness {
    let mut config = Config::default();
    if let Some(size) = options.replay_buffer_size {
        config.server.replay_buffer_size = size;
    }
    if let Some(bytes) = options.turn_log_max_bytes {
        config.server.turn_log_max_bytes = bytes;
    }

    let db = Database::in_memory().unwrap();
    let store_ids = AtomicU64::new(0);
    let store = Arc::new(
        SessionStore::new(
            db,
            Arc::new(SystemClock),
            Box::new(move || format!("row-{}", store_ids.fetch_add(1, Ordering::SeqCst) + 1)),
        )
        .unwrap(),
    );
    let runner: Arc<ScriptedRunner> = Arc::new(ScriptedRunner::default());
    let approvals = Arc::new(HubApprovalGate::new(HubApprovalGateOptions::default()));
    let ids = Arc::new(AtomicU64::new(0));

    let agents = options.agents.clone();
    let loop_runner = Arc::clone(&runner);
    let unconfigured = options.unconfigured;
    let unbuildable = options.unbuildable;

    let hub = SessionHub::new(SessionHubOptions {
        config,
        store: Arc::clone(&store),
        approvals: Arc::clone(&approvals),
        loop_for: Arc::new(move |agent_id| {
            if unconfigured {
                return Ok(None);
            }
            if unbuildable.is_some() && agent_id == unbuildable {
                return Err(GhostError::new(ErrorKind::Config, "no such provider"));
            }
            Ok(Some(Arc::clone(&loop_runner) as Arc<dyn TurnRunner>))
        }),
        // The real rule, read off the agents this harness was given, so a test
        // that deletes an agent sees exactly what a deployment would.
        resolve_agent_id: Arc::new(move |agent_id| {
            let id = agent_id.filter(|id| !id.is_empty()).unwrap_or("default");
            if id == "default" {
                return AgentResolution {
                    agent_id: id.to_owned(),
                    miss: None,
                };
            }
            match agents.iter().find(|(name, _)| *name == id) {
                Some((_, true)) => AgentResolution {
                    agent_id: id.to_owned(),
                    miss: None,
                },
                Some((_, false)) => AgentResolution {
                    agent_id: "default".to_owned(),
                    miss: Some(AgentMissReason::Disabled),
                },
                None => AgentResolution {
                    agent_id: "default".to_owned(),
                    miss: Some(AgentMissReason::Unknown),
                },
            }
        }),
        clock: None,
        new_id: Some({
            let ids = Arc::clone(&ids);
            Arc::new(move || format!("id-{}", ids.fetch_add(1, Ordering::SeqCst) + 1))
        }),
        max_queue_depth: options.max_queue_depth,
        max_sessions: options.max_sessions,
    });

    Harness {
        hub,
        runner,
        store,
        approvals,
        ids,
    }
}

impl Harness {
    fn connect(&self, options: ConnectOptions) -> TestClient {
        let (client, mut stream) = self.hub.connect(ConnectOptions {
            session_key: options.session_key.or_else(|| Some(SESSION.to_owned())),
            ..options
        });
        let frames = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&frames);
        let close_sink = Arc::clone(&closed);
        tokio::spawn(async move {
            while let Some(outbound) = stream.next().await {
                match outbound {
                    // The invariant this whole file exists to protect: every
                    // frame the hub emits is a `ServerMessage`. A field the loop
                    // renamed, or a `seq` the hub forgot to stamp, fails here
                    // rather than in a browser.
                    Outbound::Text(text) => {
                        let message: ServerMessage = serde_json::from_str(&text)
                            .unwrap_or_else(|error| panic!("not a ServerMessage: {error}: {text}"));
                        sink.lock().push(message);
                    }
                    Outbound::Close(code) => *close_sink.lock() = Some(code),
                }
            }
        });
        TestClient {
            client,
            frames,
            closed,
        }
    }

    fn plain(&self) -> TestClient {
        self.connect(ConnectOptions::default())
    }
}

fn user(session_key: &str, content: &str) -> serde_json::Value {
    json!({ "type": "user.message", "sessionKey": session_key, "content": content })
}

/// The `seq` of one frame, which every sequenced assertion reaches for.
fn seq(message: &ServerMessage) -> u64 {
    message
        .seq()
        .unwrap_or_else(|| panic!("{message:?} carries no seq"))
}

// Connections

#[tokio::test]
async fn greets_a_new_connection_with_the_protocol_version_and_where_the_session_is() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;

    let ServerMessage::Connected(connected) = &client.frames()[0] else {
        panic!("the first frame is the handshake");
    };
    assert_eq!(connected.session_key, SESSION);
    assert_eq!(connected.last_seq, 0);
    assert_eq!(connected.workspace_id, "default");
    // The version is a type, so a client built against another fails on the
    // handshake rather than three frames later.
    assert_eq!(
        serde_json::to_value(connected).unwrap()["protocolVersion"],
        json!(PROTOCOL_VERSION)
    );
}

#[tokio::test]
async fn mints_a_session_key_when_the_client_does_not_name_one() {
    let h = harness(&HarnessOptions::default());
    let (client, _stream) = h.hub.connect(ConnectOptions::default());
    assert_eq!(client.session_key(), "id-2");
    assert_eq!(h.ids.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn answers_a_ping_on_the_connection_that_sent_it() {
    let h = harness(&HarnessOptions::default());
    let one = h.plain();
    let two = h.plain();
    one.until_seen("connected").await;
    two.until_seen("connected").await;
    one.reset();
    two.reset();

    one.send(json!({ "type": "ping" }));
    one.until_seen("pong").await;
    // Connection-level, so it carries no `seq` and reaches nobody else.
    assert_eq!(one.types(), ["pong"]);
    assert!(two.frames().is_empty());
}

#[tokio::test]
async fn reports_a_malformed_frame_instead_of_failing() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    client.client.receive(Frame::Text("not json".to_owned()));
    client.until_seen("error").await;
    let ServerMessage::Error(error) = &client.frames()[0] else {
        panic!("expected an error frame");
    };
    assert_eq!(error.code, ErrorCode::BadRequest);

    client.reset();
    client.send(json!({ "type": "nonsense" }));
    client.until_seen("error").await;

    client.reset();
    // Parses as JSON and as the right variant, and still names no session.
    client.send(json!({ "type": "user.message", "sessionKey": "", "content": "hi" }));
    client.until_seen("error").await;
}

#[tokio::test]
async fn accepts_a_frame_that_arrives_as_bytes() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    client
        .client
        .receive(Frame::Binary(br#"{"type":"ping"}"#.to_vec()));
    client.until_seen("pong").await;
}

#[tokio::test]
async fn detaches_a_connection_that_fell_too_far_behind_and_keeps_the_others() {
    // 1013 — "try again later" — because the session is fine and the connection
    // is not.
    let h = harness(&HarnessOptions::default());
    let slow = h.connect(ConnectOptions {
        max_buffered_bytes: Some(64),
        ..ConnectOptions::default()
    });
    let healthy = h.plain();
    healthy.until_seen("connected").await;

    h.hub.broadcast(&HubEvent::Agent(AgentEvent::from(
        ghostai_protocol::ws::Notice {
            tag: NoticeTag,
            kind: NoticeKind::Degraded,
            message: "x".repeat(256),
            turn_id: None,
            call_id: None,
        },
    )));

    for _ in 0..500 {
        if slow.closed().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(slow.closed(), Some(1013));
    // The healthy one still gets everything.
    healthy.until_seen("notice").await;
    assert_eq!(h.hub.watchers(SESSION), 1);
}

// Turns with nothing configured

#[tokio::test]
async fn answers_not_configured_and_never_opens_a_turn() {
    // A client that saw a turn close would render an empty assistant message
    // for a request nothing ran.
    let h = harness(&HarnessOptions {
        unconfigured: true,
        ..HarnessOptions::default()
    });
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    client.send(user(SESSION, "hello"));
    client.until_seen("error").await;

    let ServerMessage::Error(error) = &client.of("error")[0] else {
        panic!("expected an error frame");
    };
    assert_eq!(error.code, ErrorCode::NotConfigured);
    assert!(!error.retryable);
    assert!(!client.types().contains(&"turn.start"));
    assert!(!client.types().contains(&"turn.end"));
    assert_eq!(h.runner.count(), 0);
}

#[tokio::test]
async fn still_answers_a_ping_with_nothing_configured() {
    let h = harness(&HarnessOptions {
        unconfigured: true,
        ..HarnessOptions::default()
    });
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();
    client.send(json!({ "type": "ping" }));
    client.until_seen("pong").await;
}

// Turns

#[tokio::test]
async fn acks_a_message_starts_a_turn_and_forwards_its_events_in_sequence() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    client.send(user(SESSION, "hello"));
    client.until_seen("message.ack").await;

    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);
    assert_eq!(turn.input.session_key, SESSION);
    assert_eq!(turn.input.channel.as_deref(), Some("web"));

    turn.start();
    turn.delta("hi ");
    turn.delta("there");
    turn.end();
    client.until_seen("turn.end").await;

    let types = client.types();
    assert_eq!(
        types
            .iter()
            .filter(|tag| **tag != "session.status")
            .copied()
            .collect::<Vec<_>>(),
        [
            "message.ack",
            "turn.start",
            "assistant.delta",
            "assistant.delta",
            "turn.end"
        ]
    );

    // One `seq` stream per session, strictly increasing, starting at 1.
    let seqs: Vec<u64> = client.frames().iter().map(seq).collect();
    assert_eq!(seqs[0], 1);
    assert!(seqs.windows(2).all(|pair| pair[1] == pair[0] + 1));
}

#[tokio::test]
async fn the_hub_only_stamps_and_never_transforms_a_turns_event() {
    // The claim `AgentEvent` + `seq` *is* `ServerMessage`. Forwarding a turn is
    // a counter and a broadcast, not a mapping table, so the frame the hub
    // broadcasts must equal the one the agent crate produces from the same
    // event and the same number.
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    client.send(user(SESSION, "hello"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);

    let representative: Vec<AgentEvent> = vec![
        AgentEvent::from(TurnStart {
            tag: TurnStartTag,
            session_key: SESSION.to_owned(),
            turn_id: turn.turn_id(),
            first_seq: Some(7),
            agent_id: "default".to_owned(),
            model: "m".to_owned(),
            provider: "p".to_owned(),
        }),
        AgentEvent::from(AssistantDelta {
            tag: AssistantDeltaTag,
            turn_id: turn.turn_id(),
            text: "chunk".to_owned(),
        }),
        AgentEvent::from(ToolApprovalRequest {
            tag: ToolApprovalRequestTag,
            turn_id: turn.turn_id(),
            call_id: "call-1".to_owned(),
            name: "exec".to_owned(),
            args: json!({ "argv": ["ls"] }),
            risk: ToolRisk::Exec,
            expires_at_ms: 1_700_000_060_000,
        }),
        AgentEvent::from(ghostai_protocol::ws::Notice {
            tag: NoticeTag,
            kind: NoticeKind::Degraded,
            message: "note".to_owned(),
            turn_id: Some(turn.turn_id()),
            call_id: None,
        }),
    ];
    // From here on, every frame this connection sees is one of the four below:
    // the ack and status the submit produced have already been asserted away.
    client.reset();
    for event in &representative {
        turn.emit(event.clone());
    }
    client.until_count("assistant.delta", 1).await;
    client.until_seen("notice").await;

    let seen = client.frames();
    assert_eq!(seen.len(), representative.len());
    for (event, frame) in representative.iter().zip(seen) {
        let stamped = event.clone().sequenced(seq(&frame));
        assert_eq!(frame, stamped, "the hub added something of its own");
    }
    turn.end();
}

#[tokio::test]
async fn turns_every_attachment_into_a_file_part_whatever_its_type() {
    // A path does the job for a screenshot and a 200 MB archive alike; deciding
    // what a model can be shown of it needs bytes off the disk.
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;

    client.send(json!({
        "type": "user.message",
        "sessionKey": SESSION,
        "content": "look",
        "attachments": [
            { "mimeType": "image/png", "path": "shot.png", "name": "shot", "sizeBytes": 12 },
            { "mimeType": "application/zip", "path": "big.zip" },
        ],
    }));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    let turn = h.runner.turn(0);
    let Content::Parts(parts) = &turn.input.content else {
        panic!("an attachment forces the parts form");
    };
    let parts = serde_json::to_value(parts).unwrap();
    let parts = parts.as_array().unwrap();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[1]["type"], "file");
    assert_eq!(parts[1]["path"], "shot.png");
    assert_eq!(parts[2]["type"], "file");
    assert_eq!(parts[2]["mimeType"], "application/zip");
}

#[tokio::test]
async fn accepts_an_attachment_only_message_and_refuses_one_with_neither() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    client.send(json!({
        "type": "user.message",
        "sessionKey": SESSION,
        "content": "",
        "attachments": [{ "mimeType": "image/png", "path": "shot.png" }],
    }));
    client.until_seen("message.ack").await;

    client.reset();
    client.send(user(SESSION, ""));
    client.until_seen("error").await;
    let ServerMessage::Error(error) = &client.of("error")[0] else {
        panic!("expected an error frame");
    };
    assert_eq!(error.message, "Message is empty");
}

#[tokio::test]
async fn queues_a_second_message_rather_than_running_two_turns_at_once() {
    // Two provider requests interleaving their writes into one history produces
    // a transcript no model can read and no user can explain.
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;

    client.send(user(SESSION, "first"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    h.runner.turn(0).start();
    client.until_seen("turn.start").await;
    client.reset();

    client.send(user(SESSION, "second"));
    client.until_seen("message.queued").await;
    assert_eq!(h.runner.count(), 1, "the second turn has not started");

    let ServerMessage::MessageQueued(queued) = &client.of("message.queued")[0] else {
        panic!("expected a queued frame");
    };
    assert_eq!(queued.event.queue_depth, 1);

    h.runner.turn(0).end();
    for _ in 0..500 {
        if h.runner.count() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(h.runner.count(), 2, "the queue drained when the turn ended");
}

#[tokio::test]
async fn refuses_a_message_past_the_queue_bound_with_session_busy() {
    let h = harness(&HarnessOptions {
        max_queue_depth: Some(2),
        ..HarnessOptions::default()
    });
    let client = h.plain();
    client.until_seen("connected").await;

    client.send(user(SESSION, "first"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    h.runner.turn(0).start();
    client.until_seen("turn.start").await;

    client.send(user(SESSION, "second"));
    client.until_count("message.queued", 1).await;
    client.send(user(SESSION, "third"));
    client.until_count("message.queued", 2).await;
    client.reset();

    client.send(user(SESSION, "fourth"));
    client.until_seen("error").await;
    let ServerMessage::Error(error) = &client.of("error")[0] else {
        panic!("expected an error frame");
    };
    assert_eq!(error.code, ErrorCode::SessionBusy);
    assert!(error.retryable, "the client may try again later");
}

#[tokio::test]
async fn acks_a_retried_client_message_id_without_queueing_it_twice() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    let frame = json!({
        "type": "user.message",
        "sessionKey": SESSION,
        "content": "hello",
        "clientMessageId": "c1",
    });
    client.send(frame.clone());
    client.until_count("message.ack", 1).await;
    client.send(frame);
    client.until_count("message.ack", 2).await;

    let acks = client.of("message.ack");
    let ServerMessage::MessageAck(first) = &acks[0] else {
        panic!("expected an ack");
    };
    let ServerMessage::MessageAck(second) = &acks[1] else {
        panic!("expected an ack");
    };
    // Re-acked with the id the first attempt got, so a reconnecting tab learns
    // its message did land.
    assert_eq!(first.event.message_id, second.event.message_id);
    assert_eq!(second.event.client_message_id.as_deref(), Some("c1"));
    assert_eq!(h.runner.count(), 1, "no second turn was queued");
}

#[tokio::test]
async fn forwards_a_mid_turn_error_without_sequencing_it_and_still_closes_the_turn() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    client.send(user(SESSION, "hello"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);
    turn.start();
    turn.emit(ErrorEvent {
        tag: ErrorTag,
        code: ErrorCode::ProviderError,
        message: "upstream said no".to_owned(),
        retryable: true,
        turn_id: Some(turn.turn_id()),
    });
    turn.end();
    client.until_seen("turn.end").await;

    let ServerMessage::Error(error) = &client.of("error")[0] else {
        panic!("expected an error frame");
    };
    assert_eq!(error.message, "upstream said no");
    // Unsequenced: it is scoped to a turn, not to the session's replayable
    // history, so it never entered the ring.
    assert!(error.turn_id.is_some());
    assert!(client.types().contains(&"turn.end"));
}

#[tokio::test]
async fn aborts_the_running_turn_on_turn_stop_and_ignores_a_stop_with_nothing_running() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;

    // A stop with nothing running is the user clicking as the turn ends.
    client.reset();
    client.send(json!({ "type": "turn.stop", "sessionKey": SESSION }));
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(client.frames().is_empty());

    client.send(user(SESSION, "hello"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);
    turn.start();
    client.until_seen("turn.start").await;

    client.send(json!({ "type": "turn.stop", "sessionKey": SESSION }));
    for _ in 0..500 {
        if turn.token.is_cancelled() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(
        turn.token.is_cancelled(),
        "the turn's own token is what fires"
    );

    // The loop normally answers a stop by ending the turn itself.
    turn.end_with(StopReason::Aborted);
    client.until_seen("turn.end").await;
    assert!(!h.hub.busy(SESSION), "the session is usable again");
}

#[tokio::test]
async fn closes_a_turn_the_loop_failed_out_of_and_says_why() {
    // A turn this hub started is a turn this hub closes: otherwise the client
    // renders a spinner forever.
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    client.send(user(SESSION, "hello"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);
    turn.start();
    turn.fail(GhostError::new(ErrorKind::Provider, "the model refused"));
    client.until_seen("turn.end").await;

    let ServerMessage::Error(error) = &client.of("error")[0] else {
        panic!("expected an error frame");
    };
    // The same mapping the REST handler uses, so one failure cannot be a
    // `provider_error` on a socket and something else on a route.
    assert_eq!(error.code, ErrorCode::ProviderError);
    assert_eq!(error.message, "the model refused");
    let ServerMessage::TurnEnd(end) = &client.of("turn.end")[0] else {
        panic!("expected a turn.end");
    };
    assert_eq!(end.event.stop_reason, StopReason::Error);
}

#[tokio::test]
async fn closes_a_turn_abandoned_by_a_cancellation_without_reporting_an_error() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    client.send(user(SESSION, "hello"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);
    turn.start();
    turn.fail(GhostError::aborted("Turn"));
    client.until_seen("turn.end").await;

    assert!(client.of("error").is_empty(), "a stop is not a failure");
    let ServerMessage::TurnEnd(end) = &client.of("turn.end")[0] else {
        panic!("expected a turn.end");
    };
    assert_eq!(end.event.stop_reason, StopReason::Aborted);
}

#[tokio::test]
async fn reports_a_loop_that_cannot_be_built_and_moves_on_to_the_next_message() {
    let h = harness(&HarnessOptions {
        agents: vec![("broken", true)],
        unbuildable: Some("broken"),
        ..HarnessOptions::default()
    });
    let client = h.connect(ConnectOptions {
        agent_id: Some("broken".to_owned()),
        ..ConnectOptions::default()
    });
    client.until_seen("connected").await;
    client.reset();

    client.send(user(SESSION, "hello"));
    client.until_seen("turn.end").await;

    let ServerMessage::Error(error) = &client.of("error")[0] else {
        panic!("expected an error frame");
    };
    assert_eq!(error.code, ErrorCode::ConfigInvalid);
    assert_eq!(h.runner.count(), 0);
}

#[tokio::test]
async fn stops_every_running_turn_on_close() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;

    client.send(user(SESSION, "hello"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);
    turn.start();
    client.until_seen("turn.start").await;

    h.hub.close();
    assert!(turn.token.is_cancelled());
    assert_eq!(h.hub.session_count(), 0);
}

// Fanout

#[tokio::test]
async fn gives_three_connections_on_one_session_the_same_stream() {
    // `MessageBus` is competing-consumer by design: handing one session's events
    // to it would deliver each event to exactly one of three open tabs.
    let h = harness(&HarnessOptions::default());
    let clients = [h.plain(), h.plain(), h.plain()];
    for client in &clients {
        client.until_seen("connected").await;
        client.reset();
    }

    clients[0].send(user(SESSION, "hello"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);
    turn.start();
    turn.delta("hi");
    turn.end();

    for client in &clients {
        client.until_seen("turn.end").await;
    }
    let first: Vec<u64> = clients[0].frames().iter().map(seq).collect();
    for client in &clients[1..] {
        assert_eq!(client.frames().iter().map(seq).collect::<Vec<_>>(), first);
    }
}

#[tokio::test]
async fn counts_who_is_looking_at_a_session_and_skips_a_connection_with_nobody_on_it() {
    let h = harness(&HarnessOptions::default());
    assert_eq!(h.hub.watchers(SESSION), 0);

    let one = h.plain();
    one.until_seen("connected").await;
    assert_eq!(h.hub.watchers(SESSION), 1);

    let two = h.plain();
    two.until_seen("connected").await;
    assert_eq!(h.hub.watchers(SESSION), 2);

    // The scheduler's own connection is attached for the whole of a run, and
    // counting it made every unattended turn look watched.
    let scheduled = h.connect(ConnectOptions {
        unattended: true,
        ..ConnectOptions::default()
    });
    scheduled.until_seen("connected").await;
    assert_eq!(h.hub.watchers(SESSION), 2);

    one.client.close();
    two.client.close();
    assert_eq!(h.hub.watchers(SESSION), 0);
    assert_eq!(h.hub.watchers("never-seen"), 0);
}

#[tokio::test]
async fn stops_sending_to_a_connection_that_closed() {
    let h = harness(&HarnessOptions::default());
    let staying = h.plain();
    let leaving = h.plain();
    staying.until_seen("connected").await;
    leaving.until_seen("connected").await;
    leaving.client.close();
    leaving.reset();
    staying.reset();

    staying.send(user(SESSION, "hello"));
    staying.until_seen("message.ack").await;
    assert!(leaving.frames().is_empty());
}

#[tokio::test]
async fn stops_delivering_a_session_it_has_switched_away_from() {
    let h = harness(&HarnessOptions::default());
    let mover = h.plain();
    let stayer = h.plain();
    mover.until_seen("connected").await;
    stayer.until_seen("connected").await;

    mover.send(json!({ "type": "session.switch", "sessionKey": "web:2" }));
    mover.until_seen("session.status").await;
    assert_eq!(mover.client.session_key(), "web:2");
    mover.reset();
    stayer.reset();

    stayer.send(user(SESSION, "hello"));
    stayer.until_seen("message.ack").await;
    assert!(mover.frames().is_empty());
}

#[tokio::test]
async fn broadcasts_one_frame_to_every_attached_client_across_sessions() {
    // A nightly job's result is addressed to whoever is looking, not to a
    // conversation.
    let h = harness(&HarnessOptions::default());
    let one = h.plain();
    let two = h.connect(ConnectOptions {
        session_key: Some("web:2".to_owned()),
        ..ConnectOptions::default()
    });
    one.until_seen("connected").await;
    two.until_seen("connected").await;
    one.reset();
    two.reset();

    h.hub.broadcast(&HubEvent::Notification(
        ghostai_protocol::ws::NotificationBody {
            tag: NotificationTag,
            id: "n1".to_owned(),
            title: "done".to_owned(),
            body: String::new(),
            level: ghostai_protocol::ws::NotificationLevel::Info,
            created_at_ms: 0,
            session_key: None,
            job_id: None,
        },
    ));

    one.until_seen("notification").await;
    two.until_seen("notification").await;
    // Each session stamps its own counter rather than inventing a second
    // sequence space the replay ring would not understand.
    assert_eq!(seq(&one.of("notification")[0]), 1);
    assert_eq!(seq(&two.of("notification")[0]), 1);
}

#[tokio::test]
async fn skips_a_session_nobody_is_watching_so_its_seq_stays_honest() {
    // Bumping a counter nobody is reading would leave a session that reconnects
    // later resuming at a `last_seq` accounting for an event it was never sent.
    let h = harness(&HarnessOptions::default());
    let watched = h.plain();
    watched.until_seen("connected").await;

    // A second session with state but no client.
    let idle = h.connect(ConnectOptions {
        session_key: Some("web:2".to_owned()),
        ..ConnectOptions::default()
    });
    idle.until_seen("connected").await;
    idle.client.close();

    h.hub.broadcast(&HubEvent::Notification(
        ghostai_protocol::ws::NotificationBody {
            tag: NotificationTag,
            id: "n1".to_owned(),
            title: "done".to_owned(),
            body: String::new(),
            level: ghostai_protocol::ws::NotificationLevel::Info,
            created_at_ms: 0,
            session_key: None,
            job_id: None,
        },
    ));
    watched.until_seen("notification").await;

    // Reconnecting to the idle session finds the counter where it was left.
    let back = h.connect(ConnectOptions {
        session_key: Some("web:2".to_owned()),
        ..ConnectOptions::default()
    });
    back.until_seen("connected").await;
    let ServerMessage::Connected(connected) = &back.frames()[0] else {
        panic!("expected the handshake");
    };
    assert_eq!(connected.last_seq, 0);
}

// Replay

#[tokio::test]
async fn replays_exactly_what_a_reconnecting_client_missed() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;

    client.send(user(SESSION, "hello"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);
    turn.start();
    turn.delta("one");
    turn.delta("two");
    client.until_count("assistant.delta", 2).await;

    let last_seen = seq(&client.frames()[1]);
    let reconnect = h.plain();
    reconnect.until_seen("connected").await;
    reconnect.reset();
    reconnect.send(json!({
        "type": "session.resume", "sessionKey": SESSION, "lastSeq": last_seen,
    }));
    reconnect.until_seen("session.replay").await;

    let ServerMessage::SessionReplay(replay) = &reconnect.of("session.replay")[0] else {
        panic!("expected a replay");
    };
    assert!(replay.event.complete);
    assert!(replay.event.messages.is_empty());
    // A client told the replay was whole has nothing to rebuild from a second
    // source.
    assert_eq!(replay.event.resuming_turn_id, None);

    reconnect.until_count("assistant.delta", 2).await;
    let replayed: Vec<u64> = reconnect
        .frames()
        .iter()
        .filter(|f| f.tag() != "session.replay" && f.tag() != "session.status")
        .map(seq)
        .collect();
    assert!(replayed.iter().all(|s| *s > last_seen));
    turn.end();
}

#[tokio::test]
async fn rebuilds_from_storage_when_the_resume_falls_outside_the_ring() {
    let h = harness(&HarnessOptions {
        replay_buffer_size: Some(1),
        ..HarnessOptions::default()
    });
    h.store
        .append(
            SESSION,
            ChatMessage::User(user_message("hello")),
            &AppendOptions::default(),
        )
        .unwrap();
    h.store
        .append(
            SESSION,
            ChatMessage::Assistant(assistant_message("hi", AssistantOptions::default())),
            &AppendOptions::default(),
        )
        .unwrap();

    let client = h.plain();
    client.until_seen("connected").await;
    client.send(user(SESSION, "again"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);
    turn.start();
    turn.delta("a");
    turn.delta("b");
    turn.end();
    client.until_seen("turn.end").await;

    let reconnect = h.plain();
    reconnect.until_seen("connected").await;
    reconnect.reset();
    reconnect.send(json!({
        "type": "session.resume", "sessionKey": SESSION, "lastSeq": 1,
    }));
    reconnect.until_seen("session.replay").await;

    let ServerMessage::SessionReplay(replay) = &reconnect.of("session.replay")[0] else {
        panic!("expected a replay");
    };
    assert!(!replay.event.complete);
    assert_eq!(replay.event.messages.len(), 2);
    // Nothing is running, so there is no open turn to name.
    assert_eq!(replay.event.resuming_turn_id, None);
}

#[tokio::test]
async fn replays_the_open_turn_in_full_when_the_resume_falls_outside_the_ring() {
    // The ordinary outcome of reloading during a delegation: a subagent spends a
    // frame per token and the ring is counted in frames.
    let h = harness(&HarnessOptions {
        replay_buffer_size: Some(1),
        ..HarnessOptions::default()
    });
    h.store
        .append(
            SESSION,
            ChatMessage::User(user_message("hello")),
            &AppendOptions::default(),
        )
        .unwrap();

    let client = h.plain();
    client.until_seen("connected").await;
    client.send(user(SESSION, "go"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);
    turn.start();
    turn.delta("one ");
    turn.delta("two");
    client.until_count("assistant.delta", 2).await;

    let reconnect = h.plain();
    reconnect.until_seen("connected").await;
    reconnect.reset();
    reconnect.send(json!({
        "type": "session.resume", "sessionKey": SESSION, "lastSeq": 1,
    }));
    reconnect.until_seen("session.replay").await;

    let ServerMessage::SessionReplay(replay) = &reconnect.of("session.replay")[0] else {
        panic!("expected a replay");
    };
    assert!(!replay.event.complete);
    // `resuming_turn_id` only ever alongside `complete: false`: it tells the
    // client to drop that turn from the tail and rebuild it from the frames.
    assert_eq!(
        replay.event.resuming_turn_id.as_deref(),
        Some(&*turn.turn_id())
    );
    assert!(!replay.event.messages.is_empty());

    reconnect.until_seen("turn.start").await;
    reconnect.until_count("assistant.delta", 1).await;
    // Merged into one entry by the turn log, carrying the later seq.
    let deltas = reconnect.of("assistant.delta");
    assert_eq!(deltas.len(), 1);
    let ServerMessage::AssistantDelta(delta) = &deltas[0] else {
        panic!("expected a delta");
    };
    assert_eq!(delta.event.text, "one two");
    turn.end();
}

#[tokio::test]
async fn names_no_turn_once_the_log_has_overrun_its_budget() {
    // A log that overran holds a *middle*, and replaying a middle over a stored
    // tail would render a turn that began halfway through.
    let h = harness(&HarnessOptions {
        replay_buffer_size: Some(1),
        turn_log_max_bytes: Some(80),
        ..HarnessOptions::default()
    });
    let client = h.plain();
    client.until_seen("connected").await;
    client.send(user(SESSION, "go"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let turn = h.runner.turn(0);
    turn.start();
    turn.delta(&"x".repeat(512));
    client.until_count("assistant.delta", 1).await;

    let reconnect = h.plain();
    reconnect.until_seen("connected").await;
    reconnect.reset();
    reconnect.send(json!({
        "type": "session.resume", "sessionKey": SESSION, "lastSeq": 1,
    }));
    reconnect.until_seen("session.replay").await;

    let ServerMessage::SessionReplay(replay) = &reconnect.of("session.replay")[0] else {
        panic!("expected a replay");
    };
    assert!(!replay.event.complete);
    assert_eq!(replay.event.resuming_turn_id, None);
    assert!(reconnect.of("assistant.delta").is_empty());
    turn.end();
}

#[tokio::test]
async fn tells_a_client_that_has_seen_everything_that_it_missed_nothing() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.send(user(SESSION, "hello"));
    client.until_seen("message.ack").await;
    let last = seq(client.frames().last().unwrap());
    client.reset();

    client.send(json!({
        "type": "session.resume", "sessionKey": SESSION, "lastSeq": last,
    }));
    client.until_seen("session.replay").await;
    let ServerMessage::SessionReplay(replay) = &client.of("session.replay")[0] else {
        panic!("expected a replay");
    };
    assert!(replay.event.complete);
    assert!(replay.event.messages.is_empty());
}

#[tokio::test]
async fn keeps_a_live_session_rather_than_evicting_to_satisfy_the_cap() {
    // The cap yields to anything live rather than dropping work to satisfy a
    // number.
    let h = harness(&HarnessOptions {
        max_sessions: Some(1),
        ..HarnessOptions::default()
    });
    let live = h.plain();
    live.until_seen("connected").await;
    live.send(user(SESSION, "hello"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    h.runner.turn(0).start();
    live.until_seen("turn.start").await;

    // Opening more sessions cannot displace the busy one.
    for index in 0..4 {
        let other = h.connect(ConnectOptions {
            session_key: Some(format!("web:{}", index + 10)),
            ..ConnectOptions::default()
        });
        other.until_seen("connected").await;
        other.client.close();
    }

    assert!(h.hub.busy(SESSION), "the turn survived the eviction passes");
    live.reset();
    h.runner.turn(0).delta("still here");
    live.until_seen("assistant.delta").await;
    h.runner.turn(0).end();
}

#[tokio::test]
async fn evicts_an_idle_session_once_the_cap_is_exceeded() {
    let h = harness(&HarnessOptions {
        max_sessions: Some(2),
        ..HarnessOptions::default()
    });
    for index in 0..4u32 {
        let client = h.connect(ConnectOptions {
            session_key: Some(format!("web:{index}")),
            ..ConnectOptions::default()
        });
        client.until_seen("connected").await;
        client.client.close();
    }
    assert!(
        h.hub.session_count() <= 2,
        "idle sessions were reclaimed: {}",
        h.hub.session_count()
    );
}

// Steering and approvals

#[tokio::test]
async fn steers_the_loop_the_turn_is_running_on_and_echoes_it_to_every_tab() {
    let h = harness(&HarnessOptions::default());
    let one = h.plain();
    let two = h.plain();
    one.until_seen("connected").await;
    two.until_seen("connected").await;

    one.send(user(SESSION, "hello"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    h.runner.turn(0).start();
    one.until_seen("turn.start").await;
    one.reset();
    two.reset();

    one.send(json!({ "type": "turn.steer", "sessionKey": SESSION, "content": "be brief" }));
    one.until_seen("steer").await;
    two.until_seen("steer").await;
    assert_eq!(
        *h.runner.steers.lock(),
        vec![(SESSION.to_owned(), "be brief".to_owned())]
    );
    h.runner.turn(0).end();
}

#[tokio::test]
async fn refuses_a_steer_when_no_turn_is_running() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    client.send(json!({ "type": "turn.steer", "sessionKey": SESSION, "content": "hi" }));
    client.until_seen("error").await;
    let ServerMessage::Error(error) = &client.of("error")[0] else {
        panic!("expected an error frame");
    };
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(h.runner.steers.lock().is_empty());
}

#[tokio::test]
async fn resolves_a_pending_approval_from_an_inbound_tool_approve() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;

    let request = ghostai_agent::ApprovalRequest {
        session_key: SESSION.to_owned(),
        root_session_key: SESSION.to_owned(),
        agent_id: "default".to_owned(),
        turn_id: "turn-1".to_owned(),
        call_id: "call-1".to_owned(),
        name: "exec".to_owned(),
        args: json!({ "argv": ["ls"] }),
        risk: ToolRisk::Exec,
        expires_at_ms: 4_000_000_000_000,
        token: CancellationToken::new(),
    };
    let gate = Arc::clone(&h.approvals);
    let pending = tokio::spawn(async move {
        use ghostai_agent::ApprovalGate as _;
        gate.ask(&request).await.unwrap()
    });
    for _ in 0..500 {
        if h.approvals.pending_count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    client.send(json!({
        "type": "tool.approve", "callId": "call-1", "approved": true, "scope": "session",
    }));
    let decision = pending.await.unwrap();
    assert!(decision.approved);
    assert_eq!(decision.scope, Some(ApprovalScope::Session));
}

#[tokio::test]
async fn says_nothing_when_an_approval_arrives_too_late_to_matter() {
    // The normal two-tab race, or an answer to a call whose turn was stopped.
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();

    client.send(json!({
        "type": "tool.approve", "callId": "gone", "approved": true, "scope": "once",
    }));
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(client.frames().is_empty());
}

// Agent routing

#[tokio::test]
async fn lets_the_stored_session_win_over_a_frame_that_names_another_agent() {
    // A history built under one agent's prompt, tools and permissions must not
    // silently continue under another's.
    let h = harness(&HarnessOptions {
        agents: vec![("writer", true), ("reviewer", true)],
        ..HarnessOptions::default()
    });
    h.store
        .update_session(
            SESSION,
            ghostai_core::session_store::UpdateSession {
                agent_id: Some(Some("writer".to_owned())),
                ..Default::default()
            },
        )
        .unwrap();

    let client = h.plain();
    client.until_seen("connected").await;
    client.send(json!({
        "type": "user.message", "sessionKey": SESSION, "content": "hi", "agentId": "reviewer",
    }));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(h.runner.turn(0).input.agent_id.as_deref(), Some("writer"));
}

#[tokio::test]
async fn runs_on_the_default_agent_when_the_one_a_session_names_is_gone() {
    let h = harness(&HarnessOptions::default());
    h.store
        .update_session(
            SESSION,
            ghostai_core::session_store::UpdateSession {
                agent_id: Some(Some("deleted".to_owned())),
                ..Default::default()
            },
        )
        .unwrap();

    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();
    client.send(user(SESSION, "hi"));
    client.until_seen("notice").await;

    let ServerMessage::Notice(notice) = &client.of("notice")[0] else {
        panic!("expected a notice");
    };
    assert_eq!(notice.event.kind, NoticeKind::AgentFallback);
    assert!(notice.event.message.contains("no longer exists"));
    // Deliberately carries no turn id: it is about the conversation's binding,
    // and a notice addressed to a turn the transcript has no item for is one the
    // client silently drops.
    assert_eq!(notice.event.turn_id, None);
}

#[tokio::test]
async fn says_so_differently_when_the_agent_is_merely_switched_off() {
    let h = harness(&HarnessOptions {
        agents: vec![("paused", false)],
        ..HarnessOptions::default()
    });
    h.store
        .update_session(
            SESSION,
            ghostai_core::session_store::UpdateSession {
                agent_id: Some(Some("paused".to_owned())),
                ..Default::default()
            },
        )
        .unwrap();

    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();
    client.send(user(SESSION, "hi"));
    client.until_seen("notice").await;

    let ServerMessage::Notice(notice) = &client.of("notice")[0] else {
        panic!("expected a notice");
    };
    assert!(notice.event.message.contains("switched off"));
}

#[tokio::test]
async fn does_not_raise_the_notice_when_the_binding_resolves() {
    let h = harness(&HarnessOptions {
        agents: vec![("writer", true)],
        ..HarnessOptions::default()
    });
    h.store
        .update_session(
            SESSION,
            ghostai_core::session_store::UpdateSession {
                agent_id: Some(Some("writer".to_owned())),
                ..Default::default()
            },
        )
        .unwrap();

    let client = h.plain();
    client.until_seen("connected").await;
    client.reset();
    client.send(user(SESSION, "hi"));
    client.until_seen("message.ack").await;
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(client.of("notice").is_empty());
}

#[tokio::test]
async fn takes_the_agent_a_session_new_names_as_the_connection_default() {
    // The web UI resends the agent on every message, which is what hid this; a
    // channel does not.
    let h = harness(&HarnessOptions {
        agents: vec![("writer", true)],
        ..HarnessOptions::default()
    });
    let client = h.plain();
    client.until_seen("connected").await;

    client.send(json!({
        "type": "session.new", "sessionKey": "web:new", "agentId": "writer",
    }));
    client.until_seen("session.status").await;
    client.send(user("web:new", "hi"));
    for _ in 0..500 {
        if h.runner.count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(h.runner.turn(0).input.agent_id.as_deref(), Some("writer"));
}

// Announcing a move and a clear

#[tokio::test]
async fn re_emits_the_status_with_the_workspace_the_store_now_holds() {
    let h = harness(&HarnessOptions::default());
    let client = h.plain();
    client.until_seen("connected").await;
    client.send(user(SESSION, "hi"));
    client.until_seen("message.ack").await;

    h.store
        .update_session(
            SESSION,
            ghostai_core::session_store::UpdateSession {
                workspace_id: Some("research".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
    client.reset();
    h.hub.session_moved(SESSION);
    client.until_seen("session.status").await;

    let ServerMessage::SessionStatus(status) = &client.of("session.status")[0] else {
        panic!("expected a status");
    };
    assert_eq!(status.event.workspace_id, "research");
}

#[tokio::test]
async fn says_nothing_for_a_session_nobody_has_open() {
    let h = harness(&HarnessOptions::default());
    // A PATCH for a conversation nobody has open must not bring hub state into
    // existence for it.
    h.hub.session_moved("never-opened");
    assert_eq!(h.hub.session_count(), 0);
}

#[tokio::test]
async fn announces_a_cleared_conversation_to_every_attached_tab() {
    let h = harness(&HarnessOptions::default());
    let one = h.plain();
    let two = h.plain();
    one.until_seen("connected").await;
    two.until_seen("connected").await;
    one.reset();
    two.reset();

    h.hub.session_cleared(SESSION);
    one.until_seen("session.reset").await;
    two.until_seen("session.reset").await;
    assert_eq!(seq(&one.of("session.reset")[0]), 1);

    // And nothing at all for a session nobody has open.
    h.hub.session_cleared("never-opened");
    assert_eq!(h.hub.session_count(), 1);
}
