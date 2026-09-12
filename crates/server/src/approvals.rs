//! The approval gate a connected client answers.
//!
//! The decision is split in two: the loop decides *whether to ask*, which is a
//! pure function of the tool's risk band and the deployment's policy, and the
//! gate decides *what the answer is* and how long it holds. This is the second
//! half for anything with a socket attached — the web UI, and every channel that
//! bridges through the hub.
//!
//! The whole of it is a map of parked senders. The loop emits
//! `tool.approvalRequest` as an ordinary event, the hub forwards it like every
//! other event, and the answer arrives later as an inbound `tool.approve` frame
//! naming the same `call_id`. Nothing here sends anything, which is what keeps
//! the gate usable from a channel that renders an approval as two buttons in a
//! chat app rather than as a card in a browser.
//!
//! The decisions that are not obvious:
//!
//!  - **A remembered answer is keyed by tool name, not by arguments.** That is
//!    what `session` and `always` mean, and a memory keyed by arguments would be
//!    a cache nobody can predict. It is also why the UI has to say what it is
//!    asking for: approving `exec` for the session approves the *next* `exec`
//!    too, whatever it turns out to be.
//!  - **A refusal is remembered exactly like an approval.** "No, and stop
//!    asking" is a thing users mean, and a scope that only ever widened
//!    permission would be a scope that only works in one direction.
//!  - **The deadline is enforced here as well as in the loop.** The loop's copy
//!    stops the *turn* waiting; this one stops the *map* growing. A prompt whose
//!    tab was closed has nothing left to answer it, and an entry nobody will
//!    ever resolve is a leak the loop cannot see.
//!  - **Cancellation resolves, it does not fail.** The loop already stopped
//!    racing this decision when the token fired; answering with an error would
//!    turn a cancelled turn into a tool failure the model then sees. A denial
//!    nobody reads is harmless.
//!
//! `always` is remembered for the lifetime of the process and no longer. A
//! durable "never ask me about this tool again" is a settings write, and a
//! decision persisted through a path nothing can revoke would be worse than one
//! that expires with the server.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use ghostai_agent::{ApprovalDecision, ApprovalGate, ApprovalRequest};
use ghostai_core::{Clock, Result, SystemClock};
use ghostai_protocol::tools::ApprovalScope;
use ghostai_providers::BoxFuture;
use parking_lot::Mutex;
use tokio::sync::oneshot;

/// What a client answered, kept past the call that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RememberedDecision {
    approved: bool,
    scope: ApprovalScope,
}

/// A parked request and the channel that settles it.
struct PendingApproval {
    request: ApprovalRequest,
    settle: oneshot::Sender<ApprovalDecision>,
}

/// Told about a request nobody is looking at, so something can go and fetch a
/// person.
///
/// A separate concern from parking the decision, and deliberately a *sink*
/// rather than a store: the gate is built before the server that owns the
/// notification table, and it has no business knowing what the other end does
/// with this.
///
/// Raised once, when the request is parked. Not on the timeout: by then the
/// answer is already decided and a notification saying "this needed you five
/// minutes ago" is worse than none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnattendedApproval {
    /// The session nobody was watching when the prompt was raised.
    pub session_key: String,
    /// The agent that asked.
    pub agent_id: String,
    /// The tool it wants to run.
    pub tool_name: String,
    /// When the prompt stops being answerable.
    pub expires_at_ms: u64,
}

/// How many clients can see a session.
///
/// `None` for the whole hook means nothing is tracking that, which is every path
/// except the live server — the CLI's one-shot runs and most tests — and those
/// are treated as attended so a fixture does not have to wire a hub to test
/// approvals.
pub type WatcherCount = Arc<dyn Fn(&str) -> usize + Send + Sync>;

/// Raised when a prompt is parked against a session nobody is watching.
pub type UnattendedSink = Arc<dyn Fn(UnattendedApproval) + Send + Sync>;

