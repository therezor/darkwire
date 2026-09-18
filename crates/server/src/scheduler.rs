//! The timer that runs jobs, and the lifecycle of one run.
//!
//! Here rather than in a crate of its own because it needs the shared database,
//! the notification store and the hub — all of which are at this level — and a
//! crate sitting beside `server` would have to depend on it back for the turn
//! runner and the broadcast. It follows the hub's discipline instead: every
//! collaborator arrives as a **structural port**, so the whole engine can be
//! driven with a hand-moved clock, a scripted connection and an in-memory
//! store, and never stands up a listener.
//!
//! Four decisions carry the file.
//!
//! **Turns go through the hub, not straight to a turn runner.** It would be
//! less code to call the loop and read its result, and it would be wrong. A
//! payload's `sessionKey` exists precisely so a job can accumulate context in
//! one session across runs, and nothing stops two jobs — or a browser tab —
//! naming the same one. Two runs writing into a single history is the failure
//! the hub exists to prevent: a transcript no model can read and no user can
//! explain. A per-job in-flight set does not prevent it, because the collision
//! is *between* jobs. The hub is the only thing in the process that serialises
//! a session, and the caller most likely to collide is the last one that should
//! be routed around it. The cost is reconstructing the answer from the event
//! stream, which is [`TurnCollector`] below.
//!
//! **The timer is one rearming wait to the earliest due job**, not a tick. A
//! thirty-minute heartbeat should not cost a wakeup a second, and a tick coarse
//! enough to be cheap makes a one-shot late. Every hop is clamped to
//! [`MAX_ARM_MS`], which also bounds how far the wall clock can drift — or be
//! stepped by NTP, or by a laptop resuming — before the engine re-reads what is
//! actually due.
//!
//! **A job never queues a second occurrence of itself.** The next run is
//! computed at *completion*, not at the scheduled instant, so a ten-minute run
//! on a five-minute interval produces a run every ten minutes rather than a
//! backlog that never drains. A provisional next time is written at dispatch so
//! that a hard kill leaves the job scheduled rather than unscheduled forever.
//!
//! **Every run writes a row before it does anything.** The row is the only
//! durable trace of a turn nobody watched, and a run that dies mid-flight has
//! to leave evidence rather than a gap — the pending reconciliation closes
//! those out at the next boot.
//!
//! **What a test drives, and what a process drives.** Every decision this
//! engine makes reads the injected [`Clock`], so [`Scheduler::tick`] is the
//! whole engine in one call and a test moves time by hand. Only the *waiting*
//! between ticks uses a real timer, in [`Scheduler::run`], which is the one
//! part a test has no reason to exercise.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use darkwire_core::clock::Clock;
use darkwire_core::cron::{next_cron_run, parse_cron};
use darkwire_core::errors::{ErrorKind, Result, WireError};
use darkwire_core::ids::DEFAULT_WORKSPACE_ID;
use darkwire_core::session_store::IdSource;
use darkwire_protocol::automation::{
    AUTOMATION_ORIGIN, AutomationJob, AutomationPayload, AutomationRun, AutomationSchedule,
    HeartbeatPayload, RunStatus,
};
use darkwire_protocol::config::Config;
use darkwire_protocol::messages::StopReason;
use darkwire_protocol::rest::Notification;
use darkwire_protocol::ws::{
    ClientMessage, NotificationLevel, ServerMessage, StopTurnMessage, StopTurnTag,
    UserMessageRequest, UserMessageTag,
};
use darkwire_providers::{ChatResult, ToolChoice};
use parking_lot::Mutex;
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

use crate::automation_store::{AutomationStore, FinishRunInput, RunOutcome, TrimmedRun};
use crate::heartbeat::{
    DecideMessagesInput, EvaluateMessagesInput, HEARTBEAT_RESULT_TOOL, HEARTBEAT_TOOL,
    HeartbeatAction, MAX_TASK_FILE_BYTES, build_decide_messages, build_evaluate_messages,
    read_decision, read_evaluation,
};
use crate::notifications::CreateNotificationInput;

/// The longest a single timer hop may be.
///
/// Not a tuning knob. Twenty-four hours bounds how far the wall clock can drift
/// — or be stepped by NTP, or by a laptop resuming — before the engine re-reads
/// what is actually due, so a job scheduled for February cannot be reached by a
/// single wait that a clock correction invalidates halfway through.
pub const MAX_ARM_MS: i64 = 24 * 60 * 60 * 1000;

/// How long a single run may take before it is abandoned.
pub const DEFAULT_RUN_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// The floor on a rearm that could otherwise be zero.
///
/// Work stays *due* while the concurrency limit is saturated, so the delay to
/// "the earliest due job" is zero for as long as the slots are full. Without a
/// floor the timer would re-arm at 0 ms and fire again immediately, spinning
/// for the entire length of a slow run — a hot loop that does nothing but
/// re-read the same rows. A freed slot wakes the waiter directly, so this is
/// the backstop rather than the mechanism.
pub const BUSY_RETRY_MS: i64 = 1000;

/// What a dead process left behind, read at the next boot.
pub const INTERRUPTED_BY_RESTART: &str = "Interrupted by a restart.";

/// What a run that was cancelled on the way down records.
pub const INTERRUPTED_BY_SHUTDOWN: &str = "Interrupted by shutdown.";

/// What a job that asks for delivery on an install with no channel records.
pub const NO_CHANNEL_WARNING: &str = "This job asks for delivery, but no channel is wired yet. The result was recorded in the notification centre instead.";

// Ports

/// What the scheduler needs of a hub connection.
pub trait SchedulerConnection: Send + Sync {
    /// Pushes one client frame into the hub as this connection.
    fn receive(&self, frame: ClientMessage);
    /// Detaches the connection.
    fn close(&self);
}

/// Where a connection's events go.
pub type EventSink = Arc<dyn Fn(ServerMessage) + Send + Sync>;

