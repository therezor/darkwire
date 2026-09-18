//! `exec` — run a program in the workspace.
//!
//! The schema takes `argv: string[]`, never a command string. That single
//! choice removes an entire class of attack: a deny-list of `$(...)`,
//! backticks and `| sh` in a string handed to a shell-less spawn can only ever
//! reject legitimate arguments (a commit message containing `$HOME`, a grep
//! for a pipe) while blocking nothing at all. With no string, there is no
//! parser, and nothing for a metacharacter to mean.
//!
//! Three parties, and the split between them is the point. The exec guard
//! decides **whether** a command may run, and with what arguments and
//! environment. A [`CommandRunner`](crate::CommandRunner) decides **where** —
//! a host child process or a container. What is left, and what this file owns:
//!
//!  - **Reconciling the two timeouts.** The model may ask for less time than
//!    the operator allows, never more, and `0` means unlimited on both sides —
//!    so it is not a plain `min`. See [`effective_timeout`].
//!
//!  - **A non-zero exit is a result, not a failure.** `grep` finding nothing
//!    exits 1, and a compiler failing is the answer the model asked for. Both
//!    come back as `is_error` with the output intact, so the model can read
//!    the compiler errors rather than a wrapper's opinion of them.
//!
//!  - **How an outcome reads.** [`render_run`] is what the model sees, and
//!    `details` is what an approval prompt and the audit log see.
//!
//! The description and the argument schema have to cover **both** placements,
//! because they are computed once and the same definition is advertised to a
//! host agent and a sandboxed one. Stating the host rules flatly — "there is no
//! shell", "paths outside the workspace are errors" — puts them beside a Sandbox
//! section telling a sandboxed agent to run `ddgr --json | jq`. Faced with the
//! contradiction a model does the conservative thing and reports it cannot do
//! the task at all, which is how "I can't search the internet" comes out of an
//! agent holding a search tool.

use std::sync::Arc;

use darkwire_core::Result;
use darkwire_protocol::{ToolAnnotations, ToolRisk};
use darkwire_security::{ExecGuardOptions, ExecPlan, guard_exec};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::builtin::built;
use crate::runner::{RunOutcome, RunRequest};
use crate::tool::{
    AnyTool, BoxFuture, ToolContext, ToolHandler, ToolOutput, ToolSpec, TypedTool,
    assert_not_aborted,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecArgs {
    #[schemars(
        length(min = 1),
        description = "Program and arguments as separate strings, e.g. [\"git\",\"status\",\"--short\"]. On the host there is no shell, so pipes, redirection and globs are not interpreted. In a sandbox there is one: [\"bash\",\"-lc\",\"a | b > c\"] works. Your instructions say which applies: a \"Sandbox\" section means the second."
    )]
    argv: Vec<String>,
    #[schemars(
        description = "Kill the process after this many milliseconds. Capped by the operator setting."
    )]
    timeout_ms: Option<u64>,
}

struct Exec;

impl ToolHandler for Exec {
    type Args = ExecArgs;

    fn execute<'a>(
        &'a self,
        args: ExecArgs,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolOutput>> {
        Box::pin(async move {
            assert_not_aborted(&ctx.token, "exec")?;

            // Fails on a denied binary, a shell, or an argument reaching outside
            // the workspace. `plan.paths` is what an approval prompt should
            // display.
            let plan = guard_exec(
                &args.argv,
                &ExecGuardOptions {
                    jail: &ctx.jail,
                    config: Some(&ctx.config.exec),
                    env: &ctx.env,
                    sandboxed: ctx.sandboxed,
                },
            )?;

            // Where it runs is the context's to decide; whether it may run was
            // settled above.
            let outcome = ctx
                .runner
                .run(RunRequest {
                    timeout_ms: effective_timeout(&plan, args.timeout_ms),
                    plan: plan.clone(),
                    token: ctx.token.clone(),
                    clock: Arc::clone(&ctx.clock),
                    tee: None,
                })
                .await?;
            Ok(render_run(&args.argv, &plan, &outcome))
        })
    }
}

