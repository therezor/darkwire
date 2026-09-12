/**
 * The form ⇄ wire conversion, as pure functions.
 *
 * Everything interesting about this form is a conversion — minutes to
 * milliseconds, a `datetime-local` string to an instant, a schedule kind to
 * which fields exist — and none of it needs a DOM to be wrong in.
 *
 * The zone is an argument rather than something these functions read, which is
 * what lets the cases below run in a zone the machine is not in. That matters:
 * the bug this design replaced was a `datetime-local` value parsed as the
 * browser's wall clock, and a test that used the browser's zone could never
 * have caught it.
 */

import { describe, expect, it } from 'vitest';

import { createWebI18n } from '@ghostwire/i18n/web';
import type { AutomationJob } from '@ghostwire/protocol';

import {
  describeSchedule,
  emptyJobForm,
  toJobForm,
  toJobRequest,
} from '@/automation/job-form.js';

const t = createWebI18n('en').getFixedT(null, 'web');

/** Deliberately not UTC and not the CI runner's zone. */
const TZ = 'Europe/Kyiv';

function job(over: Partial<AutomationJob> = {}): AutomationJob {
  return {
    id: 'j1',
    name: 'Morning',
    schedule: { kind: 'cron', expr: '0 9 * * *' },
    payload: {
      kind: 'scheduled',
      message: 'check the build',
      deliver: false,
      targets: {},
    },
    enabled: true,
    deleteAfterRun: false,
    createdAtMs: 0,
    updatedAtMs: 0,
    state: {
      nextRunAtMs: 0,
      lastRunAtMs: 0,
      lastStatus: 'pending',
      lastError: '',
      runCount: 0,
    },
    ...over,
  };
}

/** The form as `emptyJobForm` makes it, with the fields a case cares about set. */
function form(
  over: Partial<ReturnType<typeof emptyJobForm>> = {},
): ReturnType<typeof emptyJobForm> {
  return { ...emptyJobForm(TZ), ...over };
}

describe('toJobForm', () => {
  it('reads a cron schedule into its own fields', () => {
    expect(toJobForm(job(), TZ)).toMatchObject({
      scheduleKind: 'cron',
      cronExpr: '0 9 * * *',
    });
  });

  it('shows an interval in minutes, which is what people type', () => {
    const result = toJobForm(
      job({ schedule: { kind: 'every', everyMs: 900_000 } }),
      TZ,
    );
    expect(result).toMatchObject({ scheduleKind: 'every', everyMinutes: '15' });
  });

  it('reads a one-shot as a wall clock in the install zone, not the browser′s', () => {
    // 06:30Z is 09:30 in Kyiv in January. A `Date`-based conversion would render
    // whatever the machine running this is set to, which is the whole bug.
    const atMs = Date.parse('2026-01-15T06:30:00Z');
    expect(toJobForm(job({ schedule: { kind: 'at', atMs } }), TZ).at).toBe(
      '2026-01-15T08:30',
    );
    expect(
      toJobForm(job({ schedule: { kind: 'at', atMs } }), 'Asia/Tokyo').at,
    ).toBe('2026-01-15T15:30');
  });

  it('reads a heartbeat payload into the file and model boxes', () => {
    const result = toJobForm(
      job({
        payload: {
          kind: 'heartbeat',
          file: 'NOTES.md',
          model: 'tiny',
          deliver: false,
          targets: {},
        },
      }),
      TZ,
    );
    expect(result).toMatchObject({
      payloadKind: 'heartbeat',
      file: 'NOTES.md',
      model: 'tiny',
    });
  });

  it('leaves the boxes for the other kind at their defaults rather than blank', () => {
    // Switching kind in the editor must not land on an empty required field
    // that the operator never saw.
    const result = toJobForm(
      job({ schedule: { kind: 'every', everyMs: 60_000 } }),
      TZ,
    );
    expect(result.cronExpr).toBe(emptyJobForm(TZ).cronExpr);
  });
});

