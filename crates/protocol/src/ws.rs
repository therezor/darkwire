//! The WebSocket protocol.
//!
//! Both directions are unions discriminated on `type`, so a handler is an
//! exhaustive `match` and adding a message without handling it is a compile
//! error. Event names are spelled out rather than abbreviated: a wire format
//! that re-derives shapes from single-letter keys saves bytes the transport's
//! compression would have saved anyway, and costs the discriminator.
//!
//! ## Sequencing
//!
//! Every server event that belongs to a turn carries a monotonic `seq`, unique
//! per session and never reused. A reconnecting tab sends
//! `session.resume { lastSeq }` and the server replays everything after it
//! from the ring buffer, so a page refresh mid-stream rebuilds the in-flight
//! turn instead of losing it. Session-scoped rather than turn-scoped because
//! the client needs one cursor, not one per turn. Connection-level events
//! (`connected`, `pong`, `error`) carry no `seq` — they are not part of any
//! session's replayable history — and [`ServerMessage::seq`] is how a replay
//! buffer tells the two apart.
//!
//! ## Who accumulates
//!
//! The server emits deltas and the client accumulates them. The server never
//! holds a running copy of the response text, because a server-side buffer
//! that has to be reset at the right moment makes the server stateful about
//! what each client has rendered. The only server-side state is the replay
//! buffer, which is append-only.

use std::borrow::Cow;

use garde::Validate;
use indexmap::IndexMap;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json::{MAX_SAFE_INTEGER, literal, positive, tagged_union};
use crate::messages::{StopReason, StoredMessage, Usage};
use crate::tools::{ApprovalScope, ToolDefinition, ToolRisk};

/// Version of the wire protocol. Bumped on any breaking envelope change.
pub const PROTOCOL_VERSION: u64 = 2;

/// The protocol version as a type: serialises to [`PROTOCOL_VERSION`] and
/// refuses any other number, so a client built against another version fails
/// on the handshake rather than three frames later.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ProtocolVersion;

impl Serialize for ProtocolVersion {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(PROTOCOL_VERSION)
    }
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let version = u64::deserialize(d)?;
        if version == PROTOCOL_VERSION {
            Ok(Self)
        } else {
            Err(de::Error::invalid_value(
                de::Unexpected::Unsigned(version),
                &"protocol version 2",
            ))
        }
    }
}

impl JsonSchema for ProtocolVersion {
    fn schema_name() -> Cow<'static, str> {
        "ProtocolVersion".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "number", "const": PROTOCOL_VERSION })
    }
}

// Client → server

/// An upload attached to a message: a file in the workspace, named by its path.
///
/// The path, and only the path. A signed URL means nothing outside this origin
/// and expires ten minutes later even here; a path is stable, resolvable by the
/// file tools, and signable on demand when a browser needs to draw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct Attachment {
    /// The file's media type.
    #[garde(length(utf16, min = 1))]
    pub mime_type: String,
    /// Workspace-relative, as returned by `POST /api/files/upload`.
    #[garde(length(utf16, min = 1))]
    pub path: String,
    /// What the user called it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Size on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub size_bytes: Option<u64>,
}

/// The most files one message may carry.
///
/// A bound on the *count*, which the upload route's byte limit is not: every
/// attachment is read and inlined into the provider request on every iteration
/// of the turn, so a frame naming one small image a few thousand times costs
/// nothing to send and expands to thousands of base64 blocks in one body.
pub const MAX_ATTACHMENTS: usize = 20;

literal! {
    /// The `type` of a [`PingMessage`].
    pub struct PingTag = "ping";
}
literal! {
    /// The `type` of a [`UserMessageRequest`].
    pub struct UserMessageTag = "user.message";
}
literal! {
    /// The `type` of a [`RegenerateMessage`].
    pub struct RegenerateTag = "turn.regenerate";
}
literal! {
    /// The `type` of an [`EditMessage`].
    pub struct EditTag = "user.edit";
}
literal! {
    /// The `type` of a [`StopTurnMessage`].
    pub struct StopTurnTag = "turn.stop";
}
literal! {
    /// The `type` of a [`NewSessionMessage`].
    pub struct NewSessionTag = "session.new";
}
literal! {
    /// The `type` of a [`SwitchSessionMessage`].
    pub struct SwitchSessionTag = "session.switch";
}
literal! {
    /// The `type` of a [`ResumeSessionMessage`].
    pub struct ResumeSessionTag = "session.resume";
}
literal! {
    /// The `type` of a [`ToolApproveMessage`].
    pub struct ToolApproveTag = "tool.approve";
}
literal! {
    /// The `type` of a [`SteerMessage`].
    pub struct SteerTag = "turn.steer";
}

