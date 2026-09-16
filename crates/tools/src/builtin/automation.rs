//! `automation` — scheduling work the agent will do later.
//!
//! The one built-in that acts on the *future* rather than on the workspace,
//! and the one an agent does not get by default: it is absent from
//! `DEFAULT_AGENT_TOOLS`, so a new agent cannot reach it at all until an
//! operator grants it. That asymmetry is deliberate. A single approved `exec`
//! runs once; a single approved `automation` create runs forever, unattended,
//! on a timer.
//!
//! **The model's surface is a strict subset of the operator's.** It can
//! create, list and delete — not update, not run on demand, not enable or
//! disable. The missing verb that matters is `update`: repointing an existing
//! job's payload is the one edit nobody watches happen, and an agent that could
//! do it would be able to turn "post the weather" into anything at all without
//! the row ever looking new.
//!
//! Everything this tool cannot be trusted with lives on the other side of
//! [`AutomationPort`](crate::AutomationPort), which the composition root binds
//! to the calling agent and session: ownership, the per-agent cap, the refusal
//! to schedule from inside a scheduled run, and **which agent the scheduled
//! turn runs as**. The tool passes arguments a model wrote; it does not get to
//! say who it is, and so it does not get to say who the job will be.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use darkwire_core::Result;
use darkwire_protocol::json::js_trim;
use darkwire_protocol::{
    AtKind, AtSchedule, AutomationJob, AutomationPayload, AutomationSchedule, CreateAutomationJob,
    CronKind, CronSchedule, EveryKind, EverySchedule, ScheduledKind, ScheduledPayload,
    ToolAnnotations, ToolRisk,
};
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::automation::{AutomationOutcome, AutomationRefusal};
use crate::builtin::built;
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Action {
    Create,
    List,
    Delete,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AutomationArgs {
    #[schemars(
        description = "create schedules a job, list shows the ones you made, delete removes one."
    )]
    action: Action,
    #[schemars(description = "Short label for the job. Required to create.")]
    name: Option<String>,
    #[schemars(
        description = "What to ask the agent when the job fires, written as if you were typing it into a new conversation that has no history. Self-contained: spell out what a reader who was not here would need. Required to create."
    )]
    message: Option<String>,
    #[schemars(
        range(min = 1),
        description = "Repeat this often. Counted from the end of the previous run."
    )]
    every_minutes: Option<u64>,
    #[schemars(
        description = "A 5-field cron expression: minute hour day-of-month month day-of-week."
    )]
    cron: Option<String>,
    #[schemars(
        description = "ISO instant for a one-off, such as 2026-08-01T09:00:00Z. Compute it yourself."
    )]
    at: Option<String>,
    #[schemars(description = "Remove the job once it has fired. Use for a one-off reminder.")]
    delete_after_run: Option<bool>,
    #[schemars(description = "Which job to delete. Required to delete.")]
    job_id: Option<String>,
}

/// The description does three jobs beyond saying what the tool is.
///
/// It points at the current time already in the system prompt rather than
/// restating it, it says which clock a cron expression is read against, and it
/// says that the run starts in a *fresh session*.
///
/// The second sentence used to warn that a zoneless cron and the prompt's clock
/// disagreed. There is no per-job zone now: one install-wide `ui.timezone`
/// reads every expression and renders every timestamp, so the warning has
/// become a promise. It is still worth stating — a model that has been trained
/// on the old advice will otherwise convert an hour it did not need to convert.
///
/// The third is the one whose absence looked like the model not understanding
/// the tool. A scheduled run gets its own `automation:{jobId}` session and
/// cannot see the conversation that created it, so `message: "do the thing we
/// discussed"` schedules a turn that has no idea what the thing is — and the
/// failure lands a week later, on somebody who was not there. A model has no
/// way to discover it: the create succeeds, and the first run is the only
/// evidence.
const DESCRIPTION: &str = "Schedule work for later, list what you have scheduled, or cancel it. To create, give a name, a message, and exactly one of: every_minutes, cron, or at. The current time is in your system prompt — compute an \"at\" instant from it yourself. A cron expression is read in the install timezone, which is the zone named beside the current time in your prompt; write the hour you mean on that clock and do not convert it. The job runs in a fresh conversation that cannot see this one, so write the message so it stands alone — name the files, people and facts it needs instead of referring back to what was said here. You only ever see and delete jobs you created yourself.";

