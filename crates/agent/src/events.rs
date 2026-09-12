//! What a turn emits.
//!
//! One union, and every member is a [`ServerMessage`] from `ghostai-protocol`
//! minus the fields the transport owns. A WebSocket hub forwards an event by
//! stamping a `seq` on it — there is no mapping table, no per-event
//! translation function, and therefore no place for the two shapes to drift.
//! `tests/events.rs` asserts that literally, against the same frame fixtures
//! the protocol crate is held to.
//!
//! The transport owns exactly two things:
//!
//!  - **`seq`**, because sequencing is per *connection state*, not per turn.
//!    The loop does not know what else the session has emitted, and a counter
//!    it kept would restart on every turn.
//!  - **`session_key`** on the events that carry one. The loop is handed a
//!    session key and puts it on `turn.start`, where a client learns which
//!    conversation the turn belongs to; the per-delta events identify
//!    themselves by `turn_id` alone, which is what keeps a streaming event
//!    small.
//!
//! The bodies are the protocol's own structs rather than local copies, so a
//! field added to `tool.call` is a field this carries with no second edit.
//!
//! One member is not forwardable, which is a different claim from not being a
//! `ServerMessage`. [`ContextUsage`] is one — but it sits outside
//! [`NestedAgentEvent`], so a subagent cannot wrap one and the loop's
//! forwarding drops it. Measuring a child's window would be reporting the
//! wrong conversation: the child runs in a session of its own, and its figure
//! describes a history nobody is reading.

use ghostai_protocol::{
    AssistantDelta, ContextUsage, ErrorEvent, NestedAgentEvent, Notice, ReasoningDelta, Sequenced,
    ServerMessage, SubagentEventBody, ToolApprovalRequest, ToolCallStarted, ToolProgress,
    ToolResult, TurnEnd, TurnStart,
};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use tokio::sync::mpsc;

/// Everything a turn emits, minus the `seq` the transport owns.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// An event a turn produces on its own behalf. The only kind a subagent's
    /// run may be wrapped around.
    Nested(NestedAgentEvent),
    /// One event from a subagent's turn, addressed to the card it belongs
    /// under.
    Subagent(SubagentEventBody),
    /// How much of the window the next request would use. Root loop only.
    ContextUsage(ContextUsage),
}

impl AgentEvent {
    /// The discriminator value this event carries on the wire.
    pub fn tag(&self) -> &'static str {
        match self {
            AgentEvent::Nested(event) => event.tag(),
            AgentEvent::Subagent(_) => "subagent.event",
            AgentEvent::ContextUsage(_) => "context.usage",
        }
    }

    /// The wire frame this event becomes once the transport stamps a `seq`.
    pub fn sequenced(self, seq: u64) -> ServerMessage {
        Stamped::from((self, seq)).into()
    }
}

impl Serialize for AgentEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            AgentEvent::Nested(event) => event.serialize(serializer),
            AgentEvent::Subagent(event) => event.serialize(serializer),
            AgentEvent::ContextUsage(event) => event.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AgentEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<AgentEvent, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let tag = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| D::Error::custom("expected an object with a string `type` field"))?;
        match tag {
            "subagent.event" => SubagentEventBody::deserialize(value)
                .map(AgentEvent::Subagent)
                .map_err(D::Error::custom),
            "context.usage" => ContextUsage::deserialize(value)
                .map(AgentEvent::ContextUsage)
                .map_err(D::Error::custom),
            _ => NestedAgentEvent::deserialize(value)
                .map(AgentEvent::Nested)
                .map_err(D::Error::custom),
        }
    }
}

/// One event with the sequence number a transport stamped on it.
///
/// The conversion into a [`ServerMessage`] is spelled as a `From` over this
/// pair rather than over `(AgentEvent, u64)` directly, because both halves of
/// that tuple would be foreign to this crate and the orphan rule refuses the
/// impl. Nothing else about it differs: it is one step, in one place, and the
/// frame fixtures hold it.
#[derive(Debug, Clone, PartialEq)]
pub struct Stamped {
    /// What the turn emitted.
    pub event: AgentEvent,
    /// Monotonic per session, never reused. See the protocol's sequencing
    /// notes.
    pub seq: u64,
}

impl From<(AgentEvent, u64)> for Stamped {
    fn from((event, seq): (AgentEvent, u64)) -> Stamped {
        Stamped { event, seq }
    }
}