/// How the gate is built.
#[derive(Clone, Default)]
pub struct HubApprovalGateOptions {
    /// The clock the deadline is measured against. Defaults to the host's.
    pub clock: Option<Arc<dyn Clock>>,
    /// How many clients can see a session.
    pub watchers: Option<WatcherCount>,
    /// Where an unwatched prompt is announced.
    pub on_unattended: Option<UnattendedSink>,
}

/// The parked decisions, the scope memory, and the deadline that bounds both.
pub struct HubApprovalGate {
    clock: Arc<dyn Clock>,
    /// Parked senders, keyed by the tool call the client will name in its
    /// answer.
    pending: Mutex<HashMap<String, PendingApproval>>,
    /// `session` scope: session key to tool name to decision.
    by_session: Mutex<HashMap<String, HashMap<String, RememberedDecision>>>,
    /// `always` scope: agent id to tool name to decision, across every session.
    ///
    /// Keyed by agent rather than by tool alone, and that is a security boundary
    /// rather than bookkeeping. Two agents can be configured with deliberately
    /// different tool sets and approval policies; a standing "always allow
    /// `exec`" granted while using a permissive agent would otherwise
    /// pre-approve it for a locked-down one, silently undoing the restriction an
    /// operator set up. "Always" means for this agent, on every session, until
    /// the process ends.
    always: Mutex<HashMap<String, HashMap<String, RememberedDecision>>>,
    watchers: Option<WatcherCount>,
    on_unattended: Option<UnattendedSink>,
}

impl std::fmt::Debug for HubApprovalGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubApprovalGate")
            .field("pending", &self.pending_count())
            .finish_non_exhaustive()
    }
}

impl Default for HubApprovalGate {
    fn default() -> HubApprovalGate {
        HubApprovalGate::new(HubApprovalGateOptions::default())
    }
}

impl HubApprovalGate {
    /// A gate over the host clock, with no unattended sink.
    pub fn new(options: HubApprovalGateOptions) -> HubApprovalGate {
        HubApprovalGate {
            clock: options.clock.unwrap_or_else(|| Arc::new(SystemClock)),
            pending: Mutex::new(HashMap::new()),
            by_session: Mutex::new(HashMap::new()),
            always: Mutex::new(HashMap::new()),
            watchers: options.watchers,
            on_unattended: options.on_unattended,
        }
    }

    /// Which session a `session`-scoped answer belongs to.
    ///
    /// The conversation, not the turn's own session — those differ when the turn
    /// is a subagent's, because a subagent gets a session that exists for the
    /// length of one delegation. Scoping to *that* would make "this session"
    /// mean "once", which is not what the button says: the operator is looking
    /// at their conversation when they press it, and the conversation is what
    /// they meant.
    ///
    /// It is also what makes [`HubApprovalGate::clear_session`] on a
    /// conversation settle a prompt its subagent parked, rather than leaving one
    /// behind for a session that has gone.
    fn scope_of(request: &ApprovalRequest) -> &str {
        if request.root_session_key.is_empty() {
            &request.session_key
        } else {
            &request.root_session_key
        }
    }

    /// Calls waiting on an answer. A leak assertion, and a status field.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }

    /// Answers a parked request. Returns whether anything was waiting.
    ///
    /// A `false` is the normal two-tab race — the second answer arrives after
    /// the first has already released the call — and is not an error. The caller
    /// logs it and sends nothing back, because there is no client-visible
    /// difference between "you were second" and "it worked".
    pub fn resolve(&self, call_id: &str, approved: bool, scope: ApprovalScope) -> bool {
        let Some(pending) = self.pending.lock().remove(call_id) else {
            return false;
        };

        self.remember(
            HubApprovalGate::scope_of(&pending.request),
            &pending.request.agent_id,
            &pending.request.name,
            RememberedDecision { approved, scope },
        );
        tracing::info!(
            session_key = %pending.request.session_key,
            tool = %pending.request.name,
            approved,
            "approval answered"
        );
        let _ = pending.settle.send(ApprovalDecision {
            approved,
            scope: Some(scope),
            reason: None,
        });
        true
    }

