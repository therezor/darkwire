//! Background jobs: commands that outlive the `exec` call that started them.
//!
//! A job belongs to the session that started it. When it ends, its result is
//! posted into that session through [`SessionHub::post`]: it steers a running
//! turn, or wakes an idle session with a turn of its own.
//!
//! **In memory only.** A job is a child of this process, so it cannot outlive
//! a restart, and a table recording one would describe a process that is gone.
//! Every job's token is a child of one root token, cancelled when the server
//! closes, so there is still one cancellation mechanism.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};

use darkwire_core::clock::Clock;
use darkwire_core::history::truncate_head_tail;
use darkwire_protocol::automation::AUTOMATION_ORIGIN;
use darkwire_protocol::subagent::SUBAGENT_ORIGIN;
use darkwire_protocol::{Delivered, DeliveredKind, DeliveredStatus, RunningJob};
use darkwire_tools::background::{BackgroundPort, BackgroundRefusal, BackgroundResolver, StartJob};
use darkwire_tools::builtin::render_run;
use darkwire_tools::runner::{PlacementRequest, RunRequest};
use indexmap::IndexMap;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

use crate::hub::{Post, SessionHub};

/// The longest label the strip and the result header show.
const MAX_LABEL_CHARS: usize = 80;

/// Every session's running jobs. Cheap to clone.
#[derive(Clone)]
pub struct BackgroundJobs {
    inner: Arc<Inner>,
}

struct Inner {
    /// Cancelled when the server closes. Every job's token is a child of it.
    token: CancellationToken,
    clock: Arc<dyn Clock>,
    /// Where results go, and where a session's origin is read. Late, because
    /// the hub is built after the runtime that needs this registry for its
    /// tools.
    hub: OnceLock<Weak<SessionHub>>,
    boards: Mutex<HashMap<String, Board>>,
}

/// One session's jobs.
#[derive(Default)]
struct Board {
    /// The last id handed out. Ids are short so a small model copies them
    /// right.
    last: u64,
    running: IndexMap<String, Job>,
}

struct Job {
    label: String,
    started_at_ms: u64,
    token: CancellationToken,
}

/// Who a job reports to.
struct Owner {
    session_key: String,
    agent_id: String,
    workspace_id: String,
}

