/**
 * Automation jobs.
 *
 * Schedules and payloads are discriminated unions rather than a `kind` field
 * beside every variant's fields defaulted to null. The flat shape makes
 * `{kind: 'cron', atMs: 5}` representable and forces a null check on `expr`
 * even where `kind` already proves it is set; here each variant carries exactly
 * its own fields and an impossible combination fails to parse.
 *
 * The variants are **strict** rather than key-stripping, which is what makes
 * that last clause true: the default `z.object` would silently drop the stray
 * `atMs` from a cron schedule, turning an importer bug or a hand-edited job into
 * a job that quietly runs on the wrong trigger. These are our own persisted
 * shapes, so an unknown key is always a defect worth surfacing.
 */

import { z } from 'zod';

/**
 * The origin a scheduled run's session is recorded under.
 *
 * Load-bearing rather than descriptive, exactly as `SUBAGENT_ORIGIN` is:
 * `listSessions` excludes it from the unscoped listing. A job on a five-minute
 * interval writes about 105,000 sessions a year, and every one of them is a
 * single machine-started turn — a sidebar that listed them would bury the
 * conversations a person actually had under a year of cron output. They stay
 * reachable by key and by asking for `origin: 'automation'` by name, which is
 * what a run history's "open in chat" link does.
 */
export const AUTOMATION_ORIGIN = 'automation';

/** One-shot, at an absolute epoch-ms instant. */
export const AtScheduleSchema = z.strictObject({
  kind: z.literal('at'),
  atMs: z.number().int().nonnegative(),
});

/** Fixed interval. */
export const EveryScheduleSchema = z.strictObject({
  kind: z.literal('every'),
  everyMs: z.number().int().positive(),
});

/**
 * Standard 5-field cron, evaluated in the install's `ui.timezone`.
 *
 * **No per-job zone**, and its absence is deliberate. A per-job `tz` makes
 * "when does this fire" a question with three inputs — the job's zone, the
 * scheduler's default, and the zone the reader's browser happened to render the
 * answer in. One install-wide zone means the expression is read against the
 * same clock the next-run line is printed against.
 *
 * A job stored by an older build may still carry `tz`; `AutomationStore` strips
 * it on open, because this is a `strictObject` and would otherwise refuse to
 * parse the row rather than ignore the key.
 */
export const CronScheduleSchema = z.strictObject({
  kind: z.literal('cron'),
  expr: z.string().min(1),
});

export const AutomationScheduleSchema = z.discriminatedUnion('kind', [
  AtScheduleSchema,
  EveryScheduleSchema,
  CronScheduleSchema,
]);
export type AutomationSchedule = z.infer<typeof AutomationScheduleSchema>;

/**
 * Where a job's output goes. Shared by both payload kinds.
 *
 * `deliver: false` still runs the turn — the result lands in the job's run
 * history and the notification centre without interrupting anyone.
 */
const deliveryFields = {
  deliver: z.boolean().default(false),
  /** Channel id, e.g. `telegram`. Required in practice when `deliver` is set. */
  channel: z.string().optional(),
  /** Channel-specific destination (chat id, user id, room). */
  to: z.string().optional(),
  /**
   * Overrides the isolated `automation:{jobId}:{runId}` session. Setting this
   * is what makes a nightly job grow an unbounded context window, so the
   * default of "fresh session per run" is deliberate.
   */
  sessionKey: z.string().optional(),
  /**
   * The workspace a run's session is created in. Absent means the default.
   *
   * Only ever *creates*, exactly like the workspace on a socket frame. A job
   * pinned to a `sessionKey` that already exists runs in whatever workspace
   * that session is bound to and this is ignored — the stored row wins.
   *
   * A job an agent schedules for itself is stamped with the workspace that
   * agent was working in; the model does not get to name one, for the same
   * reason it does not get to name `agentId`.
   */
  workspaceId: z.string().min(1).optional(),
  agentId: z.string().optional(),
  /** Additional channel id → address fan-out. */
  targets: z.record(z.string(), z.string()).default({}),
};

export const AutomationDeliverySchema = z.object(deliveryFields);

/**
 * Sends a fixed message to the agent.
 *
 * Spelled out with `strictObject` over the shared delivery fields rather than
 * `AutomationDeliverySchema.extend(...)`, because `extend` inherits the base's
 * key-stripping behaviour and the point here is to reject a `file` on a
 * scheduled payload instead of dropping it.
 */
export const ScheduledPayloadSchema = z.strictObject({
  ...deliveryFields,
  kind: z.literal('scheduled'),
  message: z.string().min(1),
});

