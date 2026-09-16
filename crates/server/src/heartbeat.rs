//! The heartbeat's decide / run / evaluate triad.
//!
//! A heartbeat reads a task file on an interval and decides whether there is
//! anything to do. Two of its three steps are **not agent turns** — they are
//! single provider requests carrying one tool with a required tool choice.
//!
//! That is a deliberate refusal of the obvious design. Registering a
//! `heartbeat` tool in the shared registry would leak it into every ordinary
//! chat turn and into every subagent's, and teardown is per source, so hiding
//! it again would need a fourth tool source invented for one yes/no. Worse, an
//! agent turn for a classification writes a user message and an assistant
//! message into a session — forever, every thirty minutes.
//!
//! So only the middle step is a turn. This module is everything around it, and
//! it is pure: every function takes and returns plain values, which is what
//! lets the interesting half — what happens when a cheap model answers badly —
//! be tested from a [`ChatResult`] literal rather than a live endpoint.
//!
//! **The failure rule is fail-closed on acting, fail-loud on reporting.** A
//! decision that cannot be read becomes a skip with a warning, never a run. The
//! alternative — defaulting to running — is an unbounded agent turn started on
//! garbage every thirty minutes, billed to the operator, and the model that
//! produced the garbage is by construction the cheapest one in the install.

use std::sync::LazyLock;

use darkwire_core::messages::{system_message, text_part, user_message};
use darkwire_protocol::json::Object;
use darkwire_protocol::messages::{AssistantMessage, ChatMessage, ContentPart};
use darkwire_protocol::tools::{ToolDefinition, ToolRisk, ToolSource};
use darkwire_providers::ChatResult;
use serde::Deserialize;
use serde_json::{Value, json};

/// How much of a task file is worth paying to classify, every interval,
/// forever.
pub const MAX_TASK_FILE_BYTES: usize = 64 * 1024;

/// A skip reason goes in a column and onto a card; the model does not know
/// that.
const MAX_REASON_LENGTH: usize = 256;

/// Builds a tool definition from a JSON Schema literal.
///
/// The schema is written as JSON rather than assembled field by field because
/// it *is* JSON on the wire, and a builder would only make the shape harder to
/// compare with what the model is actually sent.
fn tool(name: &str, description: &str, parameters: Value) -> ToolDefinition {
    let parameters: Object = match parameters {
        Value::Object(map) => map.into_iter().collect(),
        // Unreachable for the two literals below, and an empty schema is the
        // only answer that cannot mislead a model about what to send.
        _ => Object::new(),
    };
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        parameters,
        risk: ToolRisk::Safe,
        source: ToolSource::Builtin,
        annotations: None,
    }
}

/// What the model is told to send instead of prose.
///
/// `instruction` is optional on purpose: the model's job here is the decision,
/// and a missing phrasing is not a reason to refuse a run it did commit to.
///
/// A `LazyLock` rather than a `const`: the argument schema is an ordered map,
/// which cannot be built in a const context.
pub static HEARTBEAT_TOOL: LazyLock<ToolDefinition> = LazyLock::new(|| {
    tool(
        "heartbeat",
        "Decide whether the task file asks for work right now.",
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["action", "reason"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["skip", "run"],
                    "description": "run only when the file asks for something that is due now.",
                },
                "reason": {
                    "type": "string",
                    "description": "One sentence: why there is nothing to do, or what is due.",
                },
                "instruction": {
                    "type": "string",
                    "description": "When action is run: the message to send to the agent.",
                },
            },
        }),
    )
});

/// The second decision: whether the run's result is worth interrupting anyone.
pub static HEARTBEAT_RESULT_TOOL: LazyLock<ToolDefinition> = LazyLock::new(|| {
    tool(
        "heartbeat_result",
        "Decide whether what the agent did is worth telling the user about.",
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["notify", "title"],
            "properties": {
                "notify": {
                    "type": "boolean",
                    "description": "Whether this is worth interrupting the user for.",
                },
                "title": {
                    "type": "string",
                    "description": "A short headline, under ten words.",
                },
                "summary": {"type": "string", "description": "One or two sentences."},
            },
        }),
    )
});