describe('toJobRequest', () => {
  it('converts minutes to milliseconds once, on save', () => {
    const result = toJobRequest(
      form({
        name: 'x',
        scheduleKind: 'every',
        everyMinutes: '15',
        message: 'go',
      }),
      t,
      TZ,
    );
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.create.schedule).toEqual({ kind: 'every', everyMs: 900_000 });
  });

  it('never emits a field belonging to another schedule kind', () => {
    // The schema refuses `{kind: 'cron', atMs: 5}`, and so must the form —
    // otherwise a save fails with a validation error the operator cannot see
    // the cause of.
    const result = toJobRequest(
      form({ name: 'x', scheduleKind: 'cron', message: 'go' }),
      t,
      TZ,
    );
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(Object.keys(result.create.schedule).sort()).toEqual([
      'expr',
      'kind',
    ]);
  });

  it('never emits a per-job timezone, which the schema no longer accepts', () => {
    // `CronScheduleSchema` is strict, so a stray `tz` is a 422 rather than a
    // key the server ignores. One install-wide zone is the whole design.
    const result = toJobRequest(form({ name: 'x', message: 'go' }), t, TZ);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.create.schedule).not.toHaveProperty('tz');
  });

  it('reads the one-shot field as a wall clock in the install zone', () => {
    const result = toJobRequest(
      form({
        name: 'x',
        scheduleKind: 'at',
        at: '2026-01-15T08:30',
        message: 'go',
      }),
      t,
      TZ,
    );
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.create.schedule).toEqual({
      kind: 'at',
      atMs: Date.parse('2026-01-15T06:30:00Z'),
    });
  });

  it('round-trips a one-shot through the form without moving it', () => {
    const atMs = Date.parse('2026-06-15T06:30:00Z');
    const original = job({ schedule: { kind: 'at', atMs } });
    const result = toJobRequest(toJobForm(original, TZ), t, TZ);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.create.schedule).toEqual({ kind: 'at', atMs });
  });

  it('refuses a wall-clock time the zone skipped over a spring-forward', () => {
    // Kyiv goes 03:00 → 04:00 on 2026-03-29, so 03:30 never happens. Booking an
    // hour away from where the operator pointed is worse than saying so.
    const result = toJobRequest(
      form({
        name: 'x',
        scheduleKind: 'at',
        at: '2026-03-29T03:30',
        message: 'go',
      }),
      t,
      TZ,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.errors.at).toBeDefined();
  });

  it('refuses a cron expression that is not five fields, before the round trip', () => {
    const result = toJobRequest(
      form({ name: 'x', cronExpr: '0 9 *', message: 'go' }),
      t,
      TZ,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.errors.cronExpr).toMatch(/five fields/u);
  });

  it('leaves anything past the field count to the server', () => {
    // The shape check is deliberately shallow: the real parser is Rust and the
    // browser cannot call it. `99 * * * *` is five fields and nonsense, and the
    // server's 422 is what says so.
    expect(
      toJobRequest(
        form({ name: 'x', cronExpr: '99 * * * *', message: 'go' }),
        t,
        TZ,
      ).ok,
    ).toBe(true);
  });

  it('requires a name, a message and a task file where each applies', () => {
    expect(toJobRequest(form({ message: 'go' }), t, TZ).ok).toBe(false);

    const noMessage = toJobRequest(form({ name: 'x' }), t, TZ);
    expect(noMessage.ok).toBe(false);
    if (noMessage.ok) return;
    expect(noMessage.errors.message).toBeDefined();

    const noFile = toJobRequest(
      form({ name: 'x', payloadKind: 'heartbeat', file: ' ' }),
      t,
      TZ,
    );
    expect(noFile.ok).toBe(false);
  });

  it('refuses delivery with no channel, rather than saving a job that cannot deliver', () => {
    const result = toJobRequest(
      form({ name: 'x', message: 'go', deliver: true }),
      t,
      TZ,
    );
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.errors.channel).toBeDefined();
  });

  it('omits every optional string it was given empty', () => {
    const result = toJobRequest(form({ name: 'x', message: 'go' }), t, TZ);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    for (const key of [
      'agentId',
      'workspaceId',
      'sessionKey',
      'channel',
      'to',
    ]) {
      expect(result.create.payload).not.toHaveProperty(key);
    }
  });

  it('carries a workspace onto both bodies', () => {
    // Create and update are built as one object precisely so a field cannot be
    // added to one and forgotten on the other.
    const result = toJobRequest(
      form({ name: 'x', message: 'go', workspaceId: 'research' }),
      t,
      TZ,
    );
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.create.payload).toMatchObject({ workspaceId: 'research' });
    expect(result.update.payload).toMatchObject({ workspaceId: 'research' });
  });

  it('round-trips a job unchanged through the form', () => {
    const original = job({ schedule: { kind: 'every', everyMs: 300_000 } });
    const result = toJobRequest(toJobForm(original, TZ), t, TZ);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.create.schedule).toEqual(original.schedule);
    expect(result.create.payload).toEqual(original.payload);
    expect(result.create.name).toBe(original.name);
  });
});

describe('describeSchedule', () => {
  it('says each kind in a way a list row can carry', () => {
    expect(
      describeSchedule({ kind: 'every', everyMs: 60_000 }, t, 'en', TZ),
    ).toBe('Every minute');
    expect(
      describeSchedule({ kind: 'every', everyMs: 300_000 }, t, 'en', TZ),
    ).toBe('Every 5 minutes');
    expect(
      describeSchedule({ kind: 'cron', expr: '0 9 * * *' }, t, 'en', TZ),
    ).toBe('0 9 * * *');
  });

  it('renders a one-shot in the install zone, with the zone named', () => {
    // The clock is 12-hour because `en` is, and that is the locale's business
    // rather than this function's — hence `03:30 PM` and not `15:30`.
    //
    // The zone *label* is the point of the case. Once an install can render in
    // a zone that is not the reader's own, a bare `08:30` is a number they will
    // assume is theirs. Asserted as "some label is present and the two zones
    // disagree" rather than by pinning `GMT+9`, which is CLDR data and not ours.
    const atMs = Date.parse('2026-01-15T06:30:00Z');

    const kyiv = describeSchedule({ kind: 'at', atMs }, t, 'en', TZ);
    expect(kyiv).toMatch(/^Once, at /u);
    expect(kyiv).toContain('08:30');

    const tokyo = describeSchedule({ kind: 'at', atMs }, t, 'en', 'Asia/Tokyo');
    expect(tokyo).toContain('03:30');
    expect(tokyo).not.toBe(kyiv);
    // Whatever the abbreviation is, there is one — the same instant cannot read
    // as two different clock times with nothing on screen to explain it.
    expect(tokyo).toMatch(/GMT|UTC|[A-Z]{3,5}$/u);
  });
});