/// Keep-alive.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate,
)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct PingMessage {
    /// Always `ping`.
    #[serde(rename = "type")]
    pub tag: PingTag,
}

/// A person's message, starting a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct UserMessageRequest {
    /// Always `user.message`.
    #[serde(rename = "type")]
    pub tag: UserMessageTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// The text.
    pub content: String,
    /// Attached workspace files. The browser omits the key when there are none.
    #[serde(default)]
    #[garde(length(max = MAX_ATTACHMENTS), dive)]
    pub attachments: Vec<Attachment>,
    /// The agent to run as, for a session that does not have one yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Client-generated idempotency key. Lets a retry after a dropped socket
    /// avoid appending the same user turn twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
}

/// Stop the in-flight turn. One cancellation reaches the provider request,
/// the running tool and the child process, so there is no path where the loop
/// stops but the `exec` child keeps running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct StopTurnMessage {
    /// Always `turn.stop`.
    #[serde(rename = "type")]
    pub tag: StopTurnTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
}

/// Start a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct NewSessionMessage {
    /// Always `session.new`.
    #[serde(rename = "type")]
    pub tag: NewSessionTag,
    /// The server generates one when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// Which workspace to create the conversation in. Defaults to `default`.
    /// Only ever *creates*: a session that already exists keeps the workspace
    /// it was born in, whatever this says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: Option<String>,
    /// The agent to bind it to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// Move this connection to an existing conversation.
///
/// Deliberately carries no workspace: switching to a session moves you to
/// *its* workspace, and the hub reports which one on the `session.status` that
/// follows. Letting a client name one here would let the UI's idea of the
/// current workspace and the session's own disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SwitchSessionMessage {
    /// Always `session.switch`.
    #[serde(rename = "type")]
    pub tag: SwitchSessionTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
}

/// Reconnect handshake: replay everything after `last_seq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ResumeSessionMessage {
    /// Always `session.resume`.
    #[serde(rename = "type")]
    pub tag: ResumeSessionTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// The last `seq` the client saw.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub last_seq: u64,
}

/// Answer to a `tool.approvalRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolApproveMessage {
    /// Always `tool.approve`.
    #[serde(rename = "type")]
    pub tag: ToolApproveTag,
    /// The call being decided.
    #[garde(length(utf16, min = 1))]
    pub call_id: String,
    /// The decision.
    pub approved: bool,
    /// How long it holds. Anything broader than `once` has to be chosen
    /// deliberately — a client that omits the field must not silently grant
    /// blanket approval.
    #[serde(default)]
    pub scope: ApprovalScope,
}

/// Mid-turn steering. The loop drains the steer queue and *continues* rather
/// than breaking, so guidance that arrives while the model is composing its
/// final answer still lands on the next iteration instead of being dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SteerMessage {
    /// Always `turn.steer`.
    #[serde(rename = "type")]
    pub tag: SteerTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// The guidance.
    #[garde(length(utf16, min = 1))]
    pub content: String,
}

/// Re-run a turn, discarding the answer it produced.
///
/// Over the socket rather than REST because it *starts a turn*, and every turn
/// goes through the hub so that the one-at-a-time rule, the FIFO queue, the
/// approval gate and the event stream all apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct RegenerateMessage {
    /// Always `turn.regenerate`.
    #[serde(rename = "type")]
    pub tag: RegenerateTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// The user message to re-run from. Absent means the most recent turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub seq: Option<u64>,
    /// Re-running deletes the question and the loop writes it back, so the
    /// client puts the bubble up itself meanwhile; this is the id the
    /// `message.ack` echoes so that bubble has something to reconcile against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
}

/// Replace a message and re-run from it.
///
/// One frame rather than a truncate call followed by `user.message`: the two
/// halves are a single user intent, and splitting them leaves a window in which
/// another tab's queued message lands in the gap between them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct EditMessage {
    /// Always `user.edit`.
    #[serde(rename = "type")]
    pub tag: EditTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// Must address a `user` message; the hub refuses anything else.
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub seq: u64,
    /// The replacement text.
    pub content: String,
    /// Attached workspace files.
    #[serde(default)]
    #[garde(length(max = MAX_ATTACHMENTS), dive)]
    pub attachments: Vec<Attachment>,
    /// The agent to run as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// See [`RegenerateMessage::client_message_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
}