    /// Forgets a session: its remembered answers, and any prompt still parked.
    ///
    /// Called when the hub drops a session, which is the moment nothing can
    /// answer for it any more. `always` survives — it was scoped to an agent,
    /// not to a session.
    ///
    /// Matched against the *scope* rather than the request's own session, so a
    /// prompt parked by a subagent of this conversation is settled too. It is
    /// the same reasoning as the memory above: the operator watching that prompt
    /// was watching this conversation, and closing it is what took the answer
    /// away.
    pub fn clear_session(&self, session_key: &str) {
        self.by_session.lock().remove(session_key);
        let settled: Vec<PendingApproval> = {
            let mut pending = self.pending.lock();
            let doomed: Vec<String> = pending
                .iter()
                .filter(|(_, entry)| HubApprovalGate::scope_of(&entry.request) == session_key)
                .map(|(call_id, _)| call_id.clone())
                .collect();
            doomed
                .into_iter()
                .filter_map(|call_id| pending.remove(&call_id))
                .collect()
        };
        for entry in settled {
            let _ = entry.settle.send(denial("the session was closed"));
        }
    }

    /// Forgets standing approvals for agents that are no longer configured.
    ///
    /// Not a cache detail — a permission one. An agent id is user-authored and
    /// re-creatable, so a deleted `reviewer` and a new one created under the
    /// same name are two different agents that happen to share a key. Without
    /// this, the new one silently inherits every tool the old one was ever
    /// granted standing permission for, and the operator who granted them was
    /// answering about an agent that no longer exists.
    ///
    /// Session-scoped answers are untouched: those belong to a conversation, and
    /// a conversation does not stop existing because an agent did.
    pub fn retain_agents(&self, agent_ids: &[String]) {
        self.always
            .lock()
            .retain(|agent_id, _| agent_ids.iter().any(|kept| kept == agent_id));
    }

    /// Carries standing approvals from one agent id to another.
    ///
    /// The other half of [`HubApprovalGate::retain_agents`], and the reason both
    /// are needed: a rename is the *same* agent, so its permissions follow it,
    /// where a delete-then-recreate is a different agent and its permissions
    /// must not.
    pub fn rename_agent(&self, from: &str, to: &str) {
        let mut always = self.always.lock();
        if let Some(decisions) = always.remove(from) {
            always.insert(to.to_owned(), decisions);
        }
    }

    /// The session's own answer wins over the standing one: it is the more
    /// specific of the two, and the more recently given. A session is bound to
    /// one agent, so the session-scoped map needs no agent dimension — and a
    /// subagent's turn carries its own `agent_id`, so the standing half stays
    /// per-agent even though the session half is the conversation's.
    fn recall(&self, session_key: &str, agent_id: &str, tool: &str) -> Option<RememberedDecision> {
        let session = self
            .by_session
            .lock()
            .get(session_key)
            .and_then(|tools| tools.get(tool))
            .copied();
        session.or_else(|| {
            self.always
                .lock()
                .get(agent_id)
                .and_then(|tools| tools.get(tool))
                .copied()
        })
    }

    fn remember(
        &self,
        session_key: &str,
        agent_id: &str,
        tool: &str,
        decision: RememberedDecision,
    ) {
        match decision.scope {
            ApprovalScope::Once => {}
            ApprovalScope::Always => {
                self.always
                    .lock()
                    .entry(agent_id.to_owned())
                    .or_default()
                    .insert(tool.to_owned(), decision);
            }
            ApprovalScope::Session => {
                self.by_session
                    .lock()
                    .entry(session_key.to_owned())
                    .or_default()
                    .insert(tool.to_owned(), decision);
            }
        }
    }