/**
 * Reads a markdown file and lets a cheap model decide whether to act.
 *
 * The decide/run/evaluate triad is why heartbeats aren't annoying: a forced
 * `heartbeat` tool call returns `skip` or `run`, and after a run the output is
 * evaluated again for whether it's worth interrupting the user.
 */
export const HeartbeatPayloadSchema = z.strictObject({
  ...deliveryFields,
  kind: z.literal('heartbeat'),
  /** Path relative to the workspace. */
  file: z.string().min(1).default('TASK.md'),
  model: z.string().optional(),
});

export const AutomationPayloadSchema = z.discriminatedUnion('kind', [
  ScheduledPayloadSchema,
  HeartbeatPayloadSchema,
]);
export type AutomationPayload = z.infer<typeof AutomationPayloadSchema>;

export const RunStatusSchema = z.enum(['pending', 'ok', 'error', 'skipped']);
export type RunStatus = z.infer<typeof RunStatusSchema>;

export const AutomationJobStateSchema = z.object({
  /** 0 when unscheduled (disabled, or a fired one-shot). */
  nextRunAtMs: z.number().int().nonnegative().default(0),
  lastRunAtMs: z.number().int().nonnegative().default(0),
  lastStatus: RunStatusSchema.default('pending'),
  lastError: z.string().default(''),
  runCount: z.number().int().nonnegative().default(0),
});

export const AutomationJobSchema = z.object({
  id: z.string().min(1),
  name: z.string().min(1),
  schedule: AutomationScheduleSchema,
  payload: AutomationPayloadSchema,
  state: AutomationJobStateSchema.prefault({}),
  enabled: z.boolean().default(true),
  createdAtMs: z.number().int().nonnegative().default(0),
  updatedAtMs: z.number().int().nonnegative().default(0),
  /** Self-destruct after firing — how a one-shot reminder cleans up. */
  deleteAfterRun: z.boolean().default(false),
  /**
   * Who made this, when it was not a person.
   *
   * Absent means the operator, through the panel. Present means an agent asked
   * for it during a turn, and names both the agent and the conversation — so a
   * job list that has grown mysterious can be traced back to the sentence that
   * caused it, and one agent's jobs can be found and removed together.
   *
   * A job with no attribution is the common case, which is why this is optional
   * rather than a required field carrying an empty string.
   */
  createdBy: z
    .object({
      agentId: z.string().min(1),
      /** The conversation the tool call came from. */
      sessionKey: z.string().min(1),
    })
    .optional(),
});
export type AutomationJob = z.infer<typeof AutomationJobSchema>;

/**
 * A single execution. Persisted as real rows rather than collapsing into
 * last-run-only state, so the UI can show a history and a nightly job that
 * failed three days ago is still diagnosable.
 */
export const AutomationRunSchema = z.object({
  id: z.string().min(1),
  jobId: z.string().min(1),
  startedAtMs: z.number().int().nonnegative(),
  finishedAtMs: z.number().int().nonnegative().optional(),
  status: RunStatusSchema,
  /** Set when the heartbeat model chose `skip`. */
  skipReason: z.string().optional(),
  error: z.string().optional(),
  output: z.string().optional(),
  sessionKey: z.string().optional(),
  /**
   * Things worth saying about a run that did not fail.
   *
   * A separate list rather than folding into `error`, because `status` is what
   * the panel colours and a run that succeeded with a caveat must not read as
   * broken. What lands here: `deliver: true` on an install with no channel
   * wired, a boot catch-up that coalesced several missed occurrences into one
   * run, output truncated at the cap. Each is something an operator would be
   * annoyed to discover was silently dropped, and none of them is a failure.
   */
  warnings: z.array(z.string()).default([]),
});
export type AutomationRun = z.infer<typeof AutomationRunSchema>;

/** Job creation over REST. Server assigns `id`, timestamps and `state`. */
export const CreateAutomationJobSchema = z.object({
  name: z.string().min(1),
  schedule: AutomationScheduleSchema,
  payload: AutomationPayloadSchema,
  enabled: z.boolean().default(true),
  deleteAfterRun: z.boolean().default(false),
});
export type CreateAutomationJob = z.infer<typeof CreateAutomationJobSchema>;

export const UpdateAutomationJobSchema = z.object({
  name: z.string().min(1).optional(),
  schedule: AutomationScheduleSchema.optional(),
  payload: AutomationPayloadSchema.optional(),
  enabled: z.boolean().optional(),
  deleteAfterRun: z.boolean().optional(),
});
export type UpdateAutomationJob = z.infer<typeof UpdateAutomationJobSchema>;