tagged_union! {
    /// Anything a client may send.
    pub enum ClientMessage by "type" {
        /// Keep-alive.
        Ping(PingMessage) = "ping",
        /// A person's message.
        UserMessage(UserMessageRequest) = "user.message",
        /// Re-run a turn.
        Regenerate(RegenerateMessage) = "turn.regenerate",
        /// Replace a message and re-run.
        Edit(EditMessage) = "user.edit",
        /// Stop the turn.
        StopTurn(StopTurnMessage) = "turn.stop",
        /// Start a conversation.
        NewSession(NewSessionMessage) = "session.new",
        /// Move to a conversation.
        SwitchSession(SwitchSessionMessage) = "session.switch",
        /// Reconnect.
        ResumeSession(ResumeSessionMessage) = "session.resume",
        /// Decide a tool call.
        ToolApprove(ToolApproveMessage) = "tool.approve",
        /// Steer the turn.
        Steer(SteerMessage) = "turn.steer",
    }
}

// Server → client

/// A session-scoped event with the `seq` the transport owns.
///
/// The body is a turn's event as the loop emits it; the sequence number is
/// stamped on the way out. Flattened, so the wire shape is one object with
/// `type`, `seq` and the body's fields side by side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct Sequenced<T: JsonSchema + Validate<Context = ()>> {
    /// Monotonic per session, never reused.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub seq: u64,
    /// The event.
    #[serde(flatten)]
    #[garde(dive)]
    pub event: T,
}

literal! {
    /// The `type` of a [`ConnectedEvent`].
    pub struct ConnectedTag = "connected";
}
literal! {
    /// The `type` of a [`PongEvent`].
    pub struct PongTag = "pong";
}
literal! {
    /// The `type` of an [`ErrorEvent`].
    pub struct ErrorTag = "error";
}
literal! {
    /// The `type` of a [`MessageAck`].
    pub struct MessageAckTag = "message.ack";
}
literal! {
    /// The `type` of a [`MessageQueued`].
    pub struct MessageQueuedTag = "message.queued";
}
literal! {
    /// The `type` of a [`TurnStart`].
    pub struct TurnStartTag = "turn.start";
}
literal! {
    /// The `type` of an [`AssistantDelta`].
    pub struct AssistantDeltaTag = "assistant.delta";
}
literal! {
    /// The `type` of a [`ReasoningDelta`].
    pub struct ReasoningDeltaTag = "reasoning.delta";
}
literal! {
    /// The `type` of a [`ToolCallStarted`].
    pub struct ToolCallTag = "tool.call";
}
literal! {
    /// The `type` of a [`ToolProgress`].
    pub struct ToolProgressTag = "tool.progress";
}
literal! {
    /// The `type` of a [`ToolResult`].
    pub struct ToolResultTag = "tool.result";
}
literal! {
    /// The `type` of a [`ToolApprovalRequest`].
    pub struct ToolApprovalRequestTag = "tool.approvalRequest";
}
literal! {
    /// The `type` of a [`Notice`].
    pub struct NoticeTag = "notice";
}
literal! {
    /// The `type` of a [`TurnEnd`].
    pub struct TurnEndTag = "turn.end";
}
literal! {
    /// The `type` of a [`SubagentEventBody`].
    pub struct SubagentEventTag = "subagent.event";
}
literal! {
    /// The `type` of a [`ContextUsage`].
    pub struct ContextUsageTag = "context.usage";
}
literal! {
    /// The `type` of a [`SessionStatus`].
    pub struct SessionStatusTag = "session.status";
}
literal! {
    /// The `type` of a [`SessionReset`].
    pub struct SessionResetTag = "session.reset";
}
literal! {
    /// The `type` of a [`SessionReplay`].
    pub struct SessionReplayTag = "session.replay";
}
literal! {
    /// The `type` of a [`SessionTruncated`].
    pub struct SessionTruncatedTag = "session.truncated";
}
literal! {
    /// The `type` of a [`NotificationBody`].
    pub struct NotificationTag = "notification";
}
literal! {
    /// The `type` of a [`ToolsChanged`].
    pub struct ToolsChangedTag = "tools.changed";
}
literal! {
    /// The `type` of a [`Steer`].
    pub struct SteerEventTag = "steer";
}

/// The handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ConnectedEvent {
    /// Always `connected`.
    #[serde(rename = "type")]
    pub tag: ConnectedTag,
    /// Always 2.
    pub protocol_version: ProtocolVersion,
    /// The conversation this connection landed in.
    pub session_key: String,
    /// The server's clock.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub server_time_ms: u64,
    /// Last `seq` the server has emitted, so a fresh client knows where it is.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub last_seq: u64,
    /// The workspace this connection landed in, so a reconnecting tab learns
    /// which one it is looking at without a REST round trip.
    #[serde(default = "default_workspace")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: String,
}

