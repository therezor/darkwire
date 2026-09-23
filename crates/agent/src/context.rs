//! What a turn on a session would actually send to the model, without running
//! one.
//!
//! This is the measurement behind the context strip in the web UI and the
//! terminal's context command, and it lives here for a reason worth stating: it
//! cannot live in `darkwire-core`, which has no access to the estimators or to
//! the prompt the loop assembles; and it must not live in the server, because a
//! terminal drives the loop in-process and never speaks HTTP. This crate
//! already depends on both halves, so putting it here adds no dependency edge
//! and gives both front ends one implementation instead of two that drift.
//!
//! **The figures are of the request body, not of what is stored.** They come
//! from the estimators in `darkwire-providers`, which run the same encoder the
//! transport sends with — so a field that never reaches a provider is never
//! billed. Two such fields are easy to bill by mistake: an assistant record's
//! reasoning, which is kept beside the answer to be shown and excluded from
//! history replay, and a tool's risk and source, which drive an approval prompt
//! and a badge. That is a second reason this cannot move down to core: pricing
//! the body is the provider layer's knowledge.
//!
//! It returns storage *records* rather than wire types. The REST layer narrows
//! them; the terminal never needs to.

use std::sync::Arc;

use darkwire_core::messages::{system_message, user_message};
use darkwire_core::{Result, SessionStore, StoredMessageRecord};
use darkwire_protocol::{ChatMessage, ToolDefinition};
use darkwire_providers::{estimate_message_tokens, estimate_tool_tokens};
use indexmap::IndexMap;

use crate::agent_loop::{AgentLoop, PromptPreview, PromptPreviewInput};
use crate::history_window::{FixedCost, history_budget, windowed_history};
use crate::prompt::runtime_reminder;

/// Where the tokens went.
///
/// Named sections rather than one number, because the question this exists to
/// answer is *which* block got too big.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextBreakdown {
    /// The cached prefix.
    pub system_prompt: usize,
    /// The definitions, as one array.
    pub tools: usize,
    /// The window, priced as the body carries it.
    ///
    /// Which is why adding a reasoning model to a conversation does not move
    /// this number: reasoning is stored beside the answer and dropped by the
    /// encoder, so it costs nothing on the wire and is charged nothing here.
    pub messages: usize,
    /// The trailing turn: live state, the turn's delimiter, a correction.
    ///
    /// Reported apart from the system prompt because it is the one section
    /// billed at full price on every iteration — the three above it are the
    /// provider's cached prefix. A single "prompt" figure would average the two
    /// together and hide the only number here anyone can act on.
    pub runtime_block: usize,
}

impl ContextBreakdown {
    /// The sections as the wire carries them: a map, so a new section is not a
    /// wire change.
    ///
    /// In request order, which is also cached-then-not: the three sections a
    /// provider can serve from its prefix cache, then the tail that is re-read
    /// at full price on every iteration.
    pub fn to_map(self) -> IndexMap<String, f64> {
        let mut map = IndexMap::new();
        map.insert("systemPrompt".to_owned(), as_f64(self.system_prompt));
        map.insert("tools".to_owned(), as_f64(self.tools));
        map.insert("messages".to_owned(), as_f64(self.messages));
        map.insert("runtimeBlock".to_owned(), as_f64(self.runtime_block));
        map
    }
}

/// A token count as the wire carries it. Counts are far below 2^53.
#[allow(
    clippy::cast_precision_loss,
    reason = "a token estimate over a bounded window is well below 2^53"
)]
fn as_f64(value: usize) -> f64 {
    value as f64
}

/// What a turn on this session would carry, measured.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextReport {
    /// The conversation.
    pub session_key: String,
    /// The cached prefix: the system message, without the per-iteration tail.
    pub system_prompt: String,
    /// The trailing turn, as the loop would send it — before the reminder
    /// envelope, which is framing rather than content anyone reading this panel
    /// needs.
    ///
    /// Empty in raw mode, where the operator's one template is the whole system
    /// message and there is no second half to show.
    ///
    /// **The strings here are for reading; the figures beside them are
    /// body-shaped.** The runtime figure prices this text inside the message
    /// the loop wraps it in, so it is deliberately larger than an estimate of
    /// the string alone — and the same goes for the system prompt. Someone
    /// eventually notices the two do not match and "fixes" one of them; this is
    /// the note saying which way round it goes.
    pub runtime_block: String,
    /// The definitions as the provider would receive them.
    ///
    /// Returned as well as measured, because the breakdown says a number and
    /// the only follow-up question anyone has is *which* tools. A number with
    /// nothing behind it is the part of an inspector that gets asked about.
    ///
    /// The list is fuller than the figure: risk and source are on every entry
    /// because the panel badges them, and neither is billed, because neither is
    /// sent.
    pub tools: Vec<ToolDefinition>,
    /// The window as it would be sent, in storage order.
    pub messages: Vec<StoredMessageRecord>,
    /// What the next request would cost.
    pub estimated_tokens: usize,
    /// The window it has to fit.
    pub context_window_tokens: u64,
    /// Where those tokens went.
    pub breakdown: ContextBreakdown,
}