/// How the scheduler asks the hub for a connection.
pub struct SchedulerConnectOptions {
    /// Where this connection's events go.
    pub send: EventSink,
    /// The conversation the run drives.
    pub session_key: String,
    /// Always the automation origin here.
    pub channel: String,
    /// The agent to run as, when the job names one.
    pub agent_id: Option<String>,
    /// The workspace a session this run *creates* lands in.
    ///
    /// Carries the hub's rule: a run pinned to a session that already exists
    /// leaves that session's workspace alone.
    pub workspace_id: Option<String>,
    /// Always true here: nobody is on the other end of a scheduled run.
    pub unattended: bool,
}

/// The notification frame, minus the `seq` the hub stamps per session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationBroadcast {
    /// The row id.
    pub id: String,
    /// The headline.
    pub title: String,
    /// The detail.
    pub body: String,
    /// How loud.
    pub level: NotificationLevel,
    /// When it was raised.
    pub created_at_ms: u64,
    /// The conversation it is about, if any.
    pub session_key: Option<String>,
    /// The job that raised it.
    pub job_id: Option<String>,
}

/// The live settings tree. A function, never a snapshot: a settings save must
/// move the next drain.
pub type ConfigSource = Arc<dyn Fn() -> Config + Send + Sync>;

/// Opens one hub connection for a run.
pub type ConnectFn =
    Arc<dyn Fn(SchedulerConnectOptions) -> Arc<dyn SchedulerConnection> + Send + Sync>;

/// Pushes a notification frame at every connected client.
pub type BroadcastFn = Arc<dyn Fn(NotificationBroadcast) + Send + Sync>;

/// Writes a notification row.
pub type RaiseFn = Arc<dyn Fn(CreateNotificationInput) -> Result<Notification> + Send + Sync>;

/// Deletes a session a trimmed run had been the only reference to.
pub type DeleteSessionFn = Arc<dyn Fn(&str) + Send + Sync>;

/// One direct provider request. Absent means this build has no heartbeat.
pub type ChatFn = Arc<
    dyn Fn(DirectChat) -> futures::future::BoxFuture<'static, Result<ChatResult>> + Send + Sync,
>;

/// What a heartbeat's decision or evaluation asks the provider.
pub struct DirectChat {
    /// The agent whose connection to use.
    pub agent_id: Option<String>,
    /// Overrides the agent's own model — how a cheap heartbeat model is chosen.
    pub model: Option<String>,
    /// The whole conversation for this one request.
    pub messages: Vec<darkwire_protocol::messages::ChatMessage>,
    /// The one tool this request offers.
    pub tools: Vec<darkwire_protocol::tools::ToolDefinition>,
    /// How hard to push the model towards calling it.
    pub tool_choice: ToolChoice,
    /// A ceiling on the answer.
    pub max_tokens: Option<u32>,
    /// Cancels the request.
    pub token: CancellationToken,
}

/// Reads one workspace-relative file. Absent means no heartbeat.
pub type ReadFileFn =
    Arc<dyn Fn(ReadTaskFile) -> futures::future::BoxFuture<'static, Result<String>> + Send + Sync>;

/// Which file a heartbeat reads.
///
/// A struct rather than positional arguments on purpose: the path and the
/// workspace are both strings, so a third parameter would make a transposition
/// at a call site something the compiler cannot see.
pub struct ReadTaskFile {
    /// The workspace the path is relative to.
    pub workspace_id: String,
    /// The workspace-relative path.
    pub path: String,
    /// The cap, past which the file is truncated before the model reads it.
    pub max_bytes: usize,
}

/// Everything the engine is built from.
pub struct SchedulerOptions {
    /// The job and run store.
    pub jobs: Arc<AutomationStore>,
    /// The live settings tree.
    pub config: ConfigSource,
    /// Opens a hub connection for a run.
    pub connect: ConnectFn,
    /// Pushes a notification frame at connected clients.
    pub broadcast: BroadcastFn,
    /// Writes a notification row.
    pub raise: RaiseFn,
    /// Deletes a session a trimmed run had been the only reference to.
    pub delete_session: Option<DeleteSessionFn>,
    /// One direct provider request, for the heartbeat's two decisions.
    pub chat: Option<ChatFn>,
    /// Reads a heartbeat's task file.
    pub read_file: Option<ReadFileFn>,
    /// The injected clock every decision reads.
    pub clock: Arc<dyn Clock>,
    /// Mints run and session ids.
    pub new_id: IdSource,
    /// How long a single run may take. `None` is [`DEFAULT_RUN_TIMEOUT_MS`].
    pub run_timeout_ms: Option<u64>,
}

/// The narrow view the REST routes hold.
pub trait SchedulerPort: Send + Sync {
    /// Runs a job now, out of band, and returns its `pending` row.
    fn run_now(&self, job_id: &str) -> Result<AutomationRun>;
    /// Re-reads what is due. Called after any create, update, delete or save.
    fn refresh(&self);
    /// Whether anything fires at all.
    fn enabled(&self) -> bool;
}

// Schedule arithmetic

/// When a schedule next fires after an instant, or 0 for never again.
///
/// 0 rather than `None` because that is what the column means —
/// `next_run_at_ms = 0` is "unscheduled", which covers a fired one-shot, a
/// disabled job and a cron expression like `0 0 30 2 *` that is legal to write
/// and impossible to reach. All three are the same to the timer.
pub fn next_run_after(schedule: &AutomationSchedule, from_ms: i64, tz: &str) -> Result<i64> {
    Ok(match schedule {
        AutomationSchedule::At(at) => {
            let at_ms = i64::try_from(at.at_ms).unwrap_or(i64::MAX);
            if at_ms > from_ms { at_ms } else { 0 }
        }
        AutomationSchedule::Every(every) => {
            from_ms.saturating_add(i64::try_from(every.every_ms).unwrap_or(i64::MAX))
        }
        AutomationSchedule::Cron(cron) => {
            // The install's `ui.timezone`, and only it — a job has no zone of
            // its own any more. The same setting renders the next-run line, so
            // the expression is read against the clock the answer is printed
            // against. Defaulting to the *host* zone instead would make one
            // expression fire at a different instant after the server moved.
            let spec = parse_cron(&cron.expr, Some(tz))?;
            next_cron_run(&spec, from_ms).unwrap_or(0)
        }
    })
}

