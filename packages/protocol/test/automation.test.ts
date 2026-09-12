import { describe, expect, it } from 'vitest';

import {
  AutomationJobSchema,
  AutomationPayloadSchema,
  AutomationRunSchema,
  AutomationScheduleSchema,
  CreateAutomationJobSchema,
} from '#src/automation.js';

describe('AutomationScheduleSchema', () => {
  it('parses each schedule kind', () => {
    expect(
      AutomationScheduleSchema.parse({ kind: 'at', atMs: 1_700_000_000_000 }),
    ).toMatchObject({
      kind: 'at',
    });
    expect(
      AutomationScheduleSchema.parse({ kind: 'every', everyMs: 60_000 }),
    ).toMatchObject({
      kind: 'every',
    });
    expect(
      AutomationScheduleSchema.parse({ kind: 'cron', expr: '0 9 * * 1' }),
    ).toMatchObject({
      kind: 'cron',
    });
  });

  it('rejects fields belonging to a different kind', () => {
    // The whole point of the discriminated union: `{kind: 'cron', atMs: 5}` was
    // representable when every variant's fields sat side by side.
    expect(
      AutomationScheduleSchema.safeParse({ kind: 'cron', atMs: 5 }).success,
    ).toBe(false);
    expect(
      AutomationScheduleSchema.safeParse({
        kind: 'at',
        atMs: 1,
        expr: '* * * * *',
      }).success,
    ).toBe(false);
  });

  it('requires the field its kind implies', () => {
    expect(AutomationScheduleSchema.safeParse({ kind: 'at' }).success).toBe(
      false,
    );
    expect(AutomationScheduleSchema.safeParse({ kind: 'every' }).success).toBe(
      false,
    );
    expect(AutomationScheduleSchema.safeParse({ kind: 'cron' }).success).toBe(
      false,
    );
  });

  it('rejects a zero interval that would spin the timer', () => {
    expect(
      AutomationScheduleSchema.safeParse({ kind: 'every', everyMs: 0 }).success,
    ).toBe(false);
  });

  it('refuses a per-job timezone rather than ignoring it', () => {
    // A job has no zone of its own: the install's `ui.timezone` reads every
    // expression. Strict rather than key-stripping so that a hand-edited job or
    // an importer written against the old shape fails loudly, instead of being
    // silently rescheduled onto a different clock than the one it names.
    expect(
      AutomationScheduleSchema.safeParse({
        kind: 'cron',
        expr: '* * * * *',
        tz: 'Europe/Kyiv',
      }).success,
    ).toBe(false);

    const parsed = AutomationScheduleSchema.parse({
      kind: 'cron',
      expr: '* * * * *',
    });
    expect(parsed).not.toHaveProperty('tz');
  });

  it('rejects an unknown kind', () => {
    expect(AutomationScheduleSchema.safeParse({ kind: 'hourly' }).success).toBe(
      false,
    );
  });
});

describe('AutomationPayloadSchema', () => {
  it('requires a message for a scheduled payload', () => {
    expect(
      AutomationPayloadSchema.safeParse({ kind: 'scheduled' }).success,
    ).toBe(false);
    expect(
      AutomationPayloadSchema.safeParse({ kind: 'scheduled', message: '' })
        .success,
    ).toBe(false);
    expect(
      AutomationPayloadSchema.safeParse({
        kind: 'scheduled',
        message: 'check the build',
      }).success,
    ).toBe(true);
  });

  it('defaults a heartbeat to TASK.md', () => {
    const parsed = AutomationPayloadSchema.parse({ kind: 'heartbeat' });
    expect(parsed).toMatchObject({ kind: 'heartbeat', file: 'TASK.md' });
  });

  it('does not deliver by default', () => {
    // A job that fires silently into its run history is the safe default; opting
    // in is what makes a notification a deliberate choice.
    expect(AutomationPayloadSchema.parse({ kind: 'heartbeat' })).toMatchObject({
      deliver: false,
    });
  });

  it('carries delivery fields on both kinds', () => {
    const scheduled = AutomationPayloadSchema.parse({
      kind: 'scheduled',
      message: 'x',
      deliver: true,
      channel: 'telegram',
      to: '12345',
    });
    expect(scheduled).toMatchObject({
      deliver: true,
      channel: 'telegram',
      to: '12345',
    });

    const heartbeat = AutomationPayloadSchema.parse({
      kind: 'heartbeat',
      deliver: true,
      targets: { telegram: '12345' },
    });
    expect(heartbeat).toMatchObject({ targets: { telegram: '12345' } });
  });

  it('carries a workspace on both kinds, and leaves it unset by default', () => {
    // Unset means the default workspace, which is what every job written
    // before the field existed relies on.
    expect(
      AutomationPayloadSchema.parse({ kind: 'heartbeat' }),
    ).not.toHaveProperty('workspaceId');

    expect(
      AutomationPayloadSchema.parse({
        kind: 'scheduled',
        message: 'x',
        workspaceId: 'research',
      }),
    ).toMatchObject({ workspaceId: 'research' });
    expect(
      AutomationPayloadSchema.parse({
        kind: 'heartbeat',
        workspaceId: 'research',
      }),
    ).toMatchObject({ workspaceId: 'research' });
  });

  it('still rejects an unknown key on both kinds', () => {
    // Both variants are `strictObject`, and adding a field must not have
    // relaxed that — an unknown key is a typo the author should hear about.
    expect(
      AutomationPayloadSchema.safeParse({
        kind: 'scheduled',
        message: 'x',
        workspace: 'research',
      }).success,
    ).toBe(false);
    expect(
      AutomationPayloadSchema.safeParse({ kind: 'heartbeat', nope: 1 }).success,
    ).toBe(false);
  });

  it('leaves sessionKey unset so each run gets an isolated session', () => {
    // Pinning a sessionKey is how a nightly job grows an unbounded context
    // window, so it must be explicit.
    expect(
      AutomationPayloadSchema.parse({ kind: 'heartbeat' }),
    ).not.toHaveProperty('sessionKey');
  });
});

