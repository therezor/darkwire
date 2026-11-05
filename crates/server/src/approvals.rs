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
//! A parked call is filed under its conversation as well as its `call_id`. The
//! id is the model's, so two conversations can hold the same one, and an
//! answer only reaches a call in the conversation it was sent from.
//!
//! The decisions that are not obvious:
//!
//!  - **A remembered answer is keyed by what the tool says makes two calls the
//!    same.** For most tools that is the name, so approving one for the session
//!    approves the next call too, whatever its arguments. `exec` keys on the
//!    exact command and environment, because approving `cargo test` for the
//!    session must not approve `rm -rf target`.
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
//! A conversation is as far as a remembered answer reaches. "Never ask me about
//! this again" is a settings write: the tool's permission on the agent, or for
//! `exec` a command rule. The hub writes the rule when an answer carries one,
//! so it lands in the agent's settings, where it can be seen and removed.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use darkwire_agent::{ApprovalDecision, ApprovalGate, ApprovalRequest};
use darkwire_core::{Clock, ErrorKind, Result, SystemClock, WireError};
use darkwire_protocol::tools::ApprovalScope;
use darkwire_protocol::ws::PendingApproval;
use darkwire_providers::BoxFuture;
use parking_lot::Mutex;
use tokio::sync::oneshot;

/// What a client answered, kept past the call that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RememberedDecision {
    approved: bool,
    scope: ApprovalScope,
}

/// A conversation and a call id in it.
type PendingKey = (String, String);

/// A parked request and the channel that settles it.
struct Parked {
    /// Which `park` call filed it. A model can reuse a `call_id`, and the
    /// call that is superseded must not take its successor with it when it
    /// leaves.
    id: u64,
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

/// Told each time a prompt is parked or leaves, so every tab can say what is
/// waiting, including in sessions it is not showing. It reads the list with
/// [`HubApprovalGate::snapshot`].
pub type PendingWatcher = Arc<dyn Fn() + Send + Sync>;

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
    /// Parked senders, keyed by conversation and the tool call the client will
    /// name in its answer.
    pending: Mutex<HashMap<PendingKey, Parked>>,
    /// The next [`Parked::id`].
    next_id: AtomicU64,
    /// `session` scope: session key to memory key to decision.
    by_session: Mutex<HashMap<String, HashMap<String, RememberedDecision>>>,
    watchers: Option<WatcherCount>,
    on_unattended: Option<UnattendedSink>,
    /// Set by the hub once it exists, which is after this.
    watcher: OnceLock<PendingWatcher>,
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
            next_id: AtomicU64::new(0),
            by_session: Mutex::new(HashMap::new()),
            watchers: options.watchers,
            on_unattended: options.on_unattended,
            watcher: OnceLock::new(),
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

    /// Where a request is parked: its conversation, for the reason above.
    fn key_of(request: &ApprovalRequest) -> PendingKey {
        (
            HubApprovalGate::scope_of(request).to_owned(),
            request.call_id.clone(),
        )
    }

    /// Calls waiting on an answer. A leak assertion, and a status field.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }

    /// Every prompt still waiting, soonest to expire first.
    ///
    /// Filed under the conversation, so a subagent's prompt names the session
    /// somebody would open to answer it.
    pub fn snapshot(&self) -> Vec<PendingApproval> {
        let mut out: Vec<PendingApproval> = self
            .pending
            .lock()
            .iter()
            .map(|((scope, call_id), parked)| PendingApproval {
                session_key: scope.clone(),
                call_id: call_id.clone(),
                name: parked.request.name.clone(),
                expires_at_ms: parked.request.expires_at_ms,
            })
            .collect();
        out.sort_by(|a, b| {
            (a.expires_at_ms, &a.session_key, &a.call_id).cmp(&(
                b.expires_at_ms,
                &b.session_key,
                &b.call_id,
            ))
        });
        out
    }

    /// Who to tell when the list of waiting prompts changes. The first caller
    /// wins: one gate serves one hub.
    pub fn watch(&self, watcher: PendingWatcher) {
        let _ = self.watcher.set(watcher);
    }