impl BackgroundJobs {
    /// An empty registry. It starts nothing until [`Self::attach`].
    pub fn new(clock: Arc<dyn Clock>) -> BackgroundJobs {
        BackgroundJobs {
            inner: Arc::new(Inner {
                token: CancellationToken::new(),
                clock,
                hub: OnceLock::new(),
                boards: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Where results are posted. Called once the hub exists.
    pub fn attach(&self, hub: &Arc<SessionHub>) {
        let _ = self.inner.hub.set(Arc::downgrade(hub));
    }

    /// The jobs a session is running, oldest first.
    pub fn running(&self, session_key: &str) -> Vec<RunningJob> {
        self.inner
            .boards
            .lock()
            .get(session_key)
            .map(|board| {
                board
                    .running
                    .iter()
                    .map(|(id, job)| RunningJob {
                        id: id.clone(),
                        label: job.label.clone(),
                        started_at_ms: job.started_at_ms,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Stops one job. `false` when the session runs no job with that id.
    pub fn stop(&self, session_key: &str, id: &str) -> bool {
        let boards = self.inner.boards.lock();
        let Some(job) = boards
            .get(session_key)
            .and_then(|board| board.running.get(id))
        else {
            return false;
        };
        job.token.cancel();
        true
    }

    /// Stops every job a session runs, for a session that is being deleted.
    pub fn stop_session(&self, session_key: &str) {
        if let Some(board) = self.inner.boards.lock().get(session_key) {
            for job in board.running.values() {
                job.token.cancel();
            }
        }
    }

    /// Stops every job without posting results, for a server that is
    /// closing.
    pub fn close(&self) {
        self.inner.token.cancel();
    }

    /// Whether a session may start jobs: not a subagent's or a scheduled
    /// run's, where a posted result would wake a turn nobody owns.
    fn allowed_in(&self, session_key: &str) -> bool {
        let Some(hub) = self.hub() else {
            return false;
        };
        match hub.store().get_session(session_key) {
            // No row yet is a conversation on its first turn, which only a
            // person or a channel starts.
            Ok(None) => true,
            Ok(Some(session)) => {
                session.origin != SUBAGENT_ORIGIN && session.origin != AUTOMATION_ORIGIN
            }
            Err(error) => {
                tracing::warn!(%error, session_key, "could not read a session's origin");
                false
            }
        }
    }

    fn start(&self, owner: Owner, job: StartJob) -> Result<String, BackgroundRefusal> {
        if job.limit == 0 {
            return Err(BackgroundRefusal::Off);
        }
        if !self.allowed_in(&owner.session_key) {
            return Err(BackgroundRefusal::NotHere);
        }
        let token = self.inner.token.child_token();
        let label = label_of(&job.argv);
        let id = {
            let mut boards = self.inner.boards.lock();
            let board = boards.entry(owner.session_key.clone()).or_default();
            if board.running.len() as u64 >= job.limit {
                return Err(BackgroundRefusal::AtCapacity);
            }
            board.last += 1;
            let id = board.last.to_string();
            board.running.insert(
                id.clone(),
                Job {
                    label: label.clone(),
                    started_at_ms: u64::try_from(self.inner.clock.now_ms()).unwrap_or(0),
                    token: token.clone(),
                },
            );
            id
        };
        let jobs = self.clone();
        let returned = id.clone();
        tokio::spawn(async move {
            jobs.announce(&owner.session_key).await;
            jobs.run(owner, id, label, job, token).await;
        });
        Ok(returned)
    }

    async fn run(
        &self,
        owner: Owner,
        id: String,
        label: String,
        job: StartJob,
        token: CancellationToken,
    ) {
        let outcome = job
            .runner
            .run(RunRequest {
                plan: job.plan.clone(),
                timeout_ms: job.timeout_ms,
                token: token.clone(),
                clock: Arc::clone(&job.clock),
                tee: None,
            })
            .await;
        self.forget(&owner.session_key, &id);
        if self.inner.token.is_cancelled() {
            return;
        }
        let (status, body) = match outcome {
            Ok(outcome) => {
                let rendered = render_run(&job.argv, &job.plan, &outcome);
                let status = if outcome.timed_out {
                    DeliveredStatus::Timeout
                } else if rendered.is_error {
                    DeliveredStatus::Error
                } else {
                    DeliveredStatus::Ok
                };
                let budget = usize::try_from(job.max_output_chars).unwrap_or(usize::MAX);
                (status, truncate_head_tail(&rendered.content, budget).text)
            }
            Err(_) if token.is_cancelled() => (DeliveredStatus::Killed, String::new()),
            Err(error) => (DeliveredStatus::Error, error.message),
        };
        let text = result_text(&id, &label, status, &body);
        self.announce(&owner.session_key).await;
        if let Some(hub) = self.hub() {
            hub.post(Post {
                session_key: owner.session_key,
                agent_id: Some(owner.agent_id),
                workspace_id: Some(owner.workspace_id),
                text,
                from: Delivered {
                    kind: DeliveredKind::Exec,
                    id,
                    label,
                    status,
                },
            })
            .await;
        }
    }

    fn forget(&self, session_key: &str, id: &str) {
        let mut boards = self.inner.boards.lock();
        if let Some(board) = boards.get_mut(session_key) {
            board.running.shift_remove(id);
            if board.running.is_empty() {
                boards.remove(session_key);
            }
        }
    }

    async fn announce(&self, session_key: &str) {
        if let Some(hub) = self.hub() {
            hub.jobs_changed(session_key).await;
        }
    }

    fn hub(&self) -> Option<Arc<SessionHub>> {
        self.inner.hub.get().and_then(Weak::upgrade)
    }
}

impl BackgroundResolver for BackgroundJobs {
    fn for_turn(&self, request: &PlacementRequest) -> Option<Arc<dyn BackgroundPort>> {
        Some(Arc::new(SessionPort {
            jobs: self.clone(),
            session_key: request.session_key.clone(),
            agent_id: request.agent_id.clone(),
            workspace_id: request.workspace_id.clone(),
        }))
    }
}

/// One turn's port, bound to its session.
struct SessionPort {
    jobs: BackgroundJobs,
    session_key: String,
    agent_id: String,
    workspace_id: String,
}

impl BackgroundPort for SessionPort {
    fn start(&self, job: StartJob) -> Result<String, BackgroundRefusal> {
        self.jobs.start(
            Owner {
                session_key: self.session_key.clone(),
                agent_id: self.agent_id.clone(),
                workspace_id: self.workspace_id.clone(),
            },
            job,
        )
    }

    fn kill(&self, id: &str) -> Result<(), BackgroundRefusal> {
        if self.jobs.stop(&self.session_key, id) {
            Ok(())
        } else {
            Err(BackgroundRefusal::Unknown)
        }
    }
}

/// The command as one line, shortened.
fn label_of(argv: &[String]) -> String {
    let joined = argv.join(" ");
    if joined.chars().count() <= MAX_LABEL_CHARS {
        return joined;
    }
    let mut short: String = joined.chars().take(MAX_LABEL_CHARS - 1).collect();
    short.push('…');
    short
}

/// What the model reads: a header naming the job, then its output.
fn result_text(id: &str, label: &str, status: DeliveredStatus, body: &str) -> String {
    let ended = match status {
        DeliveredStatus::Ok => "finished",
        DeliveredStatus::Error => "failed",
        DeliveredStatus::Killed => "was stopped",
        DeliveredStatus::Timeout => "timed out",
    };
    let header = format!("[Background job {id} {ended}: {label}]");
    if body.is_empty() {
        header
    } else {
        format!("{header}\n\n{body}")
    }
}