/// The measurement, given a prompt somebody has already built.
pub struct MeasureContext<'a> {
    /// Where the conversation lives.
    pub store: &'a SessionStore,
    /// The definitions as the turn would send them.
    pub tools: &'a [ToolDefinition],
    /// The conversation.
    pub session_key: &'a str,
    /// Both halves of the system message, already assembled.
    pub prompt: &'a PromptPreview,
    /// The window the total is measured against.
    pub context_window_tokens: u64,
}

/// What the window depends on beyond [`MeasureContext`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MeasureWindow {
    /// The completion the request reserves room for. `0` reserves none.
    pub max_output_tokens: u64,
    /// The row that opened the turn in progress, or `None` between turns.
    pub opening_seq: Option<i64>,
}

/// The fixed half of a request, priced as the body carries it.
///
/// The two halves of the prompt are messages by the time they are sent, not
/// strings, and the loop omits the trailing one entirely in raw mode and the
/// tools entirely when there are none. Shared with the loop, which budgets the
/// history against the same figures.
pub(crate) fn prompt_costs(prompt: &PromptPreview, tools: &[ToolDefinition]) -> ContextBreakdown {
    let system_prompt =
        estimate_message_tokens(&ChatMessage::System(system_message(&prompt.static_prompt)));
    let runtime_block = if prompt.runtime_block.is_empty() {
        0
    } else {
        estimate_message_tokens(&ChatMessage::User(user_message(runtime_reminder(
            &prompt.runtime_block,
        ))))
    };
    let tools = if tools.is_empty() {
        0
    } else {
        estimate_tool_tokens(tools)
    };
    ContextBreakdown {
        system_prompt,
        tools,
        messages: 0,
        runtime_block,
    }
}

/// The measurement, given a prompt somebody has already built.
///
/// Without a [`MeasureWindow`] no output is reserved and no turn is in
/// progress. A caller that knows the agent's `max_tokens` passes it through
/// [`measure_context_with`], which is what makes the figure match the request.
pub fn measure_context(input: &MeasureContext<'_>) -> Result<ContextReport> {
    measure_context_with(input, &MeasureWindow::default())
}

/// The measurement, given a prompt somebody has already built.
///
/// Split out of [`describe_context`] for one caller: the loop, which composes
/// exactly this prompt on every iteration and can therefore report the context
/// as it grows without paying for a second assembly. Previewing a prompt runs
/// every contributor's static section, which may do I/O. Running that once per
/// iteration to draw a bar would be a real cost on a forty-step turn, and it is
/// the whole reason this seam exists.
///
/// The history is read through the helper `build_request` uses, with the same
/// budget, so the report is the window the next request sends. The two callers
/// can differ by a few characters (the loop's runtime block names the iteration
/// it is on, the preview always says 1), which is under a token.
pub fn measure_context_with(
    input: &MeasureContext<'_>,
    window: &MeasureWindow,
) -> Result<ContextReport> {
    let fixed = prompt_costs(input.prompt, input.tools);
    // The window the loop sends, read by the same helper: the message cap,
    // the legal-start rules, then the token budget.
    let budget = history_budget(&FixedCost {
        context_window_tokens: input.context_window_tokens,
        prompt_tokens: fixed.system_prompt + fixed.runtime_block + fixed.tools,
        max_output_tokens: window.max_output_tokens,
    });
    let messages =
        windowed_history(input.store, input.session_key, window.opening_seq, budget)?.records;
    let message_tokens: usize = messages
        .iter()
        .map(|record| estimate_message_tokens(&record.message))
        .sum();
    let breakdown = ContextBreakdown {
        messages: message_tokens,
        ..fixed
    };

    Ok(ContextReport {
        session_key: input.session_key.to_owned(),
        system_prompt: input.prompt.static_prompt.clone(),
        runtime_block: input.prompt.runtime_block.clone(),
        tools: input.tools.to_vec(),
        messages,
        estimated_tokens: fixed.system_prompt + fixed.tools + message_tokens + fixed.runtime_block,
        context_window_tokens: input.context_window_tokens,
        breakdown,
    })
}

/// Measures the next turn's prompt for a session that already exists.
///
/// Returns `None` for a session with no stored row — a conversation that has
/// not started has no context to describe, and inventing an empty one would
/// report a system prompt for a workspace nobody chose.
///
/// The prompt comes from the loop rather than from a second assembly of it,
/// which is the whole reason this takes one: memory and skills arrive as
/// contributors attached to that object, and a reimplementation elsewhere
/// cannot see them.
pub async fn describe_context(
    store: &Arc<SessionStore>,
    agent_loop: &AgentLoop,
    tools: &[ToolDefinition],
    input: &PromptPreviewInput,
    context_window_tokens: u64,
) -> Result<Option<ContextReport>> {
    if store.get_session(&input.session_key)?.is_none() {
        return Ok(None);
    }

    let prompt = agent_loop.preview_prompt(input).await?;
    measure_context_with(
        &MeasureContext {
            store,
            tools,
            session_key: &input.session_key,
            prompt: &prompt,
            context_window_tokens,
        },
        &MeasureWindow {
            max_output_tokens: agent_loop.max_tokens(),
            opening_seq: None,
        },
    )
    .map(Some)
}