    /// Tells the watcher the list moved. Never called with the map locked, so
    /// the watcher can read it.
    fn changed(&self) {
        if let Some(watcher) = self.watcher.get() {
            watcher();
        }
    }

    /// The request parked under `call_id` in the conversation `session_key`,
    /// if one still is.
    pub fn pending(&self, session_key: &str, call_id: &str) -> Option<ApprovalRequest> {
        self.pending
            .lock()
            .get(&(session_key.to_owned(), call_id.to_owned()))
            .map(|entry| entry.request.clone())
    }

    /// Answers a call parked in the conversation `session_key`. Returns whether
    /// anything was waiting.
    ///
    /// A `false` is the normal two-tab race (the second answer arrives after
    /// the first has already released the call), or an answer naming a call
    /// in another conversation. Neither is an error. The caller logs it and
    /// sends nothing back, because there is no client-visible difference
    /// between "you were second" and "it worked".
    pub fn resolve(
        &self,
        session_key: &str,
        call_id: &str,
        approved: bool,
        scope: ApprovalScope,
    ) -> bool {
        let Some(pending) = self
            .pending
            .lock()
            .remove(&(session_key.to_owned(), call_id.to_owned()))
        else {
            return false;
        };

        self.remember(
            HubApprovalGate::scope_of(&pending.request),
            &pending.request.memory_key,
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
        self.changed();
        true
    }

    /// Forgets a session: its remembered answers, and any prompt still parked.
    ///
    /// Called when the hub drops a session, which is the moment nothing can
    /// answer for it any more.
    ///
    /// Matched against the *scope* rather than the request's own session, so a
    /// prompt parked by a subagent of this conversation is settled too. It is
    /// the same reasoning as the memory above: the operator watching that prompt
    /// was watching this conversation, and closing it is what took the answer
    /// away.
    pub fn clear_session(&self, session_key: &str) {
        self.by_session.lock().remove(session_key);
        let settled: Vec<Parked> = {
            let mut pending = self.pending.lock();
            let doomed: Vec<PendingKey> = pending
                .keys()
                .filter(|(scope, _)| scope == session_key)
                .cloned()
                .collect();
            doomed
                .into_iter()
                .filter_map(|key| pending.remove(&key))
                .collect()
        };
        if settled.is_empty() {
            return;
        }
        for entry in settled {
            let _ = entry.settle.send(denial("the session was closed"));
        }
        self.changed();
    }

    /// A session is bound to one agent, so the memory needs no agent dimension:
    /// the conversation the answer was given in is the whole key.
    fn recall(&self, session_key: &str, key: &str) -> Option<RememberedDecision> {
        self.by_session
            .lock()
            .get(session_key)
            .and_then(|keys| keys.get(key))
            .copied()
    }

    fn remember(&self, session_key: &str, key: &str, decision: RememberedDecision) {
        match decision.scope {
            ApprovalScope::Once => {}
            ApprovalScope::Session => {
                self.by_session
                    .lock()
                    .entry(session_key.to_owned())
                    .or_default()
                    .insert(key.to_owned(), decision);
            }
        }
    }

    /// Sends someone to find a human, when the socket cannot.
    ///
    /// A scheduled run is the case this exists for. Its session is its own,
    /// minted for the run, and nothing is subscribed to it, so
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
        // The conversation, for the same reason as the memory: a subagent's
        // session has no watchers of its own, and the person who would answer
        // is looking at its parent.
        let session_key = HubApprovalGate::scope_of(request);
        if watchers(session_key) > 0 {
            return;
        }

        tracing::info!(
            session_key = %session_key,
            tool = %request.name,
            call_id = %request.call_id,
            "approval request raised on a session nobody is watching"
        );
        sink(UnattendedApproval {
            session_key: session_key.to_owned(),
            agent_id: request.agent_id.clone(),
            tool_name: request.name.clone(),
            expires_at_ms: request.expires_at_ms,
        });
    }