/// The first fire time for a job as created or edited.
///
/// An `at` job whose instant is already past keeps it rather than being pushed
/// forward: the boot sweep is what decides whether a missed one-shot runs, and
/// silently moving it here would take that decision away.
pub fn first_run_at(
    schedule: &AutomationSchedule,
    now_ms: i64,
    enabled: bool,
    tz: &str,
) -> Result<i64> {
    if !enabled {
        return Ok(0);
    }
    if let AutomationSchedule::At(at) = schedule {
        return Ok(i64::try_from(at.at_ms).unwrap_or(i64::MAX));
    }
    next_run_after(schedule, now_ms, tz)
}

// Collecting a turn's answer from the event stream

/// What one turn produced, as reconstructed from its frames.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnOutcome {
    /// The answer text, trimmed.
    pub text: String,
    /// Why it failed, when it did.
    pub error: Option<String>,
    /// Things worth saying about a run that did not fail.
    pub warnings: Vec<String>,
}

struct CollectorInner {
    turn_id: Option<String>,
    text: String,
    error: Option<String>,
    warnings: Vec<String>,
    settle: Option<oneshot::Sender<TurnOutcome>>,
    done: bool,
}

/// Accumulates one turn's frames into an answer.
///
/// The hub is a broadcast surface, not a request/response one, so this watches
/// for the turn end matching the turn start it saw and settles there.
pub struct TurnCollector {
    inner: Mutex<CollectorInner>,
}

impl TurnCollector {
    /// A collector and the receiver its outcome arrives on.
    pub fn new() -> (Arc<TurnCollector>, oneshot::Receiver<TurnOutcome>) {
        let (tx, rx) = oneshot::channel();
        let collector = Arc::new(TurnCollector {
            inner: Mutex::new(CollectorInner {
                turn_id: None,
                text: String::new(),
                error: None,
                warnings: Vec::new(),
                settle: Some(tx),
                done: false,
            }),
        });
        (collector, rx)
    }

    /// Folds one frame in.
    ///
    /// The match is exhaustive rather than swept up by a wildcard, and that is
    /// the point: a protocol event added without a decision here would silently
    /// do nothing in every scheduled run, and nothing anywhere would say so.
    pub fn receive(&self, message: &ServerMessage) {
        let mut finish_now = false;
        {
            let mut inner = self.inner.lock();
            if inner.done {
                return;
            }
            match message {
                ServerMessage::TurnStart(event) => {
                    if inner.turn_id.is_none() {
                        inner.turn_id = Some(event.event.turn_id.clone());
                    }
                }
                ServerMessage::AssistantDelta(event) => inner.text.push_str(&event.event.text),
                ServerMessage::Notice(event) => inner.warnings.push(event.event.message.clone()),
                ServerMessage::Error(event) => {
                    // A hub-level refusal — no model configured, the session is
                    // busy. It arrives unsequenced and no turn end follows it,
                    // so this is the only place the run learns it will never
                    // start.
                    inner.error = Some(event.message.clone());
                    finish_now = true;
                }
                ServerMessage::TurnEnd(event) => {
                    if inner
                        .turn_id
                        .as_ref()
                        .is_some_and(|seen| *seen != event.event.turn_id)
                    {
                        return;
                    }
                    match event.event.stop_reason {
                        // The turn did work and produced an answer, it just ran
                        // out of tool budget saying so. Recording it as an error
                        // would notify the operator that a job broke when what
                        // actually happened is that it was busy.
                        StopReason::MaxIterations => inner.warnings.push(
                            "The turn hit its tool-iteration cap; the answer may be incomplete."
                                .to_owned(),
                        ),
                        StopReason::Complete => {}
                        other => {
                            inner.error = Some(format!(
                                "The turn ended early ({}).",
                                stop_reason_str(other)
                            ));
                        }
                    }
                    finish_now = true;
                }

                // Everything a job's transcript has no use for: the streams a
                // person watches, the connection bookkeeping a browser
                // reconciles against, and the tool traffic — a run records what
                // the turn *said*, and the calls it made along the way are in
                // the session either way.
                ServerMessage::Connected(_)
                | ServerMessage::Pong(_)
                | ServerMessage::MessageAck(_)
                | ServerMessage::MessageQueued(_)
                | ServerMessage::ReasoningDelta(_)
                | ServerMessage::ToolCall(_)
                | ServerMessage::ToolProgress(_)
                | ServerMessage::ToolResult(_)
                | ServerMessage::ToolApprovalRequest(_)
                | ServerMessage::Subagent(_)
                | ServerMessage::ContextUsage(_)
                | ServerMessage::SessionStatus(_)
                | ServerMessage::SessionReset(_)
                | ServerMessage::SessionReplay(_)
                | ServerMessage::SessionTruncated(_)
                | ServerMessage::Notification(_)
                | ServerMessage::ToolsChanged(_)
                | ServerMessage::Steer(_) => {}
            }
        }
        if finish_now {
            self.finish(None);
        }
    }

    /// Settles with whatever has arrived. Idempotent.
    pub fn finish(&self, error: Option<String>) {
        let mut inner = self.inner.lock();
        if inner.done {
            return;
        }
        inner.done = true;
        if error.is_some() {
            inner.error = error;
        }
        let outcome = TurnOutcome {
            text: inner.text.trim().to_owned(),
            error: inner.error.clone(),
            warnings: inner.warnings.clone(),
        };
        if let Some(settle) = inner.settle.take() {
            let _ = settle.send(outcome);
        }
    }
}