/// What went wrong, in words a model can act on rather than retry blindly.
fn refused<T>(outcome: &AutomationOutcome<T>) -> ToolOutput {
    let text = match outcome {
        Ok(_) | Err(AutomationRefusal::Unschedulable(_)) => {
            "Refused: that schedule cannot be honoured."
        }
        Err(AutomationRefusal::Nested) => {
            "Refused: this turn is itself a scheduled run, and a job may not schedule more jobs. Do the work now instead."
        }
        Err(AutomationRefusal::AtCapacity) => {
            "Refused: you already hold as many scheduled jobs as you may. Delete one before creating another."
        }
        Err(AutomationRefusal::NotYours) => "Refused: no such job, or it was not one you created.",
    };
    let detail = match outcome {
        Err(AutomationRefusal::Unschedulable(detail)) if !detail.is_empty() => {
            format!(" {detail}")
        }
        _ => String::new(),
    };
    ToolOutput::error(format!("{text}{detail}"))
}

/// `Date.parse` for the shapes a model writes: an RFC 3339 instant, or a
/// naive `YYYY-MM-DDTHH:MM:SS` / `YYYY-MM-DD` read as UTC.
fn parse_instant(text: &str) -> Option<u64> {
    let trimmed = js_trim(text);
    let millis = DateTime::parse_from_rfc3339(trimmed)
        .map(|instant| instant.timestamp_millis())
        .or_else(|_| {
            NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S")
                .map(|naive| naive.and_utc().timestamp_millis())
        })
        .or_else(|_| {
            NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").map(|date| {
                date.and_time(chrono::NaiveTime::MIN)
                    .and_utc()
                    .timestamp_millis()
            })
        })
        .ok()?;
    u64::try_from(millis).ok()
}

/// The three schedule shapes, as exactly one.
///
/// Refused rather than resolved by precedence when a model sends two: picking
/// one silently is how a job ends up on a schedule nobody wrote, and the model
/// can simply be told to choose.
fn to_schedule(args: &AutomationArgs) -> std::result::Result<AutomationSchedule, String> {
    let given = [
        args.every_minutes.is_some(),
        args.cron.is_some(),
        args.at.is_some(),
    ]
    .into_iter()
    .filter(|given| *given)
    .count();
    if given == 0 {
        return Err("Give exactly one of every_minutes, cron or at.".to_owned());
    }
    if given > 1 {
        return Err("Give only one of every_minutes, cron or at, not several.".to_owned());
    }

    if let Some(minutes) = args.every_minutes {
        let every_ms = minutes
            .checked_mul(60_000)
            .ok_or_else(|| "every_minutes is too large to schedule.".to_owned())?;
        return Ok(AutomationSchedule::Every(EverySchedule {
            kind: EveryKind,
            every_ms,
        }));
    }
    if let Some(cron) = &args.cron {
        return Ok(AutomationSchedule::Cron(CronSchedule {
            kind: CronKind,
            expr: js_trim(cron).to_owned(),
        }));
    }
    let at_ms = args
        .at
        .as_deref()
        .and_then(parse_instant)
        .ok_or_else(|| "at must be an ISO instant, such as 2026-08-01T09:00:00Z.".to_owned())?;
    Ok(AutomationSchedule::At(AtSchedule {
        kind: AtKind,
        at_ms,
    }))
}

/// An instant the model can compare against the clock in its own prompt.
fn iso_of(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map_or_else(
            || format!("{ms} ms"),
            |instant| instant.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        )
}

/// A schedule in the words the model wrote it in.
///
/// No timezone anywhere, and that is not an omission. An `every` is a
/// duration, a cron is echoed back verbatim as the operator's zone will read
/// it, and an instant is ISO — which is unambiguous on its own. The tool has
/// no zone to render in and should not grow one: the model's prompt already
/// names the install's, beside a current time in both forms.
fn schedule_of(schedule: &AutomationSchedule) -> String {
    match schedule {
        AutomationSchedule::Every(every) => format!("every {} min", every.every_ms / 60_000),
        AutomationSchedule::Cron(cron) => format!("cron \"{}\"", cron.expr),
        AutomationSchedule::At(at) => format!("once at {}", iso_of(at.at_ms)),
    }
}

