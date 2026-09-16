//! Constructors and accessors for the canonical message union.
//!
//! The shapes themselves live in `darkwire-protocol`: they cross the wire, so
//! the schema is their single source of truth. What lives here is the small set
//! of operations everything downstream would otherwise reimplement: building a
//! user message from a string, and reading the text back out of one.
//!
//! [`text_of`] is the load-bearing one. `content` is a list of parts precisely
//! so a message can carry images, but most consumers (a channel renderer, a
//! derived session title, a log line, an assertion) want the words. Written
//! inline, that is a filter/map/join that gets subtly different at every call
//! site, and the differences only show up on multimodal input.

use darkwire_protocol::{
    AssistantMessage, AssistantRole, ChatMessage, ContentPart, FilePart, FileTag, ImagePart,
    ImageTag, SystemMessage, SystemRole, TextPart, TextTag, ToolCall, ToolMessage, ToolRole,
    UserMessage, UserRole,
};

/// A text part.
pub fn text_part(text: impl Into<String>) -> ContentPart {
    ContentPart::Text(TextPart {
        tag: TextTag,
        text: text.into(),
    })
}

/// Where an image's bytes come from. Exactly one of the two is meaningful.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// Inline base64.
    Data(String),
    /// An absolute URL the provider fetches itself.
    Url(String),
}

/// An image part, as a *provider* takes it.
///
/// A `url` must be absolute and reachable from wherever the model runs. A
/// workspace file is neither: it is a [`file_part`], and it becomes one of
/// these only at request time. Putting a relative signed URL here is what used
/// to send every attachment nowhere.
pub fn image_part(mime_type: impl Into<String>, source: ImageSource) -> ContentPart {
    let (data, url) = match source {
        ImageSource::Data(data) => (Some(data), None),
        ImageSource::Url(url) => (None, Some(url)),
    };
    ContentPart::Image(ImagePart {
        tag: ImageTag,
        mime_type: mime_type.into(),
        data,
        url,
    })
}

/// The optional half of a [`file_part`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileDetails {
    /// What the user called the file.
    pub name: Option<String>,
    /// Size on disk.
    pub size_bytes: Option<u64>,
}

/// A workspace file, by reference. See `FilePart` for why not by value.
pub fn file_part(
    path: impl Into<String>,
    mime_type: impl Into<String>,
    details: FileDetails,
) -> ContentPart {
    ContentPart::File(FilePart {
        tag: FileTag,
        mime_type: mime_type.into(),
        path: path.into(),
        name: details.name,
        size_bytes: details.size_bytes,
    })
}

/// A system message.
pub fn system_message(content: impl Into<String>) -> SystemMessage {
    SystemMessage {
        role: SystemRole,
        content: content.into(),
    }
}

/// What a user or assistant message is built from: a string for the common
/// case, or parts for multimodal input.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// One text part.
    Text(String),
    /// Explicit parts.
    Parts(Vec<ContentPart>),
}

impl Content {
    fn into_parts(self) -> Vec<ContentPart> {
        match self {
            Content::Text(text) => vec![text_part(text)],
            Content::Parts(parts) => parts,
        }
    }
}

impl From<&str> for Content {
    fn from(text: &str) -> Self {
        Content::Text(text.to_owned())
    }
}

impl From<String> for Content {
    fn from(text: String) -> Self {
        Content::Text(text)
    }
}

impl From<Vec<ContentPart>> for Content {
    fn from(parts: Vec<ContentPart>) -> Self {
        Content::Parts(parts)
    }
}

/// A user message.
pub fn user_message(content: impl Into<Content>) -> UserMessage {
    UserMessage {
        role: UserRole,
        content: content.into().into_parts(),
    }
}

/// The optional half of an [`assistant_message`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AssistantOptions {
    /// The calls this turn made.
    pub tool_calls: Vec<ToolCall>,
    /// Reasoning text, kept beside the answer.
    pub reasoning: Option<String>,
}

/// An assistant message.
pub fn assistant_message(
    content: impl Into<Content>,
    options: AssistantOptions,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole,
        content: content.into().into_parts(),
        tool_calls: options.tool_calls,
        reasoning: options.reasoning,
    }
}

/// The optional half of a [`tool_message`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolOptions {
    /// The call failed. An explicit flag, because inspecting the content for
    /// an `Error` prefix misfires on any tool whose legitimate output begins
    /// with that word.
    pub is_error: bool,
    /// The result was head+tail truncated.
    pub truncated: bool,
}

/// A tool result.
pub fn tool_message(
    tool_call_id: impl Into<String>,
    name: impl Into<String>,
    content: impl Into<String>,
    options: ToolOptions,
) -> ToolMessage {
    ToolMessage {
        role: ToolRole,
        tool_call_id: tool_call_id.into(),
        name: name.into(),
        content: content.into(),
        is_error: options.is_error,
        truncated: options.truncated,
    }
}

/// The text of a message: the words, and only the words.
///
/// Parts are joined with a newline rather than concatenated: a provider that
/// splits one answer across several text parts means them as separate blocks,
/// and gluing them together silently merges the last word of one paragraph
/// into the first word of the next.
///
/// Image and file parts contribute nothing, deliberately. This feeds
/// `derive_session_title`, and a session titled
/// `[file: uploads/ab12cd34-scan.pdf]` is worse than one left untitled, which
/// is what an attachment-only first message gets.
pub fn text_of(message: &ChatMessage) -> String {
    match message {
        ChatMessage::System(system) => system.content.clone(),
        ChatMessage::Tool(tool) => tool.content.clone(),
        ChatMessage::User(user) => join_text(&user.content),
        ChatMessage::Assistant(assistant) => join_text(&assistant.content),
    }
}

fn join_text(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.text.as_str()),
            ContentPart::Image(_) | ContentPart::File(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a message carries image parts: the check before a vision request.
///
/// A `file` part is **not** an image, however it is going to be materialised.
/// If it were, the strip-images degradation could fire on a request whose
/// attachments have not been read yet and delete the references before
/// anything looked at them.
pub fn has_images(message: &ChatMessage) -> bool {
    let parts = match message {
        ChatMessage::System(_) | ChatMessage::Tool(_) => return false,
        ChatMessage::User(user) => &user.content,
        ChatMessage::Assistant(assistant) => &assistant.content,
    };
    parts
        .iter()
        .any(|part| matches!(part, ContentPart::Image(_)))
}

/// Drops image parts, keeping everything else.
///
/// The `strip images` step of the provider degradation ladder: a model that
/// rejects an image should still answer the question that came with it, rather
/// than failing the turn outright.
///
/// Phrased as "not an image" rather than "is text" because those stopped being
/// the same thing when `file` parts arrived: keeping only text would delete an
/// un-materialised attachment reference from the request as a side effect of a
/// degradation that has nothing to do with it.
pub fn without_images(message: ChatMessage) -> ChatMessage {
    if !has_images(&message) {
        return message;
    }
    match message {
        ChatMessage::User(mut user) => {
            user.content
                .retain(|part| !matches!(part, ContentPart::Image(_)));
            ChatMessage::User(user)
        }
        ChatMessage::Assistant(mut assistant) => {
            assistant
                .content
                .retain(|part| !matches!(part, ContentPart::Image(_)));
            ChatMessage::Assistant(assistant)
        }
        other @ (ChatMessage::System(_) | ChatMessage::Tool(_)) => other,
    }
}
