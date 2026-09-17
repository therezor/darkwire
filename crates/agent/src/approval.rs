//! The tool approval gate.
//!
//! `darkwire-protocol` describes this end to end — a risk band per tool, a
//! policy per band, a scope per answer — but the loop is where it has to be
//! read, because the loop is the only thing that sits between a model asking
//! for `exec` and a shell running.
//!
//! The split is deliberate and it is the whole design:
//!
//!  - **The loop decides whether to ask.** That is a pure function of the
//!    tool's risk and the deployment's policy, so no transport can forget to
//!    check and no transport can decide the answer differently.
//!  - **The gate decides the answer**, and it is the gate that remembers one.
//!    `once | session` is scope *memory*, which needs a session-shaped store
//!    outliving the turn. So it belongs to the thing holding the connection to
//!    a human, not to a turn that will end in a minute.
//!  - **The loop owns the deadline.** A gate that never resolves — a browser
//!    tab closed on an open prompt — would otherwise hang the turn forever,
//!    and the turn is the only party that knows it is still waiting.
//!
//! An absent gate means nobody is there to ask, so an `ask` policy runs the
//! tool: that is what keeps a one-shot terminal turn working. A `deny` policy
//! is refused either way, since refusing needs no one to answer.

use darkwire_core::Result;
use darkwire_protocol::{ApprovalScope, ToolRisk};
use darkwire_providers::BoxFuture;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// One call, waiting on a decision. Mirrors the `tool.approvalRequest` event.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// The session the call was made in.
    pub session_key: String,
    /// The session a human is actually looking at.
    ///
    /// Equal to `session_key` for an ordinary turn, and different for one
    /// running inside a subagent: a subagent gets a session of its own, which
    /// exists for the length of one delegation. Scoping a `session` answer to
    /// *that* would make the button mean "once" — the operator picks "this
    /// session" while looking at their conversation, and the conversation is
    /// what they meant.
    pub root_session_key: String,
    /// Which agent is asking.
    ///
    /// Not part of the memory key: a session is bound to one agent, so the
    /// conversation already says which. It is here because a prompt raised
    /// against a session nobody is watching has to name the agent that wants to
    /// run something, and "an agent wants to run `exec`" is not a notification
    /// anyone can act on.
    pub agent_id: String,
    /// The turn the call belongs to.
    pub turn_id: String,
    /// The call.
    pub call_id: String,
    /// The tool.
    pub name: String,
    /// Parsed arguments when the model emitted valid JSON; the raw string
    /// otherwise.
    pub args: Value,
    /// The tool's declared risk.
    pub risk: ToolRisk,
    /// Wall-clock deadline, the same value the event carries.
    pub expires_at_ms: u64,
    /// The turn's cancellation.
    ///
    /// A gate that keeps pending prompts must drop this one when it fires —
    /// the loop stops waiting either way, and a prompt left on screen for a
    /// turn that has already ended is a decision that can no longer mean
    /// anything.
    pub token: CancellationToken,
}

/// What a gate answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovalDecision {
    /// Whether the call may run.
    pub approved: bool,
    /// What the answer covers.
    ///
    /// The loop never reads it: remembering that `exec` was approved for the
    /// rest of the session is the gate's job, and a loop that cached it would
    /// have to be told when the user revoked it.
    pub scope: Option<ApprovalScope>,
    /// Logged, never shown to the model.
    pub reason: Option<String>,
}

impl ApprovalDecision {
    /// Approved, with no scope and no reason.
    pub fn allow() -> ApprovalDecision {
        ApprovalDecision {
            approved: true,
            ..ApprovalDecision::default()
        }
    }

    /// Refused, with no scope and no reason.
    pub fn refuse() -> ApprovalDecision {
        ApprovalDecision::default()
    }
}

/// Who to ask before a tool whose permission is `ask` runs.
pub trait ApprovalGate: Send + Sync {
    /// Resolves when a decision is made.
    ///
    /// An error is treated as a refusal — a gate that fails open is not a
    /// gate. An `aborted` error is the exception and means the turn ended
    /// under the prompt, which is a cancellation rather than a denial.
    fn ask<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<ApprovalDecision>>;

    /// The answer this gate already holds, if it holds one.
    ///
    /// Asked before `tool.approvalRequest` goes out, and that is the whole
    /// point of it: a gate that remembers "this session" answers `ask` in the
    /// same tick it is called, so announcing the prompt first puts a card on
    /// every client and takes it away a millisecond later. The scope the
    /// operator chose is meant to stop the question being asked, not to answer
    /// it faster.
    ///
    /// Synchronous and non-committal. A gate that remembers nothing keeps the
    /// default, and one that does must give the same answer `ask` would.
    fn remembered(&self, request: &ApprovalRequest) -> Option<ApprovalDecision> {
        let _ = request;
        None
    }
}

/// Why a call was refused.
///
/// `Policy` never reached a human; the other two did, or should have. The
/// distinction is worth keeping because it is the difference between "this
/// deployment does not do that" and "you were asked and said no", and a model
/// that cannot tell them apart retries the first one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DenialReason {
    /// The deployment's policy is `deny`; nobody was asked.
    Policy,
    /// A person refused.
    Declined,
    /// Nobody answered before the deadline.
    Timeout,
}

/// What the model reads. Phrased to stop a retry loop, not to explain a UI.
pub fn denied_tool_result(name: &str, reason: DenialReason) -> String {
    let cause = match reason {
        DenialReason::Policy => {
            format!("the \"{name}\" tool is blocked by this deployment's approval policy")
        }
        DenialReason::Declined => format!("the user refused this call to \"{name}\""),
        DenialReason::Timeout => {
            format!("nobody answered the approval request for \"{name}\" in time")
        }
    };
    format!(
        "Denied: {cause}. The tool did not run. Do not call it again — continue without it, or \
         tell the user what you need and why."
    )
}

/// What a human reads, in a notice beside the tool card.
pub fn denied_notice(name: &str, reason: DenialReason) -> String {
    match reason {
        DenialReason::Policy => {
            format!("Blocked \"{name}\": the approval policy for this tool is \"deny\".")
        }
        DenialReason::Declined => format!("Denied \"{name}\": the call was refused."),
        DenialReason::Timeout => {
            format!("Denied \"{name}\": the approval request expired before it was answered.")
        }
    }
}
