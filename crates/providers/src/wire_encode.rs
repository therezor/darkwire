//! What a `chat/completions` request body actually looks like.
//!
//! Split out of the adapter because it has two readers rather than one: the
//! transport that sends the body, and the measurement that prices it for the
//! context inspector. The whole point of the split is that neither gets its
//! own copy. A hand-kept "what the model sees" projection somewhere else would
//! be a second statement of this module, correct on the day it was written and
//! wrong the first time a branch here changes.
//!
//! The one that had already gone wrong is why this exists. An assistant
//! message carries the model's `reasoning` beside its answer, [`encode_message`]
//! has never put it on the wire, and the inspector was measuring the stored
//! object, so a bar under the composer was billing text no provider ever
//! received.
//!
//! Two encoding decisions live here and are bug-compatibility with the
//! ecosystem rather than preference:
//!
//! - **Text-only content collapses to a plain string.** The array-of-parts
//!   form is correct per the OpenAI schema, and several local servers reject
//!   it for `system` and `tool` messages. The string form is understood
//!   everywhere.
//! - **Tool-call arguments cross the wire verbatim.** A model emitting
//!   malformed JSON is routine; parsing it here would turn that into a
//!   transport-layer failure. The string is preserved so the tool registry can
//!   reject it as a typed tool error the model gets to see and retry against.
//!
//! Kept pure, with no transport, no clock and no I/O, which is what lets the
//! measuring side call it as often as a turn iterates.

use ghostai_protocol::{ChatMessage, ContentPart, ToolDefinition};
use serde::{Deserialize, Serialize};

/// One part of a multimodal message on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WireContentPart {
    /// Text.
    #[serde(rename = "text")]
    Text {
        /// The text.
        text: String,
    },
    /// An image the provider fetches or decodes itself.
    #[serde(rename = "image_url")]
    ImageUrl {
        /// The `{url}` object the wire wraps the address in.
        image_url: WireImageUrl,
    },
}

/// The address of a wire image: a data URI or an absolute URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireImageUrl {
    /// The data URI or URL.
    pub url: String,
}

/// A message's content: a plain string, parts, or `null` for a tool-only turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WireContent {
    /// Text-only content, collapsed.
    Text(String),
    /// Content with at least one image.
    Parts(Vec<WireContentPart>),
}

/// A tool call as the wire nests it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireToolCall {
    /// The model's id for the call.
    pub id: String,
    /// Always `function`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Name and verbatim arguments.
    pub function: WireFunctionCall,
}

/// The `function` half of a [`WireToolCall`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFunctionCall {
    /// The tool's name.
    pub name: String,
    /// The arguments exactly as the model wrote them.
    pub arguments: String,
}

/// One message as the body carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMessage {
    /// `system`, `user`, `assistant` or `tool`.
    pub role: String,
    /// The content. `null`, not `""`, for an assistant turn that only called
    /// tools: several providers reject an empty string where they accept null.
    pub content: Option<WireContent>,
    /// The calls an assistant turn made. Omitted when there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<WireToolCall>>,
    /// The call a tool message answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// A tool definition as the body carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireTool {
    /// Always `function`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The three fields a provider reads.
    pub function: WireFunction,
}

/// The `function` half of a [`WireTool`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireFunction {
    /// The name the model calls it by.
    pub name: String,
    /// The sentence that decides whether the model reaches for it.
    pub description: String,
    /// The argument schema.
    pub parameters: ghostai_protocol::json::Object,
}

/// One canonical part on the wire.
pub fn encode_part(part: &ContentPart) -> WireContentPart {
    match part {
        ContentPart::Text(text) => WireContentPart::Text {
            text: text.text.clone(),
        },
        // A `file` part is a workspace reference, and this wire format has
        // nowhere to put one: it is meant to have been turned into text or an
        // image before the request got here. Reaching this branch means a
        // caller went straight to a provider, so render the reference rather
        // than dropping it: a model told the path can still reach for a tool,
        // and a silently missing attachment is the failure this whole design
        // was about.
        ContentPart::File(file) => WireContentPart::Text {
            text: format!("[attachment: {} · {}]", file.path, file.mime_type),
        },
        // An inline image becomes a data URI; a signed URL is passed through
        // for the provider to fetch. Both are what `image_url` accepts.
        ContentPart::Image(image) => {
            let url = match &image.data {
                Some(data) => format!("data:{};base64,{data}", image.mime_type),
                None => image.url.clone().unwrap_or_default(),
            };
            WireContentPart::ImageUrl {
                image_url: WireImageUrl { url },
            }
        }
    }
}

/// Parts on the wire: a newline-joined string when every part is text, the
/// array form otherwise.
pub fn encode_content(parts: &[ContentPart]) -> WireContent {
    let encoded: Vec<WireContentPart> = parts.iter().map(encode_part).collect();
    let texts: Option<Vec<&str>> = encoded
        .iter()
        .map(|part| match part {
            WireContentPart::Text { text } => Some(text.as_str()),
            WireContentPart::ImageUrl { .. } => None,
        })
        .collect();
    match texts {
        Some(texts) => WireContent::Text(texts.join("\n")),
        None => WireContent::Parts(encoded),
    }
}

/// One canonical message on the wire.
///
/// An assistant's `reasoning` is absent by omission rather than by a line
/// deleting it: this object is built from the fields the wire has, and the
/// model's own thinking is not one of them.
pub fn encode_message(message: &ChatMessage) -> WireMessage {
    match message {
        ChatMessage::System(system) => WireMessage {
            role: "system".into(),
            content: Some(WireContent::Text(system.content.clone())),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage::User(user) => WireMessage {
            role: "user".into(),
            content: Some(encode_content(&user.content)),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage::Tool(tool) => WireMessage {
            role: "tool".into(),
            content: Some(WireContent::Text(tool.content.clone())),
            tool_calls: None,
            tool_call_id: Some(tool.tool_call_id.clone()),
        },
        ChatMessage::Assistant(assistant) => {
            let content = encode_content(&assistant.content);
            let tool_calls: Vec<WireToolCall> = assistant
                .tool_calls
                .iter()
                .map(|call| WireToolCall {
                    id: call.id.clone(),
                    kind: "function".into(),
                    function: WireFunctionCall {
                        name: call.name.clone(),
                        arguments: call.arguments_json.clone(),
                    },
                })
                .collect();
            let is_empty_text = matches!(&content, WireContent::Text(text) if text.is_empty());
            WireMessage {
                role: "assistant".into(),
                content: if is_empty_text && !tool_calls.is_empty() {
                    None
                } else {
                    Some(content)
                },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: None,
            }
        }
    }
}

/// The definitions as the body carries them.
///
/// Three fields of a `ToolDefinition` reach a provider. `risk`, `source` and
/// anything else the registry hangs on a tool are this project's own
/// bookkeeping: they drive an approval prompt and a badge, and no model has
/// ever seen one.
pub fn encode_tools(tools: &[ToolDefinition]) -> Vec<WireTool> {
    tools
        .iter()
        .map(|tool| WireTool {
            kind: "function".into(),
            function: WireFunction {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            },
        })
        .collect()
}
