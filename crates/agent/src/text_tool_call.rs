//! A tool call the model wrote into its answer instead of calling.
//!
//! Local models do this, and they do it most often on the iteration *after* a
//! tool returned an error — the point at which a turn most needs to recover.
//! Observed from `liquid/lfm2-24b-a2b`, whose first call in the same turn was a
//! correctly structured one:
//!
//! ```text
//! The search tool is currently rate-limited. I will try using the `fetch` tool…
//!
//! <tool_output>
//! <tool_call>
//! {"name": "fetch", "arguments": ["https://www.bbc.com/news"]}
//! </tool_call>
//! </tool_output>
//! ```
//!
//! The provider reports no tool calls, so the loop reads that as a finished
//! answer and the turn ends `complete`. The user is shown a JSON blob as the
//! reply, and nothing anywhere says the model tried to act and failed to.
//!
//! **This module only detects. It deliberately does not execute.** Running a
//! call the model merely *described* is a different and worse bug: "how do I
//! call `exec`?" answered with an example would become an `exec`. The loop's
//! response is to tell the model what it did wrong and let it try once more —
//! which costs one iteration and cannot act on something nobody asked for.
//!
//! Detection is deliberately narrow. A name that matches no registered tool is
//! prose about tools, not an attempt to use one, and the difference matters
//! because the correction is worthless when the model was only explaining
//! itself.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

/// The wrappers models reach for, as one alternation so matches stay in the
/// order they appear in the text.
///
/// One pattern per tag rather than a back-reference on the opening one: a
/// back-reference is what the JavaScript original used and is not something a
/// linear-time engine can express, and three alternatives say the same thing.
static BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?is)<tool_call>(.*?)</tool_call>|<function_call>(.*?)</function_call>|<tool_use>(.*?)</tool_use>",
    )
    .unwrap_or_else(|_| unreachable!())
});

/// A fenced block, which some models use instead of a pseudo-XML tag.
static FENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?is)```(?:json|tool_code|tool_call)?\s*(\{.*?\})\s*```")
        .unwrap_or_else(|_| unreachable!())
});

/// A bare object with the two keys a call has, for models that wrap nothing.
static BARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)\{[^{}]*"name"\s*:\s*"[A-Za-z0-9_.\-]{1,64}".{0,400}?\}"#)
        .unwrap_or_else(|_| unreachable!())
});

/// The `"name": "…"` field, wherever it sits.
static NAMED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)"name"\s*:\s*"([A-Za-z0-9_.\-]{1,64})""#).unwrap_or_else(|_| unreachable!())
});

/// Whether the text carries one of the tag forms at all.
static ANY_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)<(tool_call|function_call|tool_use)>").unwrap_or_else(|_| unreachable!())
});

/// The name of a tool this text tried to call, if it tried to call one.
///
/// `None` covers every ordinary answer, which is almost all of them — so this
/// runs the cheap substring guards before any regex work.
pub fn text_tool_call_name(text: &str, known: &[String]) -> Option<String> {
    if text.is_empty() || known.is_empty() {
        return None;
    }
    // A call, however it is wrapped, always names the tool in a `"name"` field
    // or sits in one of the tag forms. Without one of those there is nothing to
    // find, and this is the path every normal answer takes.
    if !text.contains("\"name\"") && !ANY_TAG.is_match(text) {
        return None;
    }

    let names: HashSet<&str> = known.iter().map(String::as_str).collect();

    for pattern in [&*BLOCK, &*FENCE, &*BARE] {
        for captures in pattern.captures_iter(text) {
            // `BLOCK` has one group per tag and only the matched tag's group is
            // populated; `FENCE` captures the object; `BARE` matches the whole
            // object and captures nothing. Falling back to the match itself is
            // what makes all three one loop.
            let body = (1..captures.len())
                .find_map(|index| captures.get(index))
                .map_or_else(|| captures[0].to_owned(), |group| group.as_str().to_owned());
            let Some(field) = NAMED.captures(&body) else {
                continue;
            };
            let name = &field[1];
            if names.contains(name) {
                return Some(name.to_owned());
            }
        }
    }

    None
}

/// What the model is told, in the runtime half of the next iteration's prompt.
///
/// In the prompt rather than as a message in the conversation, for two reasons:
/// the runtime half is rewritten every iteration anyway so this costs no cached
/// prefix, and a correction appended as a `user` message would read in the
/// transcript as something the operator said.
///
/// Named tools rather than a general scolding, because "use the tool interface"
/// is advice a model that just failed to use the tool interface cannot act on.
/// Saying *which* tool it was reaching for turns it into a single concrete
/// instruction.
pub fn text_tool_call_correction(name: &str) -> String {
    format!(
        "## Correction

Your previous message contained a call to `{name}` written as text in your
reply. That is not a tool call and nothing ran. Tool calls have to be made through
the tool-calling interface, as structured calls, never written out in the message
body, and never wrapped in tags.

Call `{name}` now, properly. If you cannot, say so in plain words instead and do
not write out another call."
    )
}
