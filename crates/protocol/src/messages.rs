//! The canonical message shapes.
//!
//! These cross the wire in both the WebSocket protocol and the REST session
//! routes, so they live here rather than beside the store that persists them.
//! Two properties matter downstream:
//!
//! - `AssistantMessage::tool_calls[].id` and `ToolMessage::tool_call_id` are
//!   the pairing keys the history walker follows to guarantee no `tool`
//!   message reaches a provider without its originating `assistant` turn.
//!   Orphaned tool results are a provider 400.
//! - Tool-call arguments stay a verbatim JSON *string*. Models emit malformed
//!   JSON often enough that parsing must be the tool registry's job, where a
//!   failure becomes a typed tool error the model can retry against, rather
//!   than a parse error in the transport layer.

use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::json::{MAX_SAFE_INTEGER, literal, positive, tagged_union};

/// Who wrote a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    /// The operator's standing instructions.
    System,
    /// A person, or a channel speaking for one.
    User,
    /// The model.
    Assistant,
    /// A tool result answering one of the model's calls.
    Tool,
}

literal! {
    /// The `type` of a [`TextPart`].
    pub struct TextTag = "text";
}
literal! {
    /// The `type` of an [`ImagePart`].
    pub struct ImageTag = "image";
}
literal! {
    /// The `type` of a [`FilePart`].
    pub struct FileTag = "file";
}

/// Plain text in a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct TextPart {
    /// Always `text`.
    #[serde(rename = "type")]
    pub tag: TextTag,
    /// The text.
    pub text: String,
}

/// An image as a *provider* takes it: inline base64 (`data`) or a URL it can
/// fetch itself (`url`).
///
/// `url` must be absolute and reachable from wherever the model runs. A
/// workspace file is not that — it is a [`FilePart`], and becomes one of these
/// only at request time, when the attachment is read. A relative signed URL
/// here is what once sent every attachment nowhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ImagePart {
    /// Always `image`.
    #[serde(rename = "type")]
    pub tag: ImageTag,
    /// The image's media type.
    #[garde(length(utf16, min = 1))]
    pub mime_type: String,
    /// Base64 bytes, when inlined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// An absolute URL the provider fetches itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A workspace file somebody attached to a message.
///
/// A reference, never bytes. The path outlives any signed URL, so history
/// replayed a month later still resolves to the same file — and the same part
/// describes a screenshot, a CSV and a 200 MB archive, so nothing upstream has
/// to branch on the media type to decide what an attachment *is*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct FilePart {
    /// Always `file`.
    #[serde(rename = "type")]
    pub tag: FileTag,
    /// The file's media type.
    #[garde(length(utf16, min = 1))]
    pub mime_type: String,
    /// Workspace-relative, as returned by the upload endpoint.
    #[garde(length(utf16, min = 1))]
    pub path: String,
    /// What the user called it. The path is mangled for safety; this is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Size on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub size_bytes: Option<u64>,
}

tagged_union! {
    /// One piece of a user or assistant message.
    pub enum ContentPart by "type" {
        /// Text.
        Text(TextPart) = "text",
        /// An image the provider can consume.
        Image(ImagePart) = "image",
        /// A workspace file.
        File(FilePart) = "file",
    }
}

/// A tool call as the model made it.
///
/// Flat, unlike the OpenAI wire shape's `{id, type: 'function', function: {name,
/// arguments}}`: the nesting carries no information, and flattening removes a
/// layer of indirection from every wire adapter and from the history walker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolCall {
    /// The model's id for the call, echoed by the tool message that answers it.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// The tool's advertised name.
    #[garde(length(utf16, min = 1))]
    pub name: String,
    /// The arguments exactly as the model wrote them.
    pub arguments_json: String,
}

literal! {
    /// The `role` of a [`SystemMessage`].
    pub struct SystemRole = "system";
}
literal! {
    /// The `role` of a [`UserMessage`].
    pub struct UserRole = "user";
}
literal! {
    /// The `role` of an [`AssistantMessage`].
    pub struct AssistantRole = "assistant";
}
literal! {
    /// The `role` of a [`ToolMessage`].
    pub struct ToolRole = "tool";
}

/// The operator's standing instructions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SystemMessage {
    /// Always `system`.
    pub role: SystemRole,
    /// The prompt text.
    pub content: String,
}

/// A person's message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct UserMessage {
    /// Always `user`.
    pub role: UserRole,
    /// Text, images and attached files.
    #[garde(dive)]
    pub content: Vec<ContentPart>,
}

/// The model's reply, with any tool calls it made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct AssistantMessage {
    /// Always `assistant`.
    pub role: AssistantRole,
    /// The answer.
    #[garde(dive)]
    pub content: Vec<ContentPart>,
    /// The calls this turn made, if any.
    #[serde(default)]
    #[garde(dive)]
    pub tool_calls: Vec<ToolCall>,
    /// Reasoning text, kept beside the answer rather than inside `content`.
    ///
    /// Its own field so it can be shown in its own collapsible block, and so a
    /// surface replaying a stored conversation can fold it the same way without
    /// re-parsing the content parts to find where the answer starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// How long the model spent on that reasoning, where it was measured.
    ///
    /// Stored beside the text so a replayed turn reads like a live one: the
    /// summary row a folded run shows carries a duration, and a clock that
    /// started when the prompt opened cannot supply it for a run that happened
    /// last week. Absent rather than zero when nothing measured it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub reasoning_ms: Option<u64>,
}