/// The model may ask for less time than the operator allows, never more.
///
/// `0` means unlimited on both sides, which is why this is not a plain `min`:
/// an operator cap of 0 must not clamp a model's 30-second request to zero,
/// and a model's 0 must not lift a configured cap.
pub fn effective_timeout(plan: &ExecPlan, requested: Option<u64>) -> u64 {
    match requested {
        None | Some(0) => plan.timeout_ms,
        Some(requested) if plan.timeout_ms == 0 => requested,
        Some(requested) => requested.min(plan.timeout_ms),
    }
}

/// How a command's outcome reads to the model.
///
/// One rendering for both placements. A second would drift — eventually
/// forgetting to mention the transcript, or the exit code — and the model would
/// have no way to tell which one it was reading.
pub fn render_run(argv: &[String], plan: &ExecPlan, outcome: &RunOutcome) -> ToolOutput {
    let mut sections: Vec<String> = Vec::new();
    if !outcome.stdout.is_empty() {
        sections.push(outcome.stdout.trim_end().to_owned());
    }
    if !outcome.stderr.is_empty() {
        sections.push(format!("[stderr]\n{}", outcome.stderr.trim_end()));
    }
    if sections.is_empty() {
        sections.push("(no output)".to_owned());
    }

    if outcome.truncated {
        // With a transcript there is somewhere to send the model for the rest,
        // and saying so is the difference between a truncation it can recover
        // from and one it has to guess around. That is the whole token
        // argument: a 12,000-token scan comes back as a summary and a path.
        //
        // **`exec`, explicitly, not `read_file`.** The transcript is mounted
        // into the container from outside the workspace, so its path is
        // absolute and outside the jail — `read_file` would refuse it as an
        // escape. Naming the wrong tool here would send the model down a path
        // that cannot work and cost it a turn discovering that.
        sections.push(match &outcome.transcript_dir {
            None => format!(
                "[exec: output truncated at {} bytes per stream.]",
                plan.max_output_bytes
            ),
            Some(dir) => format!(
                "[exec: output truncated at {} bytes per stream. The complete output is at {dir}/stdout.log and {dir}/stderr.log. Reach it with exec: grep/tail/cat those paths rather than re-running the command. read_file cannot: the path is outside the workspace.]",
                plan.max_output_bytes
            ),
        });
    }
    if outcome.timed_out {
        sections.push("[exec: the command was killed after exceeding its time limit.]".to_owned());
    }

    // The exit status goes last, where a reader — and a model reading a
    // truncated result — is most likely to still see it.
    let code = outcome.code.unwrap_or(0);
    sections.push(match &outcome.signal {
        Some(signal) => format!("Killed by {signal}"),
        None => format!("Exit code: {code}"),
    });

    let mut output = ToolOutput {
        content: sections.join("\n\n"),
        is_error: outcome.timed_out || outcome.signal.is_some() || code != 0,
        details: serde_json::Map::new(),
    };
    output = output
        .with_detail(
            "argv",
            Value::Array(argv.iter().map(|a| Value::from(a.as_str())).collect()),
        )
        .with_detail(
            "paths",
            Value::Array(
                plan.paths
                    .iter()
                    .map(|path| Value::from(path.to_string_lossy().into_owned()))
                    .collect(),
            ),
        )
        .with_detail("exitCode", outcome.code.map_or(Value::Null, Value::from))
        .with_detail(
            "signal",
            outcome.signal.as_deref().map_or(Value::Null, Value::from),
        )
        .with_detail("timedOut", outcome.timed_out);
    if let Some(dir) = &outcome.transcript_dir {
        output = output.with_detail("transcriptDir", dir.as_str());
    }
    output
}

/// The `exec` tool.
pub fn exec_tool() -> AnyTool {
    built(TypedTool::new(
        ToolSpec::new(
            "exec",
            "Run a program and return its output. Arguments are passed as an argv array. On the host it runs in the workspace root on the real filesystem, so an argument pointing outside the workspace is refused rather than clamped: \"/etc/passwd\" and \"../x\" are errors, and there is no shell. In a sandbox it runs inside a container that mounts only the workspace: a shell is available, and absolute paths address the container rather than this machine. Your instructions carry a \"Sandbox\" section when that is the case, naming what the image holds.",
        )
        .risk(ToolRisk::Exec)
        .annotations(ToolAnnotations {
            title: Some("Run command".to_owned()),
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            open_world_hint: Some(true),
            ..ToolAnnotations::default()
        }),
        Exec,
    ))
}
