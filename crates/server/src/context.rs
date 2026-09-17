//! What the agent would send to the model, for one session.
//!
//! Kept here rather than inside the sessions route, because the interesting
//! twenty lines are not glue — they are the *agent-resolution policy*, and two
//! surfaces disagreeing about which agent a conversation is measured against is
//! exactly the class of bug that made the measurement a shared function in the
//! first place.
//!
//! The policy: a session is measured against **its own agent**, since that
//! agent's tools, prompt and context window are what a turn here would actually
//! carry, and a meter read against another agent's window is simply wrong.
//! Unless the session names an agent that has since been deleted, in which case
//! the honest answer is the one a turn *would* get — the default — reported
//! through `requested_agent_id` so a reader can be told what they are looking
//! at rather than quietly shown something else.

use darkwire_agent::{MeasureContext, PromptPreviewInput, measure_context};
use darkwire_core::{Result, to_stored_message};
use darkwire_protocol::messages::{ChatMessage, StoredMessage};
use darkwire_protocol::rest::ContextResponse;

use crate::runtime::ServerRuntime;

/// Measures one session, or `None` when there is nothing to measure.
///
/// `None` rather than a failure, because the two callers want different things
/// from it: the route turns it into a 404, and a chat channel says "nothing to
/// measure yet" — a conversation that has been created but not spoken in is
/// normal, not an error.
pub async fn build_context_response(
    runtime: &dyn ServerRuntime,
    session_key: &str,
) -> Result<Option<ContextResponse>> {
    let store = runtime.store();
    let Some(session) = store.get_session(session_key)? else {
        return Ok(None);
    };

    let bound = session.agent_id.unwrap_or_default();
    let missing = !bound.is_empty() && !runtime.agents().iter().any(|entry| entry.id == bound);
    let effective: Option<&str> = if missing || bound.is_empty() {
        None
    } else {
        Some(bound.as_str())
    };

    let agent = runtime.agent(effective)?;
    // This session's list, not the agent's: under lazy discovery the two
    // differ, and the inspector's promise is "what the provider was sent".
    let tools = agent.session_tools(session_key);
    let prompt = agent
        .system_prompt(&PromptPreviewInput {
            session_key: session_key.to_owned(),
            channel: Some("web".to_owned()),
            // The effective id, not the stored one: this reaches the preview,
            // and a preview built for an agent that will not run is a preview
            // of something that is not going to happen.
            agent_id: effective.map(str::to_owned),
        })
        .await?;

    // The measurement itself lives in `darkwire-agent`, so every surface reports
    // the same numbers from the same code rather than a second implementation
    // of the windowing rules.
    let report = measure_context(&MeasureContext {
        store: &store,
        tools: &tools,
        session_key,
        prompt: &prompt,
        context_window_tokens: agent.context_window_tokens().into(),
    })?;

    Ok(Some(ContextResponse {
        session_key: report.session_key,
        system_prompt: report.system_prompt,
        runtime_block: report.runtime_block,
        tools: report.tools,
        messages: report
            .messages
            .iter()
            .map(to_stored_message)
            .map(without_reasoning)
            .collect(),
        estimated_tokens: u64::try_from(report.estimated_tokens).unwrap_or(u64::MAX),
        context_window_tokens: report.context_window_tokens,
        breakdown: report.breakdown.to_map(),
        agent_id: Some(agent.id().to_owned()),
        // Present only on a fallback, so a client can treat its presence as the
        // whole signal rather than comparing two ids on every response.
        requested_agent_id: if missing { Some(bound) } else { None },
    }))
}

/// The message as the request carries it: without the model's own reasoning.
///
/// Applied here rather than in the shared mapper, and the difference matters.
/// That mapper serves the transcript endpoints too, where reasoning is the
/// collapsible block beside an answer and dropping it would empty a feature.
/// This route answers "what is in the window", the wire has never carried
/// reasoning, and a payload that shipped it invited the panel to show it.
///
/// The two mappers deliberately differ; anything reading both should expect
/// that.
fn without_reasoning(mut stored: StoredMessage) -> StoredMessage {
    if let ChatMessage::Assistant(assistant) = &mut stored.message {
        assistant.reasoning = None;
    }
    stored
}