/// Which way the decision went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatAction {
    /// Nothing is due; no turn runs.
    Skip,
    /// Something is due; the agent is asked to do it.
    Run,
}

/// What the decision step concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatDecision {
    /// Which way it went.
    pub action: HeartbeatAction,
    /// Populated for both outcomes; becomes the run's skip reason on a skip.
    pub reason: String,
    /// What to send the agent. Always non-empty when the action is a run.
    pub instruction: String,
    /// Non-empty when the model answered badly enough to be worth recording.
    pub warnings: Vec<String>,
}

/// What the evaluation step concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatEvaluation {
    /// Whether this is worth interrupting anyone about.
    pub notify: bool,
    /// The headline.
    pub title: String,
    /// One or two sentences, or empty.
    pub summary: String,
    /// Non-empty when the model did not answer the question.
    pub warnings: Vec<String>,
}

/// The decision tool's arguments, as the model writes them.
#[derive(Debug, Deserialize)]
struct DecisionArguments {
    action: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    instruction: Option<String>,
}

/// The evaluation tool's arguments, as the model writes them.
#[derive(Debug, Deserialize)]
struct EvaluationArguments {
    notify: bool,
    title: String,
    #[serde(default)]
    summary: Option<String>,
}

/// Cuts to `limit` characters, ellipsis included.
///
/// Counted in characters rather than bytes so a multi-byte reason is not cut
/// mid-code-point, and `limit` is a display cap rather than a storage one.
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let kept: String = text.chars().take(limit.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// The first tool call by name, or `None`.
fn tool_call_named<'a>(message: &'a AssistantMessage, name: &str) -> Option<&'a str> {
    message
        .tool_calls
        .iter()
        .find(|call| call.name == name)
        .map(|call| call.arguments_json.as_str())
}

