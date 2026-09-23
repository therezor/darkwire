//! A tool call cut off by the output token limit.
//!
//! When a completion stops at `max_tokens` in the middle of a call, the
//! arguments are a prefix of a JSON object. Running it would hand a tool
//! half an instruction: a `write` with half a file, an `exec` with half a
//! command. Storing it as a call would put a malformed call in history for
//! every later request to carry. So the loop keeps the calls that parse, drops
//! the rest, and tells the model which one it has to send again.

use darkwire_protocol::ToolCall;

/// The calls whose arguments parse, and the names of the ones that do not.
///
/// Empty arguments count as complete, the way `parse_tool_args` reads them:
/// a tool with no parameters is called with nothing.
pub fn split_cut_calls(calls: &[ToolCall]) -> (Vec<ToolCall>, Vec<String>) {
    let mut kept = Vec::with_capacity(calls.len());
    let mut cut = Vec::new();
    for call in calls {
        let complete = call.arguments_json.trim().is_empty()
            || serde_json::from_str::<serde_json::Value>(&call.arguments_json).is_ok();
        if complete {
            kept.push(call.clone());
        } else {
            cut.push(call.name.clone());
        }
    }
    (kept, cut)
}

/// What the model is told in the runtime half of the next iteration's prompt.
///
/// In the prompt rather than in history for the same reason as the text-call
/// correction: it costs no cached prefix and does not read as something the
/// operator said.
pub fn length_cut_correction(names: &[String], max_tokens: u64) -> String {
    let (subject, calls, verb) = phrase(names);
    format!(
        "## Correction

Your {subject} to {calls} {verb} cut off at the {max_tokens}-token output limit,
so the arguments were incomplete and nothing ran. Retry with shorter arguments:
split the work into several smaller calls if it does not fit in one."
    )
}

/// What a reader is told in the `length_cut` notice.
pub fn length_cut_notice(names: &[String], max_tokens: u64) -> String {
    let (subject, calls, verb) = phrase(names);
    format!(
        "The {subject} to {calls} {verb} cut off at the {max_tokens}-token limit. Asking the \
         model to retry."
    )
}

/// "call", the quoted names, and the verb that agrees with them.
fn phrase(names: &[String]) -> (&'static str, String, &'static str) {
    let calls = names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() == 1 {
        ("call", calls, "was")
    } else {
        ("calls", calls, "were")
    }
}