/// The wire spelling of a stop reason, for the sentence a failed run records.
fn stop_reason_str(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Complete => "complete",
        StopReason::MaxIterations => "max_iterations",
        StopReason::Aborted => "aborted",
        StopReason::WallTimeout => "wall_timeout",
        StopReason::Error => "error",
    }
}

// The engine

#[derive(Default)]
struct State {
    started: bool,
    stopping: bool,
    /// The zone the stored next-run values were computed against.
    ///
    /// `None` until the engine starts, which is what makes the first refresh
    /// after a boot a no-op rather than a full rescan of jobs whose next run
    /// the boot sweep just settled.
    zoned_at: Option<String>,
    /// Job ids with a run in flight. What stops a job overlapping itself.
    in_flight: HashMap<String, CancellationToken>,
}

struct Inner {
    jobs: Arc<AutomationStore>,
    config: ConfigSource,
    connect: ConnectFn,
    broadcast: BroadcastFn,
    raise: RaiseFn,
    delete_session: Option<DeleteSessionFn>,
    chat: Option<ChatFn>,
    read_file: Option<ReadFileFn>,
    clock: Arc<dyn Clock>,
    new_id: IdSource,
    run_timeout: Duration,
    state: Mutex<State>,
    /// Woken by a refresh, by a finished run, and by a stop.
    wake: Notify,
}

/// The engine. Cheap to clone; every clone drives the same state.
#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Scheduler")
    }
}

impl Scheduler {
    /// Builds the engine. Nothing runs until [`Scheduler::start`].
    pub fn new(options: SchedulerOptions) -> Scheduler {
        Scheduler {
            inner: Arc::new(Inner {
                jobs: options.jobs,
                config: options.config,
                connect: options.connect,
                broadcast: options.broadcast,
                raise: options.raise,
                delete_session: options.delete_session,
                chat: options.chat,
                read_file: options.read_file,
                clock: options.clock,
                new_id: options.new_id,
                run_timeout: Duration::from_millis(
                    options.run_timeout_ms.unwrap_or(DEFAULT_RUN_TIMEOUT_MS),
                ),
                state: Mutex::new(State::default()),
                wake: Notify::new(),
            }),
        }
    }

    /// The settings tree, read live.
    fn config(&self) -> Config {
        (self.inner.config)()
    }

    /// Read live, so changing it in Appearance moves the next rearm.
    ///
    /// `ui.timezone` rather than a scheduler setting of its own: this is the
    /// same zone every timestamp is rendered in, which is what makes
    /// `0 9 * * *` mean nine on the clock the operator is reading. A settings
    /// save calls [`Scheduler::refresh`], so an existing cron job is
    /// rescheduled on the save rather than on the next fire.
    fn timezone(&self) -> String {
        self.config().ui.timezone
    }

    /// Reconciles what a dead process left and applies boot catch-up.
    ///
    /// Nothing runs before this, and a disabled scheduler stops here — the
    /// store and the REST routes still work, so an operator can author jobs
    /// before switching it on.
    pub fn start(&self) -> Result<()> {
        {
            let mut state = self.inner.state.lock();
            if state.started {
                return Ok(());
            }
            state.started = true;
            state.stopping = false;
        }
        // The zone the stored next-runs are already valid against, so the first
        // refresh after boot has nothing to recompute.
        let tz = self.timezone();
        self.inner.state.lock().zoned_at = Some(tz);

        let closed = self.inner.jobs.reconcile_pending(INTERRUPTED_BY_RESTART)?;
        if closed > 0 {
            tracing::warn!(
                runs = closed,
                "closed out automation runs left by a previous process"
            );
        }

        if !self.enabled() {
            tracing::info!("scheduler is disabled in settings; no jobs will run");
            return Ok(());
        }

        self.catch_up()?;
        self.inner.wake.notify_one();
        Ok(())
    }

    /// Stops the engine and cancels what is running.
    ///
    /// Must run **before** the hub closes: every in-flight run is driving a
    /// turn through it, and pulling the hub first would leave those turns
    /// writing into a store whose connection is about to go.
    pub fn stop(&self) {
        let tokens: Vec<CancellationToken> = {
            let mut state = self.inner.state.lock();
            state.stopping = true;
            state.started = false;
            state.in_flight.values().cloned().collect()
        };
        for token in tokens {
            token.cancel();
        }
        self.inner.wake.notify_waiters();
    }

    /// How many runs are in flight. What a caller waits to reach zero.
    pub fn in_flight(&self) -> usize {
        self.inner.state.lock().in_flight.len()
    }

