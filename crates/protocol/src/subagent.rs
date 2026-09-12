//! The vocabulary a delegated run is recorded under.
//!
//! Small, and here rather than beside the loop that writes it, because four
//! layers read it: the session store filters on the origin, the loop writes
//! the lineage, the server hands the bag through, and the browser reads the
//! pointer back to fetch a subagent's transcript after a reload. A constant
//! duplicated across that span is a string that eventually differs in one of
//! them.
//!
//! Lineage lives in the session's metadata bag rather than in a column. It
//! costs no schema, no index and no query surface, and nothing needs to search
//! by it — the parent holds a map of its children, and each child holds a
//! pointer back.

use garde::Validate;
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json::Object;

/// The origin a subagent's session is recorded under.
///
/// Load-bearing rather than descriptive, and the load it bears is *not* that
/// the store hides these rows: a delegated run's turn is the thing anyone
/// debugging a bad answer has to read, and a row nothing lists is a transcript
/// with no way in. What it bears is the narrowing in both directions — the web
/// sidebar excludes it so a shortlist of thirty is thirty conversations rather
/// than thirty rows of machinery, while the sessions listing passes nothing and
/// lists them all.
pub const SUBAGENT_ORIGIN: &str = "subagent";

/// Where a subagent's session records what delegated to it.
pub const SUBAGENT_METADATA_KEY: &str = "subagent";

/// Where a *parent* session records which call produced which child session.
const SUBAGENT_RUNS_METADATA_KEY: &str = "subagentRuns";

/// What a subagent's session knows about the call that started it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentLineage {
    /// The session that delegated.
    pub parent_session_key: String,
    /// The turn the delegating call belonged to.
    pub parent_turn_id: String,
    /// The tool call that delegated.
    pub parent_call_id: String,
    /// The agent this run executes as.
    pub agent_id: String,
    /// 1 for a subagent of the session's own agent.
    pub depth: u64,
}

/// What a parent remembers about one delegation, once the events are gone.
///
/// The session key alone would do to *fetch* the run; the agent and its label
/// are here so a rebuilt transcript can name the card before the fetch
/// resolves. Without them a reloaded conversation would render "Subagent run"
/// over a spinner and only learn whose run it was afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Validate)]
#[serde(rename_all = "camelCase")]
#[garde(allow_unvalidated)]
pub struct SubagentRunRef {
    /// The subagent's own session.
    #[garde(length(utf16, min = 1))]
    pub session_key: String,
    /// The agent that ran it.
    pub agent_id: String,
    /// The agent's label at the time.
    pub label: String,
}

/// `callId → run`, read out of a parent session's metadata.
///
/// Tolerant of anything that is not the expected shape, because the bag is
/// untyped storage that other things also write, and one malformed entry must
/// not stop a transcript rendering.
pub fn subagent_runs_of(metadata: &Object) -> IndexMap<String, SubagentRunRef> {
    let mut runs = IndexMap::new();
    let Some(Value::Object(raw)) = metadata.get(SUBAGENT_RUNS_METADATA_KEY) else {
        return runs;
    };
    for (call_id, value) in raw {
        let Value::Object(entry) = value else {
            continue;
        };
        let Some(session_key) = entry.get("sessionKey").and_then(Value::as_str) else {
            continue;
        };
        if session_key.is_empty() {
            continue;
        }
        let text = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        runs.insert(
            call_id.clone(),
            SubagentRunRef {
                session_key: session_key.to_owned(),
                agent_id: text("agentId"),
                label: text("label"),
            },
        );
    }
    runs
}

/// Adds one run to a parent's map, returning the whole metadata bag.
pub fn with_subagent_run(metadata: &Object, call_id: &str, run: &SubagentRunRef) -> Object {
    let mut runs = subagent_runs_of(metadata);
    runs.insert(call_id.to_owned(), run.clone());
    let mut next = metadata.clone();
    let encoded = runs
        .into_iter()
        .map(|(id, run)| (id, serde_json::to_value(run).unwrap_or(Value::Null)))
        .collect::<serde_json::Map<String, Value>>();
    next.insert(
        SUBAGENT_RUNS_METADATA_KEY.to_owned(),
        Value::Object(encoded),
    );
    next
}

/// What the model reads about a delegation whose operator wrote nothing.
///
/// Here rather than beside the loop because the settings UI has to show it: the
/// field is optional, and a placeholder that invents an *example* of what an
/// operator might write leaves them unable to find out what happens if they
/// write nothing. Takes the label rather than a binding, and is called with the
/// *target's* current label, so the sentence follows a rename — which is the
/// reason to show it rather than to prefill a box with it.
pub fn default_subagent_prompt(label: &str) -> String {
    format!(
        "Hand a self-contained task to the \"{label}\" agent and wait for its answer. It does \
         not see this conversation, so say everything it needs; it replies with a written \
         result, not with raw tool output."
    )
}