/// The prose half of an answer, for the case where that is all there is.
fn text_of(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

// Decide

/// What the decision request needs to say.
#[derive(Debug, Clone)]
pub struct DecideMessagesInput {
    /// Workspace-relative, for the model's benefit and the reason text.
    pub file: String,
    /// The file's contents, already capped.
    pub contents: String,
    /// Formatted for the model, so "tomorrow" in a task file means something.
    pub now_iso: String,
}

/// The two messages the decision request carries.
pub fn build_decide_messages(input: &DecideMessagesInput) -> Vec<ChatMessage> {
    let DecideMessagesInput {
        file,
        contents,
        now_iso,
    } = input;
    vec![
        ChatMessage::System(system_message(format!(
            "You decide whether a task file asks for work right now. \
             The current time is {now_iso}. \
             Answer only by calling the heartbeat tool — never with prose. \
             Choose skip unless something in the file is actually due: this runs on a \
             timer forever, and a run that was not needed costs the user real money \
             and a real interruption."
        ))),
        ChatMessage::User(user_message(vec![text_part(format!(
            "Task file `{file}`:\n\n```\n{contents}\n```"
        ))])),
    ]
}

/// Reads the decision out of a completion.
///
/// Every failure lands on a skip, and each carries a warning so the run history
/// says why rather than showing an unexplained no-op.
///
/// The no-tool-call branch is not defensive programming — it **will** happen.
/// The resilience decorator's drop-tool-choice rung strips a required tool
/// choice and retries whenever a provider objects to it, so a model that
/// answers in prose is a normal outcome of a normal degradation, not a broken
/// install.
pub fn read_decision(result: &ChatResult, file: &str) -> HeartbeatDecision {
    let Some(arguments_json) = tool_call_named(&result.message, &HEARTBEAT_TOOL.name) else {
        return HeartbeatDecision {
            action: HeartbeatAction::Skip,
            reason: "The model did not answer with a decision.".to_owned(),
            instruction: String::new(),
            warnings: vec![
                "The heartbeat model answered without calling the decision tool, so this interval was skipped."
                    .to_owned(),
            ],
        };
    };

    let parsed = match serde_json::from_str::<DecisionArguments>(arguments_json) {
        Ok(parsed) if parsed.action == "skip" || parsed.action == "run" => parsed,
        Ok(parsed) => {
            let detail = format!("action {} is not skip or run", parsed.action);
            return unreadable_decision(&detail);
        }
        Err(error) => return unreadable_decision(&error.to_string()),
    };

    if parsed.action == "skip" {
        return HeartbeatDecision {
            action: HeartbeatAction::Skip,
            reason: truncate(
                if parsed.reason.is_empty() {
                    "Nothing due."
                } else {
                    &parsed.reason
                },
                MAX_REASON_LENGTH,
            ),
            instruction: String::new(),
            warnings: Vec::new(),
        };
    }

    // A run with no phrasing still runs. The model committed to the decision;
    // only the wording was missing, and refusing on that would turn a working
    // heartbeat into one that silently never acts.
    let phrasing = parsed
        .instruction
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    HeartbeatDecision {
        action: HeartbeatAction::Run,
        reason: truncate(
            if parsed.reason.is_empty() {
                "The task file asks for work."
            } else {
                &parsed.reason
            },
            MAX_REASON_LENGTH,
        ),
        instruction: match phrasing {
            Some(text) => text.to_owned(),
            None => format!("Read `{file}` and do what it asks."),
        },
        warnings: if phrasing.is_none() {
            vec![
                "The heartbeat model chose to run without saying what to do; the task file was used as-is."
                    .to_owned(),
            ]
        } else {
            Vec::new()
        },
    }
}

fn unreadable_decision(detail: &str) -> HeartbeatDecision {
    HeartbeatDecision {
        action: HeartbeatAction::Skip,
        reason: truncate(
            &format!("The model's decision could not be read ({detail})."),
            MAX_REASON_LENGTH,
        ),
        instruction: String::new(),
        warnings: vec![format!(
            "The heartbeat model sent arguments that did not parse: {detail}."
        )],
    }
}

// Evaluate

/// What the evaluation request needs to say.
#[derive(Debug, Clone)]
pub struct EvaluateMessagesInput {
    /// What the agent was asked to do.
    pub instruction: String,
    /// What it answered.
    pub output: String,
}

/// The two messages the evaluation request carries.
pub fn build_evaluate_messages(input: &EvaluateMessagesInput) -> Vec<ChatMessage> {
    let EvaluateMessagesInput {
        instruction,
        output,
    } = input;
    vec![
        ChatMessage::System(system_message(
            "You decide whether an unattended agent run is worth interrupting someone about. \
             Answer only by calling the heartbeat_result tool. \
             Choose notify only for something the user would want to know now — work \
             finished, a decision needed, something broken. Routine progress is not worth a notification.",
        )),
        ChatMessage::User(user_message(vec![text_part(format!(
            "The agent was asked:\n{instruction}\n\nIt answered:\n{output}"
        ))])),
    ]
}

/// Reads the evaluation, defaulting to **notifying**.
///
/// The opposite default to [`read_decision`], and for a symmetric reason:
/// there, failing open costs an unwanted agent turn; here it costs a toast. A
/// notification nobody needed is a minor annoyance, and a finished run nobody
/// was told about is invisible — so the cheap mistake is the one to make.
pub fn read_evaluation(result: &ChatResult, fallback_title: &str) -> HeartbeatEvaluation {
    let parsed = tool_call_named(&result.message, &HEARTBEAT_RESULT_TOOL.name)
        .and_then(|raw| serde_json::from_str::<EvaluationArguments>(raw).ok())
        .filter(|parsed| !parsed.title.is_empty());

    let Some(parsed) = parsed else {
        return HeartbeatEvaluation {
            notify: true,
            title: fallback_title.to_owned(),
            summary: text_of(&result.message),
            warnings: vec![
                "The heartbeat model did not say whether this was worth a notification.".to_owned(),
            ],
        };
    };

    HeartbeatEvaluation {
        notify: parsed.notify,
        title: truncate(&parsed.title, MAX_REASON_LENGTH),
        summary: parsed.summary.unwrap_or_default(),
        warnings: Vec::new(),
    }
}