describe('AutomationJobSchema', () => {
  const schedule = { kind: 'cron' as const, expr: '0 9 * * *' };
  const payload = { kind: 'scheduled' as const, message: 'good morning' };

  it('fills in state and flags', () => {
    const job = AutomationJobSchema.parse({
      id: 'j1',
      name: 'Morning',
      schedule,
      payload,
    });
    expect(job).toMatchObject({
      enabled: true,
      deleteAfterRun: false,
      state: {
        nextRunAtMs: 0,
        lastStatus: 'pending',
        runCount: 0,
        lastError: '',
      },
    });
  });

  it('requires an id and a name', () => {
    expect(
      AutomationJobSchema.safeParse({ name: 'x', schedule, payload }).success,
    ).toBe(false);
    expect(
      AutomationJobSchema.safeParse({ id: '', name: 'x', schedule, payload })
        .success,
    ).toBe(false);
    expect(
      AutomationJobSchema.safeParse({ id: 'j', name: '', schedule, payload })
        .success,
    ).toBe(false);
  });

  it('rejects an unknown run status', () => {
    expect(
      AutomationJobSchema.safeParse({
        id: 'j',
        name: 'n',
        schedule,
        payload,
        state: { lastStatus: 'running' },
      }).success,
    ).toBe(false);
  });

  it('accepts the skipped status a heartbeat produces', () => {
    const job = AutomationJobSchema.parse({
      id: 'j',
      name: 'n',
      schedule,
      payload,
      state: { lastStatus: 'skipped' },
    });
    expect(job.state.lastStatus).toBe('skipped');
  });
});

describe('AutomationRunSchema', () => {
  const base = {
    id: 'r1',
    jobId: 'j1',
    startedAtMs: 1_700_000_000_000,
    status: 'ok' as const,
  };

  it('defaults to no warnings', () => {
    expect(AutomationRunSchema.parse(base).warnings).toEqual([]);
  });

  it('does not share the warnings array between parses', () => {
    // The same defect `pinnedSkills` has a test for: a shared default array is
    // one run's caveat showing up on every other run.
    expect(AutomationRunSchema.parse(base).warnings).not.toBe(
      AutomationRunSchema.parse(base).warnings,
    );
  });

  it('keeps warnings separate from an error, so a caveat is not a failure', () => {
    const run = AutomationRunSchema.parse({
      ...base,
      warnings: ['deliver: true reached no channel'],
    });
    expect(run.status).toBe('ok');
    expect(run).not.toHaveProperty('error');
    expect(run.warnings).toEqual(['deliver: true reached no channel']);
  });
});

describe('CreateAutomationJobSchema', () => {
  it('omits server-assigned fields', () => {
    const parsed = CreateAutomationJobSchema.parse({
      name: 'One-shot',
      schedule: { kind: 'at', atMs: 1_700_000_000_000 },
      payload: { kind: 'scheduled', message: 'ping' },
      deleteAfterRun: true,
    });
    expect(parsed).not.toHaveProperty('id');
    expect(parsed).not.toHaveProperty('state');
    expect(parsed.deleteAfterRun).toBe(true);
  });

  it('still validates the schedule union', () => {
    expect(
      CreateAutomationJobSchema.safeParse({
        name: 'bad',
        schedule: { kind: 'cron' },
        payload: { kind: 'scheduled', message: 'x' },
      }).success,
    ).toBe(false);
  });
});