/// A tool's answer to one call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct ToolMessage {
    /// Always `tool`.
    pub role: ToolRole,
    /// The `id` of the [`ToolCall`] this answers.
    #[garde(length(utf16, min = 1))]
    pub tool_call_id: String,
    /// The tool's name.
    #[garde(length(utf16, min = 1))]
    pub name: String,
    /// The result text.
    pub content: String,
    /// A failed tool call is still a legal history entry — the model needs to
    /// see the error to recover. An explicit flag, because inspecting the
    /// content for an `Error` prefix misfires on any tool whose legitimate
    /// output begins with that word.
    #[serde(default)]
    pub is_error: bool,
    /// Set when the result was head+tail truncated to fit the tool-output cap.
    #[serde(default)]
    pub truncated: bool,
    /// How long the call took, for the same reason as [`AssistantMessage::reasoning_ms`]:
    /// the card and the folded row both say it, and nothing can work it out
    /// after the fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub duration_ms: Option<u64>,
}

tagged_union! {
    /// Any message in a conversation.
    pub enum ChatMessage by "role" {
        /// The operator's instructions.
        System(SystemMessage) = "system",
        /// A person.
        User(UserMessage) = "user",
        /// The model.
        Assistant(AssistantMessage) = "assistant",
        /// A tool result.
        Tool(ToolMessage) = "tool",
    }
}

/// A persisted message: the message plus its storage identity.
///
/// An envelope rather than a flattened intersection, because the message is a
/// union discriminated on `role` and merging storage fields into it would cost
/// the discriminator on both sides of the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct StoredMessage {
    /// The row id.
    #[garde(length(utf16, min = 1))]
    pub id: String,
    /// The session it belongs to.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// Storage's per-session ordering, and the only stable way to *address* a
    /// message: editing, regenerating and branching all name a point in the
    /// conversation, and an id cannot express "and everything after this". It
    /// is already the REST pagination cursor, so publishing it reveals nothing.
    #[garde(range(min = 1, max = MAX_SAFE_INTEGER))]
    #[schemars(transform = positive)]
    pub seq: u64,
    /// When it was written.
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub created_at_ms: u64,
    /// Groups every message produced by one user turn, including tool traffic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// The message itself.
    #[garde(dive)]
    pub message: ChatMessage,
}

/// Token accounting as reported by the provider.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct Usage {
    /// Tokens in the request.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub prompt_tokens: u64,
    /// Tokens the model produced.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub completion_tokens: u64,
    /// Both, as the provider summed them.
    #[serde(default)]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub total_tokens: u64,
    /// Prompt-cache hits, where the provider reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub cached_tokens: Option<u64>,
    /// Tokens spent thinking, where the provider reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(range(max = MAX_SAFE_INTEGER))]
    pub reasoning_tokens: Option<u64>,
}

/// Completion tokens per second, or `None` when the figure would be a lie.
///
/// Zero elapsed or zero completion tokens returns nothing rather than zero or
/// infinity: a turn that produced no tokens has no rate, and a turn measured at
/// zero milliseconds was not measured.
#[allow(
    clippy::cast_precision_loss,
    reason = "token counts are far below 2^53"
)]
pub fn tokens_per_second(usage: &Usage, elapsed_ms: f64) -> Option<f64> {
    if elapsed_ms <= 0.0 || usage.completion_tokens == 0 {
        return None;
    }
    Some((usage.completion_tokens as f64) * 1000.0 / elapsed_ms)
}

/// The timings a rate can be derived from.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnTiming {
    /// Time the model actually spent emitting tokens, where that was measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_ms: Option<f64>,
    /// The completion tokens produced inside `generation_ms`, and only those.
    ///
    /// Not the turn's `completion_tokens`, and the difference is the point: a
    /// request whose whole reply arrived in one frame — which is how Ollama
    /// sends a bare tool call, at any length — is charged for its tokens and
    /// measured at zero. Pairing them keeps such a request out of both sides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_tokens: Option<f64>,
    /// Wall time across the whole turn, including everything that is not
    /// decoding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<f64>,
}

/// The rate to show a person, divided by generation time where there is any.
///
/// The whole-turn wall clock is the wrong divisor: a local model loading its
/// weights for forty seconds, a tool that runs for ten and an approval nobody
/// answered for a minute all land in it, and the figure that comes out reports
/// a model as slow when the model was not running. `generation_ms` and
/// `generation_tokens` are one measurement taken twice, so both are used or
/// neither is — a reply that arrived in a single frame has tokens and a window
/// of zero, and pairing its tokens with another request's window overstates
/// the rate. The fallback to the wall clock keeps every turn recorded before
/// the window was measured, and every turn whose replies all came in one
/// frame, showing a figure; both take the same branch, which is why this tests
/// for zero and not merely for absence.
pub fn turn_rate(usage: &Usage, timing: &TurnTiming) -> Option<f64> {
    if let (Some(ms), Some(tokens)) = (timing.generation_ms, timing.generation_tokens)
        && ms > 0.0
        && tokens > 0.0
    {
        return Some(tokens * 1000.0 / ms);
    }
    timing
        .elapsed_ms
        .and_then(|elapsed| tokens_per_second(usage, elapsed))
}

/// Why a turn stopped. Every value is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model answered.
    Complete,
    /// Somebody stopped it.
    Aborted,
    /// The tool-iteration cap was reached.
    MaxIterations,
    /// The wall-clock cap was reached.
    WallTimeout,
    /// Something failed.
    Error,
}