    /// The wait loop: sleep to the next due instant, then [`Scheduler::tick`].
    ///
    /// The one part of the engine that touches a real timer. Everything it
    /// decides comes from [`Scheduler::tick`], which reads the injected clock,
    /// so a test drives the engine by moving that clock and calling `tick`
    /// rather than by waiting.
    pub async fn run(&self) {
        loop {
            if self.inner.state.lock().stopping {
                return;
            }
            // Nothing scheduled: an idle install should not be waking up to
            // discover that repeatedly, so it waits to be told.
            let Some(delay) = self.next_delay_ms() else {
                self.inner.wake.notified().await;
                continue;
            };
            if delay > 0 {
                let millis = u64::try_from(delay).unwrap_or(0);
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(millis)) => {}
                    () = self.inner.wake.notified() => continue,
                }
            }
            if let Err(error) = self.tick() {
                tracing::error!(err = %error.message, "automation drain failed");
            }
        }
    }

    /// How long until the next wake, or `None` when nothing is scheduled.
    ///
    /// Clamped to [`MAX_ARM_MS`] and floored at zero.
    pub fn next_delay_ms(&self) -> Option<i64> {
        if self.inner.state.lock().stopping || !self.enabled() {
            return None;
        }
        let at = self.inner.jobs.earliest_due_ms().ok().flatten()?;
        Some(
            at.saturating_sub(self.inner.clock.now_ms())
                .clamp(0, MAX_ARM_MS),
        )
    }

    /// Dispatches everything due, up to the concurrency limit.
    ///
    /// The whole engine in one call, and the seam a test drives: move the
    /// clock, call this, assert on the rows.
    ///
    /// Concurrency is read live on every tick, so raising it in Settings takes
    /// effect on the next wake rather than the next restart.
    pub fn tick(&self) -> Result<()> {
        if self.inner.state.lock().stopping || !self.enabled() {
            return Ok(());
        }

        let config = self.config();
        let limit = i64::try_from(config.scheduler.concurrency).unwrap_or(i64::MAX)
            - i64::try_from(self.in_flight()).unwrap_or(0);
        if limit <= 0 {
            return Ok(());
        }

        for job in self.inner.jobs.due_jobs(self.inner.clock.now_ms(), limit)? {
            // A job already running is left due rather than started twice. Its
            // own completion recomputes the next time and wakes the waiter.
            if self.inner.state.lock().in_flight.contains_key(&job.id) {
                continue;
            }
            self.dispatch(&job, Vec::new())?;
        }
        Ok(())
    }

    /// Re-reads what is due. Called after any create, update, delete or save.
    pub fn refresh_now(&self) -> Result<()> {
        if !self.inner.state.lock().started {
            return Ok(());
        }
        self.rezone()?;
        self.inner.wake.notify_one();
        Ok(())
    }

    /// Recomputes cron next-runs when the install's zone changed under them.
    ///
    /// A cron expression is a wall-clock time, so its stored instant is only
    /// valid against the zone it was computed in. Changing `ui.timezone` is
    /// therefore a reschedule, and doing it on the refresh a settings save
    /// already calls is what makes the panel's answer true immediately rather
    /// than after each job happens to fire once more.
    ///
    /// **Cron only.** An interval job has no wall clock in it, and recomputing
    /// it would push its next run a full interval into the future on every
    /// unrelated save. A one-shot is an instant that was already resolved.
    /// Neither moves when the zone does.
    ///
    /// Guarded on an actual change rather than run unconditionally: a refresh
    /// follows every create, update and delete, and a rescan of every job on
    /// each of those is work with no answer to give.
    fn rezone(&self) -> Result<()> {
        let tz = self.timezone();
        {
            let mut state = self.inner.state.lock();
            if state.zoned_at.as_deref() == Some(tz.as_str()) {
                return Ok(());
            }
            state.zoned_at = Some(tz.clone());
        }

        let now = self.inner.clock.now_ms();
        let mut moved = 0_u64;
        for job in self.inner.jobs.list_jobs()? {
            if !matches!(job.schedule, AutomationSchedule::Cron(_)) || !job.enabled {
                continue;
            }
            let next = next_run_after(&job.schedule, now, &tz)?;
            if u64::try_from(next).unwrap_or(0) == job.state.next_run_at_ms {
                continue;
            }
            self.inner.jobs.set_next_run(&job.id, next)?;
            moved += 1;
        }
        if moved > 0 {
            tracing::info!(
                timezone = tz,
                jobs = moved,
                "timezone changed; rescheduled cron jobs against it"
            );
        }
        Ok(())
    }

    /// Boot catch-up.
    ///
    /// Coalesces rather than replaying: a job that missed twelve occurrences
    /// over a weekend runs **once**, because the point of a scheduled job is
    /// that the work happens, not that it happens twelve times at 9am on
    /// Monday. A missed one-shot is the case the flag was written for, and
    /// turning it off records the miss rather than hiding it — a reminder that
    /// silently vanished is worse than one that says it was missed.
    fn catch_up(&self) -> Result<()> {
        let now = self.inner.clock.now_ms();
        let missed = self.inner.jobs.missed_jobs(now)?;
        if missed.is_empty() {
            return Ok(());
        }

        let catch_up = self.config().scheduler.catch_up_on_boot;
        let tz = self.timezone();

        for job in missed {
            if catch_up {
                let scheduled = job.state.next_run_at_ms;
                self.dispatch(
                    &job,
                    vec![format!(
                        "This run was started at boot: its scheduled time ({scheduled}) passed while the server was down."
                    )],
                )?;
                continue;
            }

            if matches!(job.schedule, AutomationSchedule::At(_)) {
                // Recorded, not deleted: a one-shot that vanished without trace
                // is a reminder the operator never learns was missed. The
                // self-destruct flag is deliberately not honoured here, because
                // it did not run.
                let run = self.inner.jobs.start_run(&job.id, None)?;
                self.inner.jobs.finish_run(
                    &run.id,
                    &FinishRunInput {
                        status: RunStatus::Skipped,
                        skip_reason: Some("Missed while the server was down.".to_owned()),
                        ..FinishRunInput::default()
                    },
                )?;
                self.inner.jobs.record_outcome(
                    &job.id,
                    &RunOutcome {
                        ran_at_ms: now,
                        status: RunStatus::Skipped,
                        error: None,
                    },
                )?;
                self.inner.jobs.set_next_run(&job.id, 0)?;
                continue;
            }

            let next = next_run_after(&job.schedule, now, &tz)?;
            self.inner.jobs.set_next_run(&job.id, next)?;
        }
        Ok(())
    }

    // One run

    /// Starts a run and returns its `pending` row.
    ///
    /// The provisional next time is written **here**, before anything can fail,
    /// so a hard kill leaves the job scheduled at roughly the right moment
    /// rather than unscheduled forever. Completion corrects it.
    fn dispatch(&self, job: &AutomationJob, warnings: Vec<String>) -> Result<AutomationRun> {
        let now = self.inner.clock.now_ms();
        let run = self.inner.jobs.start_run(
            &job.id,
            // A plain id. The job it belongs to is on this very row and the
            // origin is on the session, so a key that spelled either out was
            // long without being more useful — nothing ever parsed it back.
            Some(
                payload_session_key(&job.payload)
                    .cloned()
                    .unwrap_or_else(|| (self.inner.new_id)()),
            ),
        )?;

        // A one-shot is unscheduled the instant it is dispatched. Anything else
        // gets a provisional time that completion will move.
        //
        // A *disabled* job stays unscheduled, and that is the on-demand run's
        // doing: it is the one caller that reaches a job the timer would never
        // pick up. Writing a next-run time here left a row badged Disabled with
        // a "Next run" beside it — a contradiction an operator has to work out
        // for themselves, and a stale time the job would inherit whenever it
        // was next switched on.
        self.inner
            .jobs
            .set_next_run(&job.id, self.next_run_for(job, now)?)?;

        let token = CancellationToken::new();
        self.inner
            .state
            .lock()
            .in_flight
            .insert(job.id.clone(), token.clone());

        let engine = self.clone();
        let job = job.clone();
        let run_row = run.clone();
        tokio::spawn(async move {
            if let Err(error) = engine.execute(&job, &run_row, &token, warnings).await {
                tracing::error!(
                    job_id = job.id,
                    err = %error.message,
                    "automation run failed unexpectedly"
                );
            }
            engine.inner.state.lock().in_flight.remove(&job.id);
            engine.inner.wake.notify_one();
        });

        Ok(run)
    }

    /// The whole of one run: decide, turn, evaluate, record, settle.
    async fn execute(
        &self,
        job: &AutomationJob,
        run: &AutomationRun,
        token: &CancellationToken,
        seeded: Vec<String>,
    ) -> Result<()> {
        let mut warnings = seeded;
        if payload_delivers(&job.payload) {
            warnings.push(NO_CHANNEL_WARNING.to_owned());
        }

        let mut status = RunStatus::Ok;
        let mut output = String::new();
        let mut error: Option<String> = None;
        let mut skip_reason: Option<String> = None;
        let mut notify: Option<(String, String)> = None;

        let attempt = self
            .attempt(
                job,
                run,
                token,
                &mut warnings,
                &mut output,
                &mut skip_reason,
            )
            .await;
        match attempt {
            Ok(Some(headline)) => notify = Some(headline),
            Ok(None) => {
                if skip_reason.is_some() {
                    status = RunStatus::Skipped;
                }
            }
            Err(failure) => {
                status = RunStatus::Error;
                error = Some(if token.is_cancelled() {
                    INTERRUPTED_BY_SHUTDOWN.to_owned()
                } else {
                    failure.message
                });
            }
        }

        self.inner.jobs.finish_run(
            &run.id,
            &FinishRunInput {
                status,
                output: if output.is_empty() {
                    None
                } else {
                    Some(output)
                },
                error: error.clone(),
                skip_reason,
                warnings,
            },
        )?;
        self.inner.jobs.record_outcome(
            &job.id,
            &RunOutcome {
                ran_at_ms: i64::try_from(run.started_at_ms).unwrap_or(0),
                status,
                error: error.clone(),
            },
        )?;

        self.settle(job, status, error.as_deref(), notify)
    }

    /// The body of a run, up to the point where an outcome is known.
    ///
    /// `Ok(Some(..))` is a headline worth notifying about, `Ok(None)` is a
    /// skip or a silent success, `Err` is a failure.
    async fn attempt(
        &self,
        job: &AutomationJob,
        run: &AutomationRun,
        token: &CancellationToken,
        warnings: &mut Vec<String>,
        output: &mut String,
        skip_reason: &mut Option<String>,
    ) -> Result<Option<(String, String)>> {
        let instruction = match &job.payload {
            AutomationPayload::Heartbeat(payload) => {
                let decided = self.decide(job, payload, token).await?;
                warnings.extend(decided.warnings.iter().cloned());
                if decided.action != HeartbeatAction::Run {
                    *skip_reason = Some(decided.reason);
                    return Ok(None);
                }
                decided.instruction
            }
            AutomationPayload::Scheduled(payload) => payload.message.clone(),
        };

        let turn = self.run_turn(job, run, &instruction, token).await?;
        warnings.extend(turn.warnings.iter().cloned());
        output.push_str(&turn.text);
        if let Some(failure) = turn.error {
            return Err(WireError::new(ErrorKind::Tool, failure));
        }

        if matches!(job.payload, AutomationPayload::Heartbeat(_)) {
            let verdict = self.evaluate(job, &instruction, &turn.text, token).await?;
            warnings.extend(verdict.warnings.iter().cloned());
            if verdict.notify {
                return Ok(Some((verdict.title, verdict.summary)));
            }
            return Ok(None);
        }

        Ok(Some((
            format!("{} finished", job.name),
            first_chars(&turn.text, 500),
        )))
    }

    /// Everything that happens once a run's outcome is written.
    fn settle(
        &self,
        job: &AutomationJob,
        status: RunStatus,
        error: Option<&str>,
        notify: Option<(String, String)>,
    ) -> Result<()> {
        if status == RunStatus::Error {
            // An error always notifies. The evaluate step never gets to veto
            // it: a failure nobody was told about is a job that has quietly not
            // worked for a week.
            self.notify(
                job,
                &format!("{} failed", job.name),
                error.unwrap_or_default(),
                NotificationLevel::Error,
            )?;
        } else if let Some((title, body)) = notify {
            self.notify(job, &title, &body, NotificationLevel::Info)?;
        }

        // Trimming here rather than on a sweep: the row that pushes the history
        // over the cap is the one that just landed.
        let retention = i64::try_from(self.config().scheduler.run_retention).unwrap_or(i64::MAX);
        let trimmed: Vec<TrimmedRun> = self.inner.jobs.trim_runs(&job.id, retention)?;
        for gone in trimmed {
            // Only the sessions this engine minted. A payload session key the
            // operator chose is deliberately shared and long-lived, and
            // deleting it would take a conversation with it.
            let Some(session_key) = gone.session_key else {
                continue;
            };
            if payload_session_key(&job.payload) == Some(&session_key) {
                continue;
            }
            if let Some(delete) = &self.inner.delete_session {
                delete(&session_key);
            }
        }

        if job.delete_after_run && matches!(job.schedule, AutomationSchedule::At(_)) {
            self.inner.jobs.delete_job(&job.id)?;
            return Ok(());
        }

        // The authoritative next time, now that the run's real duration is
        // known. An interval job that took longer than its interval lands in
        // the future rather than immediately due, which is what stops a slow
        // job from becoming a permanent backlog.
        if !matches!(job.schedule, AutomationSchedule::At(_)) {
            let next = self.next_run_for(job, self.inner.clock.now_ms())?;
            self.inner.jobs.set_next_run(&job.id, next)?;
        }
        Ok(())
    }

    /// When this job should fire next, or 0 for "not scheduled".
    ///
    /// Zero for a one-shot, which has just used up its only occurrence, and
    /// zero for a disabled job, which has no next occurrence at all — an
    /// on-demand run must not quietly put one back on the timer's books.
    fn next_run_for(&self, job: &AutomationJob, now_ms: i64) -> Result<i64> {
        if matches!(job.schedule, AutomationSchedule::At(_)) || !job.enabled {
            return Ok(0);
        }
        next_run_after(&job.schedule, now_ms, &self.timezone())
    }

    fn notify(
        &self,
        job: &AutomationJob,
        title: &str,
        body: &str,
        level: NotificationLevel,
    ) -> Result<()> {
        let notification = (self.inner.raise)(CreateNotificationInput {
            title: title.to_owned(),
            body: body.to_owned(),
            level,
            session_key: None,
            job_id: Some(job.id.clone()),
        })?;

        (self.inner.broadcast)(NotificationBroadcast {
            id: notification.id,
            title: notification.title,
            body: notification.body,
            level: notification.level,
            created_at_ms: notification.created_at_ms,
            session_key: notification.session_key,
            job_id: Some(job.id.clone()),
        });
        Ok(())
    }

    /// Drives one turn through the hub and collects its answer.
    ///
    /// The timeout is not belt-and-braces: a hub-level refusal that arrives
    /// after the collector has stopped listening, or a socket that never
    /// produces a turn end, would otherwise leave the run `pending` forever
    /// with no process behind it.
    async fn run_turn(
        &self,
        job: &AutomationJob,
        run: &AutomationRun,
        message: &str,
        token: &CancellationToken,
    ) -> Result<TurnOutcome> {
        let (collector, outcome) = TurnCollector::new();
        // The fallback is for a run row written before runs began recording a
        // key. The job id, so such runs share one session rather than minting a
        // fresh one each time.
        let session_key = run.session_key.clone().unwrap_or_else(|| job.id.clone());

        let sink = Arc::clone(&collector);
        let connection = (self.inner.connect)(SchedulerConnectOptions {
            send: Arc::new(move |event: ServerMessage| sink.receive(&event)),
            session_key: session_key.clone(),
            channel: AUTOMATION_ORIGIN.to_owned(),
            agent_id: payload_agent_id(&job.payload).cloned(),
            workspace_id: payload_workspace_id(&job.payload).cloned(),
            // This connection drives the turn and collects its output; it
            // cannot answer anything. Saying so is what lets the approval gate
            // tell an unattended run from a conversation someone has open.
            unattended: true,
        });

        connection.receive(ClientMessage::UserMessage(UserMessageRequest {
            tag: UserMessageTag,
            session_key: session_key.clone(),
            content: message.to_owned(),
            attachments: Vec::new(),
            agent_id: payload_agent_id(&job.payload).cloned(),
            // The run id, so a redelivery is acknowledged rather than run twice.
            client_message_id: Some(run.id.clone()),
        }));

        let stop = || {
            connection.receive(ClientMessage::StopTurn(StopTurnMessage {
                tag: StopTurnTag,
                session_key: session_key.clone(),
            }));
        };

        let result = tokio::select! {
            outcome = outcome => outcome.ok(),
            () = token.cancelled() => {
                stop();
                collector.finish(Some(INTERRUPTED_BY_SHUTDOWN.to_owned()));
                None
            }
            () = tokio::time::sleep(self.inner.run_timeout) => {
                stop();
                collector.finish(Some(
                    "The run exceeded its time limit and was stopped.".to_owned(),
                ));
                None
            }
        };
        connection.close();

        Ok(result.unwrap_or_else(|| TurnOutcome {
            text: String::new(),
            error: Some(if token.is_cancelled() {
                INTERRUPTED_BY_SHUTDOWN.to_owned()
            } else {
                "The run exceeded its time limit and was stopped.".to_owned()
            }),
            warnings: Vec::new(),
        }))
    }

    // Heartbeat

    async fn decide(
        &self,
        job: &AutomationJob,
        payload: &HeartbeatPayload,
        token: &CancellationToken,
    ) -> Result<crate::heartbeat::HeartbeatDecision> {
        let (Some(chat), Some(read_file)) = (&self.inner.chat, &self.inner.read_file) else {
            // Not a silent "always run": that would start an unbounded turn on
            // whatever the file happens to say, every interval, forever.
            return Err(WireError::new(
                ErrorKind::NotFound,
                "This build has no direct provider access, so a heartbeat cannot decide whether to run.",
            ));
        };

        let contents = match read_file(ReadTaskFile {
            // Defaulted here rather than at each composition root, so "the job
            // named no workspace" has one answer instead of one per wiring.
            workspace_id: payload
                .workspace_id
                .clone()
                .unwrap_or_else(|| DEFAULT_WORKSPACE_ID.to_owned()),
            path: payload.file.clone(),
            max_bytes: MAX_TASK_FILE_BYTES,
        })
        .await
        {
            Ok(contents) => contents,
            // A heartbeat with no task file is a normal idle state on a fresh
            // install, not a fault. It costs nothing — no provider call
            // happens.
            Err(failure) if failure.kind == ErrorKind::NotFound => {
                return Ok(crate::heartbeat::HeartbeatDecision {
                    action: HeartbeatAction::Skip,
                    reason: format!("No {} in the workspace.", payload.file),
                    instruction: String::new(),
                    warnings: Vec::new(),
                });
            }
            Err(failure) => return Err(failure),
        };

        if contents.trim().is_empty() {
            return Ok(crate::heartbeat::HeartbeatDecision {
                action: HeartbeatAction::Skip,
                reason: format!("{} is empty.", payload.file),
                instruction: String::new(),
                warnings: Vec::new(),
            });
        }

        let truncated = contents.chars().count() > MAX_TASK_FILE_BYTES;
        let result = chat(DirectChat {
            agent_id: payload.agent_id.clone(),
            model: payload.model.clone(),
            messages: build_decide_messages(&DecideMessagesInput {
                file: payload.file.clone(),
                contents: if truncated {
                    first_chars(&contents, MAX_TASK_FILE_BYTES)
                } else {
                    contents
                },
                now_iso: iso_8601(self.inner.clock.now_ms()),
            }),
            tools: vec![HEARTBEAT_TOOL.clone()],
            tool_choice: ToolChoice::Required,
            max_tokens: Some(256),
            token: token.clone(),
        })
        .await?;

        let mut decision = read_decision(&result, &payload.file);
        tracing::debug!(
            job_id = job.id,
            action = ?decision.action,
            reason = decision.reason,
            "heartbeat decision"
        );
        if truncated {
            decision.warnings.push(format!(
                "{} was truncated before the model read it.",
                payload.file
            ));
        }
        Ok(decision)
    }

    async fn evaluate(
        &self,
        job: &AutomationJob,
        instruction: &str,
        output: &str,
        token: &CancellationToken,
    ) -> Result<crate::heartbeat::HeartbeatEvaluation> {
        let fallback = format!("{} finished", job.name);
        let Some(chat) = &self.inner.chat else {
            return Ok(crate::heartbeat::HeartbeatEvaluation {
                notify: true,
                title: fallback,
                summary: first_chars(output, 500),
                warnings: Vec::new(),
            });
        };

        let model = match &job.payload {
            AutomationPayload::Heartbeat(payload) => payload.model.clone(),
            AutomationPayload::Scheduled(_) => None,
        };
        let result = chat(DirectChat {
            agent_id: payload_agent_id(&job.payload).cloned(),
            model,
            messages: build_evaluate_messages(&EvaluateMessagesInput {
                instruction: instruction.to_owned(),
                output: output.to_owned(),
            }),
            tools: vec![HEARTBEAT_RESULT_TOOL.clone()],
            tool_choice: ToolChoice::Required,
            max_tokens: Some(256),
            token: token.clone(),
        })
        .await?;

        Ok(read_evaluation(&result, &fallback))
    }
}