/// One job as a line a model can read back and act on.
///
/// The schedule and the next run are on it because without them this tool has
/// no feedback loop. `id — name` is enough to delete a job and not enough to
/// answer "what is scheduled?", to notice that a cron was read differently
/// than it was meant, or to see that a one-shot has already fired. A model
/// that cannot check its own work re-creates it instead.
fn detail_of(job: &AutomationJob) -> String {
    let next = if job.state.next_run_at_ms == 0 {
        "not scheduled".to_owned()
    } else {
        format!("next {}", iso_of(job.state.next_run_at_ms))
    };
    let disabled = if job.enabled { "" } else { " · disabled" };
    format!("{} · {next}{disabled}", schedule_of(&job.schedule))
}

fn describe(job: &AutomationJob) -> String {
    format!("{} — {} · {}", job.id, job.name, detail_of(job))
}

/// A non-empty, trimmed argument, or `None`.
fn given(value: Option<&String>) -> Option<&str> {
    value
        .map(|text| js_trim(text))
        .filter(|text| !text.is_empty())
}

struct Automation;

impl Automation {
    fn create(port: &dyn crate::automation::AutomationPort, args: &AutomationArgs) -> ToolOutput {
        let Some(name) = given(args.name.as_ref()) else {
            return ToolOutput::error("Give a name to create a job.");
        };
        let Some(message) = given(args.message.as_ref()) else {
            return ToolOutput::error("Give a message to create a job.");
        };
        let schedule = match to_schedule(args) {
            Ok(schedule) => schedule,
            Err(message) => return ToolOutput::error(message),
        };
        let one_shot = matches!(schedule, AutomationSchedule::At(_));
        let input = CreateAutomationJob {
            name: name.to_owned(),
            schedule,
            payload: AutomationPayload::Scheduled(ScheduledPayload {
                deliver: false,
                channel: None,
                to: None,
                session_key: None,
                workspace_id: None,
                agent_id: None,
                targets: IndexMap::new(),
                kind: ScheduledKind,
                message: message.to_owned(),
            }),
            enabled: true,
            delete_after_run: args.delete_after_run.unwrap_or(one_shot),
        };
        let created = port.create(input);
        match &created {
            // The resolved first run, not an echo of the arguments. A cron the
            // model meant as 9am local and the scheduler read as something else
            // is only visible here, on the one line the model reads before
            // telling the user it is done.
            Ok(job) => ToolOutput::text(format!(
                "Scheduled \"{}\" ({}) · {}",
                job.name,
                job.id,
                detail_of(job)
            )),
            Err(_) => refused(&created),
        }
    }
}

impl ToolHandler for Automation {
    type Args = AutomationArgs;

    fn execute<'a>(
        &'a self,
        args: AutomationArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "automation")?;

            let Some(port) = &ctx.automation else {
                return Ok(ToolOutput::error(
                    "Refused: this installation has no scheduler, so nothing can be scheduled.",
                ));
            };

            Ok(match args.action {
                Action::List => {
                    let listed = port.list();
                    match &listed {
                        Err(_) => refused(&listed),
                        Ok(jobs) if jobs.is_empty() => {
                            ToolOutput::text("You have no scheduled jobs.")
                        }
                        Ok(jobs) => ToolOutput::text(format!(
                            "Your scheduled jobs:\n{}",
                            jobs.iter().map(describe).collect::<Vec<_>>().join("\n")
                        )),
                    }
                }
                Action::Delete => {
                    let Some(job_id) = given(args.job_id.as_ref()) else {
                        return Ok(ToolOutput::error("Give job_id to delete."));
                    };
                    let removed = port.delete(job_id);
                    match &removed {
                        Ok(()) => ToolOutput::text(format!("Deleted {job_id}.")),
                        Err(_) => refused(&removed),
                    }
                }
                Action::Create => Automation::create(port.as_ref(), &args),
            })
        })
    }
}

/// The `automation` tool.
pub fn automation_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new("automation", DESCRIPTION)
            // The band, not the act. Creating a job runs nothing itself; what
            // it grants is an agent turn happening later without anyone
            // watching, which is a larger grant than one command, not a
            // smaller one.
            .risk(ToolRisk::Exec)
            .annotations(ToolAnnotations {
                title: Some("Scheduled jobs".to_owned()),
                ..ToolAnnotations::default()
            }),
        Automation,
    ))
}