fn default_workspace() -> String {
    crate::ids::DEFAULT_WORKSPACE_ID.to_owned()
}

/// Answer to a `ping`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct PongEvent {
    /// Always `pong`.
    #[serde(rename = "type")]
    pub tag: PongTag,
    /// The server's clock.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub server_time_ms: u64,
}

/// Typed error codes, never substring-sniffed. Deriving a code by searching
/// response *content* for "429" or "overloaded" means a model that legitimately
/// writes about rate limiting triggers a retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No valid credential.
    Unauthorized,
    /// The frame did not parse.
    BadRequest,
    /// No such thing.
    NotFound,
    /// Slow down.
    RateLimited,
    /// The model endpoint failed.
    ProviderError,
    /// A tool failed.
    ToolError,
    /// The settings do not parse.
    ConfigInvalid,
    /// No provider and model are configured, so no turn can run.
    ///
    /// Distinct from `config_invalid`: nothing is wrong with the settings,
    /// they are merely incomplete, and the client's response is to offer setup
    /// rather than to report a fault. Every other route works in this state.
    NotConfigured,
    /// The session is mid-turn.
    SessionBusy,
    /// Something else.
    Internal,
}

/// A failure, scoped to a turn or to the connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ErrorEvent {
    /// Always `error`.
    #[serde(rename = "type")]
    pub tag: ErrorTag,
    /// What went wrong, as a code.
    pub code: ErrorCode,
    /// What went wrong, for a person.
    pub message: String,
    /// Whether trying again might work.
    #[serde(default)]
    pub retryable: bool,
    /// Present when the error is scoped to a turn rather than the connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

/// The user message was stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct MessageAck {
    /// Always `message.ack`.
    #[serde(rename = "type")]
    pub tag: MessageAckTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// The stored message's id.
    #[garde(length(utf16, min = 1))]
    pub message_id: String,
    /// The client's idempotency key, echoed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
}

/// The session was mid-turn; the message runs when the current one finishes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct MessageQueued {
    /// Always `message.queued`.
    #[serde(rename = "type")]
    pub tag: MessageQueuedTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// How many are waiting.
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub queue_depth: u64,
}

/// A turn began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct TurnStart {
    /// Always `turn.start`.
    #[serde(rename = "type")]
    pub tag: TurnStartTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// The turn.
    #[garde(length(utf16, min = 1))]
    pub turn_id: String,
    /// The `seq` of the user message that started this turn. Reported *here*
    /// as well as on `turn.end` because a turn that fails never reaches its
    /// end, and without it a failed turn had no storage address to re-run
    /// from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub first_seq: Option<u64>,
    /// Which agent is running the turn. Reported per turn rather than looked
    /// up from the session, because a session can be moved to another agent
    /// and a transcript that relabelled its history would be describing turns
    /// that never happened.
    pub agent_id: String,
    /// The model in use.
    pub model: String,
    /// The provider instance in use.
    pub provider: String,
}

/// A chunk of the assistant's answer. Clients append; the server never resends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AssistantDelta {
    /// Always `assistant.delta`.
    #[serde(rename = "type")]
    pub tag: AssistantDeltaTag,
    /// The turn.
    #[garde(length(utf16, min = 1))]
    pub turn_id: String,
    /// The chunk.
    pub text: String,
}

/// Reasoning stream, rendered in its own collapsible block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ReasoningDelta {
    /// Always `reasoning.delta`.
    #[serde(rename = "type")]
    pub tag: ReasoningDeltaTag,
    /// The turn.
    #[garde(length(utf16, min = 1))]
    pub turn_id: String,
    /// The chunk.
    pub text: String,
}

/// The model called a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolCallStarted {
    /// Always `tool.call`.
    #[serde(rename = "type")]
    pub tag: ToolCallTag,
    /// The turn.
    #[garde(length(utf16, min = 1))]
    pub turn_id: String,
    /// The call.
    #[garde(length(utf16, min = 1))]
    pub call_id: String,
    /// The tool.
    #[garde(length(utf16, min = 1))]
    pub name: String,
    /// Parsed args when valid JSON; the raw string otherwise.
    pub args: Value,
    /// The tool's declared risk, so a card can badge itself.
    pub risk: ToolRisk,
}