impl SchedulerPort for Scheduler {
    /// Runs a job now, out of band.
    ///
    /// Returns the `pending` row rather than the finished one: a turn takes
    /// minutes, and the REST route that calls this answers 202 so a browser is
    /// not holding a request open for the length of an agent run.
    fn run_now(&self, job_id: &str) -> Result<AutomationRun> {
        let Some(job) = self.inner.jobs.get_job(job_id)? else {
            return Err(WireError::new(
                ErrorKind::NotFound,
                format!("No automation job with id \"{job_id}\"."),
            ));
        };
        if self.inner.state.lock().in_flight.contains_key(job_id) {
            return Err(WireError::new(
                ErrorKind::Conflict,
                format!("\"{}\" is already running.", job.name),
            ));
        }
        self.dispatch(&job, Vec::new())
    }

    fn refresh(&self) {
        if let Err(error) = self.refresh_now() {
            tracing::error!(err = %error.message, "automation refresh failed");
        }
    }

    fn enabled(&self) -> bool {
        self.config().scheduler.enabled
    }
}

// Payload accessors

/// The delivery fields are restated on each payload variant rather than nested,
/// so reading one means naming the variant. These three are the reads the
/// engine makes.
fn payload_session_key(payload: &AutomationPayload) -> Option<&String> {
    match payload {
        AutomationPayload::Scheduled(p) => p.session_key.as_ref(),
        AutomationPayload::Heartbeat(p) => p.session_key.as_ref(),
    }
}