    /// Parks one request and waits for whichever of the three answers arrives
    /// first.
    ///
    /// An expiry is an error of kind `timeout`, not a refusal, so the loop can
    /// tell the model nobody answered rather than that somebody said no. This
    /// timer and the loop's count down to the same instant, and this one
    /// usually fires first.
    async fn park(&self, request: &ApprovalRequest) -> Result<ApprovalDecision> {
        if let Some(remembered) = ApprovalGate::remembered(self, request) {
            tracing::debug!(
                session_key = %request.session_key,
                tool = %request.name,
                "approval answered from memory"
            );
            return Ok(remembered);
        }

        // A turn already cancelled has nobody left to show a prompt to. Parking
        // one would rely on a cancellation that has already happened firing
        // again, which it never does.
        if request.token.is_cancelled() {
            return Ok(denial("the turn was cancelled"));
        }

        let (tx, rx) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // A `call_id` is the model's, so a collision within one conversation is
        // not impossible. The older prompt is the one nothing will answer: its
        // turn has moved on.
        let superseded = self.pending.lock().insert(
            HubApprovalGate::key_of(request),
            Parked {
                id,
                request: request.clone(),
                settle: tx,
            },
        );
        if let Some(older) = superseded {
            let _ = older
                .settle
                .send(denial("superseded by another call with the same id"));
        }
        // Every way out of here takes the entry with it, including the one
        // this function never sees: the loop races this against its own
        // deadline and drops the future when that wins.
        let _leaving = Leaving {
            gate: self,
            key: HubApprovalGate::key_of(request),
            id,
        };

        // After the prompt is parked, not before: the sink is what goes looking
        // for a person, and it must not be able to fire for a call that has
        // already been settled by the cancellation check above.
        self.raise_if_unattended(request);
        self.changed();

        // A deadline already past fires immediately rather than never: the loop
        // hands this a wall-clock instant, and a clock that has moved is not a
        // reason to wait forever.
        let remaining = request
            .expires_at_ms
            .saturating_sub(u64::try_from(self.clock.now_ms()).unwrap_or(0));

        tokio::select! {
            // Biased, and the order is the decision: an answer that has
            // arrived beats a deadline that expired in the same instant,
            // and a cancellation beats a deadline that never mattered.
            biased;
            answered = rx => Ok(answered.unwrap_or_else(|_| denial("the approval request was dropped"))),
            () = request.token.cancelled() => Ok(denial("the turn was cancelled")),
            () = tokio::time::sleep(Duration::from_millis(remaining)) => {
                tracing::warn!(
                    session_key = %request.session_key,
                    tool = %request.name,
                    call_id = %request.call_id,
                    "approval request expired unanswered"
                );
                Err(WireError::new(ErrorKind::Timeout, "the approval request expired"))
            }
        }
    }

    /// Drops a parked entry without settling it, if it is still the one
    /// `park` call `id` filed. The caller already has its answer, or has gone.
    fn forget(&self, key: &PendingKey, id: u64) {
        let removed = {
            let mut pending = self.pending.lock();
            match pending.get(key) {
                Some(parked) if parked.id == id => pending.remove(key).is_some(),
                _ => false,
            }
        };
        if removed {
            self.changed();
        }
    }
}

/// Takes a parked entry out of the map when `park` returns or is dropped.
struct Leaving<'a> {
    gate: &'a HubApprovalGate,
    key: PendingKey,
    id: u64,
}

impl Drop for Leaving<'_> {
    fn drop(&mut self) {
        self.gate.forget(&self.key, self.id);
    }
}

impl ApprovalGate for HubApprovalGate {
    fn ask<'a>(&'a self, request: &'a ApprovalRequest) -> BoxFuture<'a, Result<ApprovalDecision>> {
        Box::pin(self.park(request))
    }

    /// The same lookup `park` starts with, offered before the prompt is
    /// announced so a remembered answer never reaches a screen.
    fn remembered(&self, request: &ApprovalRequest) -> Option<ApprovalDecision> {
        self.recall(HubApprovalGate::scope_of(request), &request.memory_key)
            .map(|remembered| ApprovalDecision {
                approved: remembered.approved,
                scope: Some(remembered.scope),
                reason: None,
            })
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