/// Liveness for a long-running tool, emitted every 15 s while it runs so the
/// UI can show that a slow `exec` has not hung.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolProgress {
    /// Always `tool.progress`.
    #[serde(rename = "type")]
    pub tag: ToolProgressTag,
    /// The turn.
    #[garde(length(utf16, min = 1))]
    pub turn_id: String,
    /// The call.
    #[garde(length(utf16, min = 1))]
    pub call_id: String,
    /// How long it has been running.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub elapsed_ms: u64,
    /// What the tool says it is doing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A tool answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolResult {
    /// Always `tool.result`.
    #[serde(rename = "type")]
    pub tag: ToolResultTag,
    /// The turn.
    #[garde(length(utf16, min = 1))]
    pub turn_id: String,
    /// The call.
    #[garde(length(utf16, min = 1))]
    pub call_id: String,
    /// Whether it succeeded.
    pub ok: bool,
    /// The result text.
    pub content: String,
    /// Whether the result was cut to fit the output cap.
    #[serde(default)]
    pub truncated: bool,
    /// How long it took.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub duration_ms: u64,
}

/// Blocks the call until a matching `tool.approve` arrives or the policy times
/// out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolApprovalRequest {
    /// Always `tool.approvalRequest`.
    #[serde(rename = "type")]
    pub tag: ToolApprovalRequestTag,
    /// The turn.
    #[garde(length(utf16, min = 1))]
    pub turn_id: String,
    /// The call.
    #[garde(length(utf16, min = 1))]
    pub call_id: String,
    /// The tool.
    #[garde(length(utf16, min = 1))]
    pub name: String,
    /// Parsed args when valid JSON; the raw string otherwise.
    pub args: Value,
    /// The tool's declared risk.
    pub risk: ToolRisk,
    /// When the prompt closes as denied.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub expires_at_ms: u64,
}

/// Advisory notices.
///
/// `prompt_injection` is the important one: detection is **non-destructive**.
/// Replacing a matched tool result with a warning banner means reading this
/// project's own security documentation silently wipes the output and leaves
/// the model hallucinating around the hole. The content passes through intact,
/// the nonce delimiters do the actual defending, and this raises a badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// A tool result looked like an instruction.
    PromptInjection,
    /// A request was retried with a feature stripped.
    Degraded,
    /// History was trimmed to fit the window.
    TruncatedHistory,
    /// Another provider answered.
    ProviderFallback,
    /// An operator denied a call.
    ApprovalDenied,
    /// The session is bound to an agent that no longer resolves, so the turn
    /// ran on `default` instead.
    ///
    /// A notice rather than an error because the turn *happened*: an agent id
    /// is user-authored and can be deleted at any moment, and refusing every
    /// conversation that named one would make a config edit break work that
    /// has nothing to do with it. The binding is left alone, so re-creating
    /// the agent restores every conversation waiting for it — which is why
    /// this repeats each turn. `default` may allow tools the departed agent
    /// did not, so this widens what the turn could do rather than narrowing it.
    AgentFallback,
    /// The model called a tool on an agent whose tools are switched off.
    ///
    /// Separate from `approval_denied` because nothing was denied: the agent's
    /// permission map still says `allow`. The model invented a call it was
    /// never offered — the request carried no tools at all — so the notice is
    /// about a capability, not about a decision anyone made.
    ToolsDisabled,
}

/// An advisory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct Notice {
    /// Always `notice`.
    #[serde(rename = "type")]
    pub tag: NoticeTag,
    /// What kind.
    pub kind: NoticeKind,
    /// For a person.
    pub message: String,
    /// The turn, when it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// The call, when it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
}

/// A turn finished.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct TurnEnd {
    /// Always `turn.end`.
    #[serde(rename = "type")]
    pub tag: TurnEndTag,
    /// The turn.
    #[garde(length(utf16, min = 1))]
    pub turn_id: String,
    /// Why it stopped.
    pub stop_reason: StopReason,
    /// What it cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(dive)]
    pub usage: Option<Usage>,
    /// How many tool round-trips it took.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub iterations: u64,
    /// Wall time from the first append to this event. Deliberately *not* the
    /// divisor for tokens/s: it spans the model load, prompt eval, every tool
    /// call and every approval wait. Still what a person means by "how long
    /// did that take", and the rate falls back to it when there is no window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub elapsed_ms: Option<u64>,
    /// Time the model spent emitting tokens, summed over the requests this
    /// turn made. Absent when nothing could be measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub generation_ms: Option<u64>,
    /// The completion tokens produced inside `generation_ms`. Deliberately not
    /// the turn's `usage.completion_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub generation_tokens: Option<u64>,
    /// Turn start to the first token anyone saw: the preamble, queueing,
    /// weight loading and prompt eval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub first_token_ms: Option<u64>,
    /// The `seq` of the user message that started this turn. Reporting it here
    /// is what lets a message become editable the instant its turn ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub first_seq: Option<u64>,
    /// The `seq` of the last message the turn appended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub last_seq: Option<u64>,
}

