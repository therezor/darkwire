//! A conversation summarised into a new session that opens with the summary.
//!
//! The old session is left as it was. The new one starts with the exchange
//! that happened, a request for a summary and the summary as the answer, so
//! roles alternate and the summary is the first thing on screen. Its metadata
//! names the session it continues, under [`COMPACTED_FROM_KEY`].
//!
//! One request, no tools, and a system prompt of its own: the agent's prompt
//! describes how to act, and this asks for something else. The history is
//! windowed to the context left after that prompt and the summary's cap, so a
//! nearly full conversation fits without its oldest turns silently dropping.

use std::sync::Arc;

use darkwire_core::db::blocking;
use darkwire_core::messages::{
    AssistantOptions, assistant_message, system_message, text_of, text_part, user_message,
};
use darkwire_core::session_store::{AppendOptions, CreateSession, SessionRecord};
use darkwire_core::{ErrorKind, Result, WireError};
use darkwire_protocol::{ChatMessage, ContentPart};
use darkwire_providers::estimate_tokens;
use darkwire_providers::types::ChatRequest;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{AgentLoop, history_rows};
use crate::history_window::{FixedCost, RowPrices, history_budget, window_records};

/// Where a continued session names the one it carries on from.
pub const COMPACTED_FROM_KEY: &str = "compactedFrom";

/// The most a summary may be, in tokens. Capped below the agent's own limit
/// so the history keeps most of the window.
pub const SUMMARY_MAX_TOKENS: u64 = 4_096;

/// What the summary request runs under in place of the agent's prompt.
pub const SUMMARY_PROMPT: &str = "You are summarising a conversation so it can carry on in a \
new session that starts from your summary. Write it for the assistant who continues: what \
the user wants, what has been done and decided, the state of any work in progress, files \
and names that matter, and anything still open. Keep facts exact. Leave out greetings, \
dead ends that no longer matter, and tool output that was only read once. Write prose and \
short lists, not a transcript.";

/// The question the new session opens with.
pub const SUMMARY_REQUEST: &str = "Summarise the conversation so far.";

/// Added to a title, once, to name the session that continues it.
const CONTINUED_SUFFIX: &str = " (continued)";

/// What to summarise, and where the summary goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactRequest {
    /// The conversation to summarise.
    pub session_key: String,
    /// The session to create. Must not exist yet.
    pub new_key: String,
    /// What the summary should keep or leave out, in the user's words.
    pub instructions: Option<String>,
}

/// The title of the session that continues `title`.
#[must_use]
pub fn continued_title(title: &str) -> String {
    let base = title.trim().trim_end_matches(CONTINUED_SUFFIX).trim();
    if base.is_empty() {
        "Conversation".to_owned() + CONTINUED_SUFFIX
    } else {
        format!("{base}{CONTINUED_SUFFIX}")
    }
}

/// The question that opens the new session, with the user's instructions.
fn opening_question(instructions: Option<&str>) -> String {
    match instructions.map(str::trim).filter(|text| !text.is_empty()) {
        Some(instructions) => format!("{SUMMARY_REQUEST}\n\n{instructions}"),
        None => SUMMARY_REQUEST.to_owned(),
    }
}

/// History as a summary request carries it: an attachment becomes its name,
/// and a trailing tool call no result answers is dropped, since a provider
/// refuses a call left open.
fn summarisable(mut messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    while matches!(messages.last(), Some(ChatMessage::Assistant(last)) if !last.tool_calls.is_empty())
    {
        messages.pop();
    }
    for message in &mut messages {
        let parts = match message {
            ChatMessage::User(user) => &mut user.content,
            ChatMessage::Assistant(assistant) => &mut assistant.content,
            _ => continue,
        };
        for part in parts.iter_mut() {
            let name = match part {
                ContentPart::File(file) => file.name.clone().unwrap_or_else(|| file.path.clone()),
                ContentPart::Image(_) => "an image".to_owned(),
                ContentPart::Text(_) => continue,
            };
            *part = text_part(format!("[attachment: {name}]"));
        }
    }
    messages
}

impl AgentLoop {
    /// Summarises `request.session_key` into a new session and returns it.
    ///
    /// # Errors
    ///
    /// The summary request failed or came back empty, or the store refused the
    /// new session. Nothing is written in either case.
    pub async fn compact(
        &self,
        request: &CompactRequest,
        token: &CancellationToken,
    ) -> Result<SessionRecord> {
        let inner = &self.inner;
        let store = Arc::clone(&inner.store);
        let key = request.session_key.clone();
        let old = blocking(move || store.get_session(&key))
            .await?
            .ok_or_else(|| {
                WireError::new(ErrorKind::NotFound, "There is no session to summarise.")
                    .with_detail("sessionKey", request.session_key.as_str())
            })?;
        let rows = history_rows(&inner.store, &request.session_key).await?;
        if rows.is_empty() {
            return Err(WireError::new(
                ErrorKind::InvalidInput,
                "There is nothing to summarise yet.",
            ));
        }

        let question = opening_question(request.instructions.as_deref());
        let max_output_tokens = inner.config.max_tokens.min(SUMMARY_MAX_TOKENS);
        let budget = history_budget(&FixedCost {
            context_window_tokens: inner.config.context_window_tokens,
            prompt_tokens: estimate_tokens(SUMMARY_PROMPT) + estimate_tokens(&question),
            max_output_tokens,
            calibration: None,
        });
        // Images are priced as the names they become.
        let mut prices = RowPrices::new(false);
        let history = summarisable(window_records(rows, None, budget, &mut prices).into_messages());

        let mut messages = Vec::with_capacity(history.len() + 2);
        messages.push(ChatMessage::System(system_message(SUMMARY_PROMPT)));
        messages.extend(history);
        messages.push(ChatMessage::User(user_message(question.clone())));
        let summary_request = ChatRequest {
            model: inner.model_id.clone(),
            messages,
            tools: Vec::new(),
            tool_choice: None,
            max_tokens: Some(max_output_tokens),
            temperature: inner.config.temperature,
            reasoning_effort: None,
            cache_key: Some(request.session_key.clone()),
        };
        let result = inner.provider.chat(&summary_request, token).await?;
        let summary = text_of(&ChatMessage::Assistant(result.message.clone()));
        if summary.trim().is_empty() {
            return Err(WireError::new(
                ErrorKind::Provider,
                "The model answered the summary request with nothing.",
            ));
        }

        let store = Arc::clone(&inner.store);
        let new_key = request.new_key.clone();
        let mut metadata = Map::new();
        metadata.insert(
            COMPACTED_FROM_KEY.to_owned(),
            Value::String(old.key.clone()),
        );
        blocking(move || {
            let created = store.ensure_session(
                &new_key,
                CreateSession {
                    title: Some(continued_title(&old.title)),
                    origin: Some(old.origin.clone()),
                    workspace_id: Some(old.workspace_id.clone()),
                    agent_id: old.agent_id.clone(),
                    metadata: Some(metadata),
                },
            )?;
            let opened = store.append_many(
                &new_key,
                vec![
                    ChatMessage::User(user_message(question)),
                    ChatMessage::Assistant(assistant_message(summary, AssistantOptions::default())),
                ],
                &AppendOptions::default(),
            );
            if let Err(error) = opened {
                let _ = store.delete_session(&new_key);
                return Err(error);
            }
            Ok(created)
        })
        .await
    }
}