    /// Sends someone to find a human, when the socket cannot.
    ///
    /// A scheduled run is the case this exists for. Its session is its own —
    /// `automation:{job_id}:{run_id}` — and nothing is subscribed to it, so
    /// `tool.approvalRequest` goes out to an empty room and the turn waits the
    /// full approval timeout for a denial that was certain the moment it was
    /// raised. The same is true of a conversation whose tab was closed mid-turn.
    ///
    /// Deliberately *not* a change of policy. The request still parks, still
    /// times out, and still denies; this only makes it findable while it is
    /// open. An unattended run that pre-approved its own tools would be a much
    /// larger grant than any operator gave when they granted the tool.
    fn raise_if_unattended(&self, request: &ApprovalRequest) {
        let (Some(watchers), Some(sink)) = (self.watchers.as_ref(), self.on_unattended.as_ref())
        else {
            return;
        };
        if watchers(&request.session_key) > 0 {
            return;
        }

        tracing::info!(
            session_key = %request.session_key,
            tool = %request.name,
            call_id = %request.call_id,
            "approval request raised on a session nobody is watching"
        );
        sink(UnattendedApproval {
            session_key: request.session_key.clone(),
            agent_id: request.agent_id.clone(),
            tool_name: request.name.clone(),
            expires_at_ms: request.expires_at_ms,
        });
    }

    /// Parks one request and waits for whichever of the three answers arrives
    /// first.
    async fn park(&self, request: &ApprovalRequest) -> ApprovalDecision {
        if let Some(remembered) = self.recall(
            HubApprovalGate::scope_of(request),
            &request.agent_id,
            &request.name,
        ) {
            tracing::debug!(
                session_key = %request.session_key,
                tool = %request.name,
                "approval answered from memory"
            );
            return ApprovalDecision {
                approved: remembered.approved,
                scope: Some(remembered.scope),
                reason: None,
            };
        }

        // A turn already cancelled has nobody left to show a prompt to. Parking
        // one would rely on a cancellation that has already happened firing
        // again, which it never does.
        if request.token.is_cancelled() {
            return denial("the turn was cancelled");
        }

        let (tx, rx) = oneshot::channel();
        // A `call_id` is the model's, so a collision is not impossible. The
        // older prompt is the one nothing will answer — its turn has moved on.
        let superseded = self.pending.lock().insert(
            request.call_id.clone(),
            PendingApproval {
                request: request.clone(),
                settle: tx,
            },
        );
        if let Some(older) = superseded {
            let _ = older
                .settle
                .send(denial("superseded by another call with the same id"));
        }

        // After the prompt is parked, not before: the sink is what goes looking
        // for a person, and it must not be able to fire for a call that has
        // already been settled by the cancellation check above.
        self.raise_if_unattended(request);

        // A deadline already past fires immediately rather than never: the loop
        // hands this a wall-clock instant, and a clock that has moved is not a
        // reason to wait forever.
        let remaining = request
            .expires_at_ms
            .saturating_sub(u64::try_from(self.clock.now_ms()).unwrap_or(0));

        let decision = tokio::select! {
            // Biased, and the order is the decision: an answer that has
            // arrived beats a deadline that expired in the same instant,
            // and a cancellation beats a deadline that never mattered.
            biased;
            answered = rx => answered.unwrap_or_else(|_| denial("the approval request was dropped")),
            () = request.token.cancelled() => {
                self.forget(&request.call_id);
                denial("the turn was cancelled")
            }
            () = tokio::time::sleep(Duration::from_millis(remaining)) => {
                tracing::warn!(
                    session_key = %request.session_key,
                    tool = %request.name,
                    call_id = %request.call_id,
                    "approval request expired unanswered"
                );
                self.forget(&request.call_id);
                denial("the approval request expired")
            }
        };
        decision
    }

    /// Drops a parked entry without settling it — the caller already has the
    /// answer it is going to return.
    fn forget(&self, call_id: &str) {
        self.pending.lock().remove(call_id);
    }
}

impl ApprovalGate for HubApprovalGate {
    fn ask<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<ApprovalDecision>> {
        Box::pin(async move { Ok(self.park(request).await) })
    }
}

/// A refusal carrying the reason for the log, never for the model.
fn denial(reason: &str) -> ApprovalDecision {
    ApprovalDecision {
        approved: false,
        scope: None,
        reason: Some(reason.to_owned()),
    }
}