tagged_union! {
    /// Everything a turn emits, minus the `seq` the transport owns.
    ///
    /// The same bodies the sequenced events wrap, so a field added to
    /// `tool.call` is a field a nested `tool.call` carries with no second edit.
    /// `subagent.event` itself is **not** a member: nesting deeper than one
    /// level works by forwarding, not by wrapping a wrapper.
    pub enum NestedAgentEvent by "type" {
        /// A turn began.
        TurnStart(TurnStart) = "turn.start",
        /// Answer text.
        AssistantDelta(AssistantDelta) = "assistant.delta",
        /// Reasoning text.
        ReasoningDelta(ReasoningDelta) = "reasoning.delta",
        /// A tool was called.
        ToolCall(ToolCallStarted) = "tool.call",
        /// A tool is still running.
        ToolProgress(ToolProgress) = "tool.progress",
        /// A tool answered.
        ToolResult(ToolResult) = "tool.result",
        /// A tool needs approval.
        ToolApprovalRequest(ToolApprovalRequest) = "tool.approvalRequest",
        /// An advisory.
        Notice(Notice) = "notice",
        /// A turn finished.
        TurnEnd(TurnEnd) = "turn.end",
        /// A failure. Already unsequenced.
        Error(ErrorEvent) = "error",
    }
}

/// One event from a subagent's own turn, addressed to the card it belongs
/// under.
///
/// A wrapper rather than an optional `parentCallId` on every event, because a
/// new member of the union is one case in an exhaustive `match`, so every
/// consumer is *made* to decide — where an optional field would let the
/// channel projection quietly fold a subagent's deltas into the reply it sends
/// to a chat app. `turn_id` is always the root turn, rewritten on the way up at
/// every level; `parent_session_key` + `parent_call_id` are where it nests,
/// unique at any depth because a call id is only unique within one assistant
/// message; `session_key` is the subagent's own session, a real row a client
/// fetches to show this run again after a reload. Depth beyond one level works
/// by **forwarding**, which is why the payload is not recursive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SubagentEventBody {
    /// Always `subagent.event`.
    #[serde(rename = "type")]
    pub tag: SubagentEventTag,
    /// The root turn.
    #[garde(length(utf16, min = 1))]
    pub turn_id: String,
    /// The session that delegated.
    #[garde(length(utf16, min = 1))]
    pub parent_session_key: String,
    /// The delegating call.
    #[garde(length(utf16, min = 1))]
    pub parent_call_id: String,
    /// The subagent that produced the inner event.
    #[garde(length(utf16, min = 1))]
    pub agent_id: String,
    /// Its label, so a card can name it without resolving the id.
    #[serde(default)]
    pub label: String,
    /// The subagent's own session.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// 1 for a subagent of the session's own agent.
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub depth: u64,
    /// The inner event.
    #[garde(dive)]
    pub event: NestedAgentEvent,
}

/// How much of the context window the next request would use, restated
/// whenever the history grows.
///
/// Session-scoped rather than turn-scoped: it describes the conversation, not
/// the turn that happened to grow it, and carries no `turn_id` for that reason.
/// The numbers are the same ones the context route returns — a bar and a panel
/// that disagree about the same conversation are worse than a bar that is late.
/// Emitted by the root loop only, which is why it is absent from
/// [`NestedAgentEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ContextUsage {
    /// Always `context.usage`.
    #[serde(rename = "type")]
    pub tag: ContextUsageTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// What the next request would cost.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub estimated_tokens: u64,
    /// The window it has to fit.
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub context_window_tokens: u64,
    /// Section name to tokens. A map rather than a fixed object, so a new
    /// section is not a wire change.
    #[serde(default)]
    pub breakdown: IndexMap<String, f64>,
}

/// Where a session stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SessionStatus {
    /// Always `session.status`.
    #[serde(rename = "type")]
    pub tag: SessionStatusTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// Whether a turn is running.
    pub busy: bool,
    /// How many messages are waiting.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub queue_depth: u64,
    /// The session's workspace, restated on every switch.
    #[serde(default = "default_workspace")]
    #[garde(length(utf16, min = 1))]
    pub workspace_id: String,
    /// The running turn, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