/// The one conversion a transport performs: stamp the sequence number.
///
/// `error` is the exception, and it is the protocol's rather than this
/// crate's: a failure is not part of a session's replayable history, so
/// [`ServerMessage::Error`] carries no `seq` and the number is dropped here.
impl From<Stamped> for ServerMessage {
    fn from(Stamped { event, seq }: Stamped) -> ServerMessage {
        match event {
            AgentEvent::Nested(nested) => match nested {
                NestedAgentEvent::TurnStart(body) => {
                    ServerMessage::TurnStart(Sequenced { seq, event: body })
                }
                NestedAgentEvent::AssistantDelta(body) => {
                    ServerMessage::AssistantDelta(Sequenced { seq, event: body })
                }
                NestedAgentEvent::ReasoningDelta(body) => {
                    ServerMessage::ReasoningDelta(Sequenced { seq, event: body })
                }
                NestedAgentEvent::ToolCall(body) => {
                    ServerMessage::ToolCall(Sequenced { seq, event: body })
                }
                NestedAgentEvent::ToolProgress(body) => {
                    ServerMessage::ToolProgress(Sequenced { seq, event: body })
                }
                NestedAgentEvent::ToolResult(body) => {
                    ServerMessage::ToolResult(Sequenced { seq, event: body })
                }
                NestedAgentEvent::ToolApprovalRequest(body) => {
                    ServerMessage::ToolApprovalRequest(Sequenced { seq, event: body })
                }
                NestedAgentEvent::Notice(body) => {
                    ServerMessage::Notice(Sequenced { seq, event: body })
                }
                NestedAgentEvent::TurnEnd(body) => {
                    ServerMessage::TurnEnd(Sequenced { seq, event: body })
                }
                NestedAgentEvent::Error(body) => ServerMessage::Error(body),
            },
            AgentEvent::Subagent(body) => ServerMessage::Subagent(Sequenced { seq, event: body }),
            AgentEvent::ContextUsage(body) => {
                ServerMessage::ContextUsage(Sequenced { seq, event: body })
            }
        }
    }
}

impl From<NestedAgentEvent> for AgentEvent {
    fn from(event: NestedAgentEvent) -> AgentEvent {
        AgentEvent::Nested(event)
    }
}

impl From<SubagentEventBody> for AgentEvent {
    fn from(event: SubagentEventBody) -> AgentEvent {
        AgentEvent::Subagent(event)
    }
}

impl From<ContextUsage> for AgentEvent {
    fn from(event: ContextUsage) -> AgentEvent {
        AgentEvent::ContextUsage(event)
    }
}

/// `From<Body> for AgentEvent` for each body the protocol already converts into
/// a [`NestedAgentEvent`], so a call site names the event and nothing else.
macro_rules! nested_from {
    ($($ty:ty),+ $(,)?) => {
        $(impl From<$ty> for AgentEvent {
            fn from(event: $ty) -> AgentEvent {
                AgentEvent::Nested(event.into())
            }
        })+
    };
}

nested_from!(
    TurnStart,
    AssistantDelta,
    ReasoningDelta,
    ToolCallStarted,
    ToolProgress,
    ToolResult,
    ToolApprovalRequest,
    Notice,
    TurnEnd,
    ErrorEvent,
);

/// How many events a turn may run ahead of whoever is reading them.
///
/// The backpressure a `for await` gave for free: a turn that streams faster
/// than a socket drains blocks on the send rather than buffering an answer in
/// memory. Large enough that a normal turn never touches it — a whole reply is
/// a few hundred deltas — and small enough that a consumer which has stopped
/// reading stops the turn within one screenful.
pub const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Where a turn's events go.
///
/// Handed down to dispatch and to a subagent's forwarding rather than returned
/// from them, because a Rust turn is a task with a channel where the original
/// was a generator with `yield`: the composition that `yield*` gave is a shared
/// sink, and nothing below the loop then has to thread events back up through
/// its return type.
#[derive(Debug, Clone)]
pub struct EventSink {
    tx: mpsc::Sender<AgentEvent>,
}

impl EventSink {
    /// A sink writing into `tx`.
    pub fn new(tx: mpsc::Sender<AgentEvent>) -> EventSink {
        EventSink { tx }
    }

    /// Emits one event, waiting if the consumer is behind.
    ///
    /// A closed channel is not an error: the consumer has gone, the turn's
    /// token has been cancelled by the guard that went with it, and the loop
    /// unwinds on the next cancellation check rather than on this one.
    pub async fn emit(&self, event: impl Into<AgentEvent>) {
        let _ = self.tx.send(event.into()).await;
    }

    /// Whether the consumer has gone.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}