fn payload_agent_id(payload: &AutomationPayload) -> Option<&String> {
    match payload {
        AutomationPayload::Scheduled(p) => p.agent_id.as_ref(),
        AutomationPayload::Heartbeat(p) => p.agent_id.as_ref(),
    }
}

fn payload_workspace_id(payload: &AutomationPayload) -> Option<&String> {
    match payload {
        AutomationPayload::Scheduled(p) => p.workspace_id.as_ref(),
        AutomationPayload::Heartbeat(p) => p.workspace_id.as_ref(),
    }
}

fn payload_delivers(payload: &AutomationPayload) -> bool {
    match payload {
        AutomationPayload::Scheduled(p) => p.deliver,
        AutomationPayload::Heartbeat(p) => p.deliver,
    }
}

/// The first `limit` characters, so a cut never lands mid-code-point.
fn first_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// An epoch-millisecond instant as the model is shown it.
///
/// Formatted here rather than through a date library because the only consumer
/// is a sentence in a prompt, and the one property it needs is that "tomorrow"
/// in a task file has something to be relative to.
fn iso_8601(now_ms: i64) -> String {
    let seconds = now_ms.div_euclid(1000);
    let millis = now_ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Days since the epoch to a civil date, by Howard Hinnant's algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if m <= 2 { y + 1 } else { y },
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}