/// The conversation was cleared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SessionReset {
    /// Always `session.reset`.
    #[serde(rename = "type")]
    pub tag: SessionResetTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
}

/// Replayed history after `session.resume`, so a reconnecting client can
/// restore completed messages before the live deltas resume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SessionReplay {
    /// Always `session.replay`.
    #[serde(rename = "type")]
    pub tag: SessionReplayTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// The stored tail.
    #[garde(dive)]
    pub messages: Vec<StoredMessage>,
    /// False when `last_seq` fell outside the ring buffer and history was
    /// trimmed.
    #[serde(default = "crate::json::yes")]
    pub complete: bool,
    /// A turn whose frames follow this one, in full, and which they define.
    ///
    /// Only ever set alongside `complete: false`, the one case where a client
    /// is handed storage *and* frames — legal because storage cannot describe
    /// a turn that has not ended. The named turn is the exception: its
    /// finished iterations *are* in the tail, and the frames repeat them, so a
    /// client drops whatever it holds for this turn and rebuilds it from the
    /// frames. Absent means the tail is history and the frames continue from
    /// its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(length(utf16, min = 1))]
    pub resuming_turn_id: Option<String>,
}

/// A suffix of the conversation was dropped — by a regenerate, an edit, or a
/// truncation from another client.
///
/// Carries the stored tail rather than only the cut point, so a client rebuilds
/// from one frame instead of racing a refetch against the turn that is about to
/// start. Sequenced, so it reaches every attached tab and enters the replay
/// ring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SessionTruncated {
    /// Always `session.truncated`.
    #[serde(rename = "type")]
    pub tag: SessionTruncatedTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// Everything after this `seq` is gone.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub up_to_seq: u64,
    /// The surviving tail.
    #[garde(dive)]
    pub messages: Vec<StoredMessage>,
}

/// How loud a notification is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    /// Informational.
    #[default]
    Info,
    /// Something finished well.
    Success,
    /// Something needs a look.
    Warning,
    /// Something failed.
    Error,
}

/// A notification was raised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct NotificationBody {
    /// Always `notification`.
    #[serde(rename = "type")]
    pub tag: NotificationTag,
    /// The row id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// The headline.
    pub title: String,
    /// The detail.
    pub body: String,
    /// How loud.
    #[serde(default)]
    pub level: NotificationLevel,
    /// When it was raised.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub created_at_ms: u64,
    /// The conversation it is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// Set when raised by an automation run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
}

/// Tool list changed — an MCP server reconnected, or an extension loaded or
/// unloaded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolsChanged {
    /// Always `tools.changed`.
    #[serde(rename = "type")]
    pub tag: ToolsChangedTag,
    /// The new list.
    #[garde(dive)]
    pub tools: Vec<ToolDefinition>,
}

/// Mid-turn steering echoed back so every tab shows what was injected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct Steer {
    /// Always `steer`.
    #[serde(rename = "type")]
    pub tag: SteerEventTag,
    /// The conversation.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// The guidance.
    pub content: String,
}

/// `message.ack`, sequenced.
pub type MessageAckEvent = Sequenced<MessageAck>;
/// `message.queued`, sequenced.
pub type MessageQueuedEvent = Sequenced<MessageQueued>;
/// `turn.start`, sequenced.
pub type TurnStartEvent = Sequenced<TurnStart>;
/// `assistant.delta`, sequenced.
pub type AssistantDeltaEvent = Sequenced<AssistantDelta>;
/// `reasoning.delta`, sequenced.
pub type ReasoningDeltaEvent = Sequenced<ReasoningDelta>;
/// `tool.call`, sequenced.
pub type ToolCallEvent = Sequenced<ToolCallStarted>;
/// `tool.progress`, sequenced.
pub type ToolProgressEvent = Sequenced<ToolProgress>;
/// `tool.result`, sequenced.
pub type ToolResultEvent = Sequenced<ToolResult>;
/// `tool.approvalRequest`, sequenced.
pub type ToolApprovalRequestEvent = Sequenced<ToolApprovalRequest>;
/// `notice`, sequenced.
pub type NoticeEvent = Sequenced<Notice>;
/// `turn.end`, sequenced.
pub type TurnEndEvent = Sequenced<TurnEnd>;
/// `subagent.event`, sequenced.
pub type SubagentEvent = Sequenced<SubagentEventBody>;
/// `context.usage`, sequenced.
pub type ContextUsageEvent = Sequenced<ContextUsage>;
/// `session.status`, sequenced.
pub type SessionStatusEvent = Sequenced<SessionStatus>;
/// `session.reset`, sequenced.
pub type SessionResetEvent = Sequenced<SessionReset>;
/// `session.replay`, sequenced.
pub type SessionReplayEvent = Sequenced<SessionReplay>;
/// `session.truncated`, sequenced.
pub type SessionTruncatedEvent = Sequenced<SessionTruncated>;
/// `notification`, sequenced.
pub type NotificationEvent = Sequenced<NotificationBody>;
/// `tools.changed`, sequenced.
pub type ToolsChangedEvent = Sequenced<ToolsChanged>;
/// `steer`, sequenced.
pub type SteerEvent = Sequenced<Steer>;

tagged_union! {
    /// Anything the server may send.
    pub enum ServerMessage by "type" {
        /// The handshake.
        Connected(ConnectedEvent) = "connected",
        /// Keep-alive answer.
        Pong(PongEvent) = "pong",
        /// A failure.
        Error(ErrorEvent) = "error",
        /// A user message was stored.
        MessageAck(MessageAckEvent) = "message.ack",
        /// A user message is waiting.
        MessageQueued(MessageQueuedEvent) = "message.queued",
        /// A turn began.
        TurnStart(TurnStartEvent) = "turn.start",
        /// Answer text.
        AssistantDelta(AssistantDeltaEvent) = "assistant.delta",
        /// Reasoning text.
        ReasoningDelta(ReasoningDeltaEvent) = "reasoning.delta",
        /// A tool was called.
        ToolCall(ToolCallEvent) = "tool.call",
        /// A tool is still running.
        ToolProgress(ToolProgressEvent) = "tool.progress",
        /// A tool answered.
        ToolResult(ToolResultEvent) = "tool.result",
        /// A tool needs approval.
        ToolApprovalRequest(ToolApprovalRequestEvent) = "tool.approvalRequest",
        /// An advisory.
        Notice(NoticeEvent) = "notice",
        /// A turn finished.
        TurnEnd(TurnEndEvent) = "turn.end",
        /// A subagent's event.
        Subagent(SubagentEvent) = "subagent.event",
        /// The context bar's figure.
        ContextUsage(ContextUsageEvent) = "context.usage",
        /// Where a session stands.
        SessionStatus(SessionStatusEvent) = "session.status",
        /// The conversation was cleared.
        SessionReset(SessionResetEvent) = "session.reset",
        /// Replayed history.
        SessionReplay(SessionReplayEvent) = "session.replay",
        /// A suffix was dropped.
        SessionTruncated(SessionTruncatedEvent) = "session.truncated",
        /// A notification.
        Notification(NotificationEvent) = "notification",
        /// The tool list changed.
        ToolsChanged(ToolsChangedEvent) = "tools.changed",
        /// Steering echoed.
        Steer(SteerEvent) = "steer",
    }
}

/// Server events that are not part of a session's replayable history.
pub const UNSEQUENCED_SERVER_EVENTS: &[&str] = &["connected", "pong", "error"];

impl ServerMessage {
    /// The event's sequence number, or `None` for a connection-level event.
    ///
    /// What a replay buffer stores is exactly the events this returns `Some`
    /// for; a new event without a `seq` would silently drop from replay, and
    /// the completeness test over [`UNSEQUENCED_SERVER_EVENTS`] is what makes
    /// that a failing test instead.
    pub fn seq(&self) -> Option<u64> {
        match self {
            Self::Connected(_) | Self::Pong(_) | Self::Error(_) => None,
            Self::MessageAck(e) => Some(e.seq),
            Self::MessageQueued(e) => Some(e.seq),
            Self::TurnStart(e) => Some(e.seq),
            Self::AssistantDelta(e) => Some(e.seq),
            Self::ReasoningDelta(e) => Some(e.seq),
            Self::ToolCall(e) => Some(e.seq),
            Self::ToolProgress(e) => Some(e.seq),
            Self::ToolResult(e) => Some(e.seq),
            Self::ToolApprovalRequest(e) => Some(e.seq),
            Self::Notice(e) => Some(e.seq),
            Self::TurnEnd(e) => Some(e.seq),
            Self::Subagent(e) => Some(e.seq),
            Self::ContextUsage(e) => Some(e.seq),
            Self::SessionStatus(e) => Some(e.seq),
            Self::SessionReset(e) => Some(e.seq),
            Self::SessionReplay(e) => Some(e.seq),
            Self::SessionTruncated(e) => Some(e.seq),
            Self::Notification(e) => Some(e.seq),
            Self::ToolsChanged(e) => Some(e.seq),
            Self::Steer(e) => Some(e.seq),
        }
    }
}
