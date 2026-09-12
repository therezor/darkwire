/**
 * A scheduled job as a form, and back again.
 *
 * Pure, and every field is a `string` — the house rule from `fields.ts`, and
 * this form is the case that makes it obvious: an interval typed in minutes
 * passes through `12.` and `` on the way to `12`, and neither is a number. The
 * conversion happens once, on save.
 *
 * **Cron is validated by shape here and by the server for real.** Five
 * non-empty fields is all this checks. `parse_cron` in `ghostai-core` would
 * agree with the server exactly, and the browser cannot call it. The server
 * answers a 422 naming the field, the panel renders it,
 * and then shows the server's own `state.nextRunAtMs` as a date — which is
 * better feedback than a validity flag anyway, because it says *when*.
 */

import type { TFunction } from 'i18next';
import {
  formatDateTime,
  instantFromZonedInput,
  zonedInputValue,
} from '@ghostwire/i18n';
import type {
  AutomationJob,
  AutomationSchedule,
  CreateAutomationJob,
  UpdateAutomationJob,
} from '@ghostwire/protocol';

import { parseNumber } from '@/components/form/fields.js';

export type ScheduleKind = AutomationSchedule['kind'];
export type PayloadKind = AutomationJob['payload']['kind'];

/** Every field a string, converted once on save. */
export interface JobForm {
  readonly name: string;
  readonly enabled: boolean;
  readonly deleteAfterRun: boolean;
  readonly scheduleKind: ScheduleKind;
  /** `datetime-local` value, for an `at` schedule. */
  readonly at: string;
  /** Minutes, for an `every` schedule. Minutes rather than ms — nobody types 300000. */
  readonly everyMinutes: string;
  readonly cronExpr: string;
  readonly payloadKind: PayloadKind;
  readonly message: string;
  readonly file: string;
  readonly model: string;
  readonly agentId: string;
  readonly workspaceId: string;
  readonly sessionKey: string;
  readonly deliver: boolean;
  readonly channel: string;
  readonly to: string;
}

type JobFormResult =
  | {
      readonly ok: true;
      readonly create: CreateAutomationJob;
      readonly update: UpdateAutomationJob;
    }
  | { readonly ok: false; readonly errors: Readonly<Record<string, string>> };

const MINUTE_MS = 60_000;

/**
 * The install's zone, threaded in rather than read here.
 *
 * These functions are pure and stay pure — the zone lives in a React context,
 * and reaching for it from a module function would make every one of them
 * untestable without mounting a provider. Callers pass `useAppTimezone()`.
 */
export function emptyJobForm(timeZone: string, nowMs = Date.now()): JobForm {
  return {
    name: '',
    enabled: true,
    deleteAfterRun: false,
    scheduleKind: 'cron',
    at: zonedInputValue(nowMs + 60 * MINUTE_MS, timeZone),
    everyMinutes: '60',
    cronExpr: '0 9 * * *',
    payloadKind: 'scheduled',
    message: '',
    file: 'TASK.md',
    model: '',
    agentId: '',
    workspaceId: '',
    sessionKey: '',
    deliver: false,
    channel: '',
    to: '',
  };
}

export function toJobForm(job: AutomationJob, timeZone: string): JobForm {
  const empty = emptyJobForm(timeZone);
  const { schedule, payload } = job;

  return {
    ...empty,
    name: job.name,
    enabled: job.enabled,
    deleteAfterRun: job.deleteAfterRun,
    scheduleKind: schedule.kind,
    ...(schedule.kind === 'at'
      ? { at: zonedInputValue(schedule.atMs, timeZone) }
      : {}),
    ...(schedule.kind === 'every'
      ? { everyMinutes: String(schedule.everyMs / MINUTE_MS) }
      : {}),
    ...(schedule.kind === 'cron' ? { cronExpr: schedule.expr } : {}),
    payloadKind: payload.kind,
    ...(payload.kind === 'scheduled' ? { message: payload.message } : {}),
    ...(payload.kind === 'heartbeat'
      ? { file: payload.file, model: payload.model ?? '' }
      : {}),
    agentId: payload.agentId ?? '',
    workspaceId: payload.workspaceId ?? '',
    sessionKey: payload.sessionKey ?? '',
    deliver: payload.deliver,
    channel: payload.channel ?? '',
    to: payload.to ?? '',
  };
}

function buildSchedule(
  form: JobForm,
  t: TFunction,
  timeZone: string,
):
  | { readonly ok: true; readonly schedule: AutomationSchedule }
  | { readonly ok: false; readonly errors: Record<string, string> } {
  if (form.scheduleKind === 'at') {
    // Read as a wall clock in the install's zone, not the browser's. A bare
    // `Date.parse` on a `datetime-local` value means the browser's, silently —
    // so the field would mean one thing and the row it renders back another.
    const atMs = instantFromZonedInput(form.at, timeZone);
    if (atMs === null) {
      // `null` covers both "not a datetime" and "a wall-clock time this zone
      // skipped over a spring-forward". The second is why this is not a
      // `Number.isNaN` check: that time never happened, and booking an hour
      // away from where the operator pointed is worse than refusing.
      return { ok: false, errors: { at: t('automation.atUnreal') } };
    }
    return { ok: true, schedule: { kind: 'at', atMs } };
  }

  if (form.scheduleKind === 'every') {
    const minutes = parseNumber(form.everyMinutes, t, {
      min: 1,
      integer: true,
    });
    if (!minutes.ok) {
      return { ok: false, errors: { everyMinutes: minutes.error } };
    }
    return {
      ok: true,
      schedule: { kind: 'every', everyMs: minutes.value * MINUTE_MS },
    };
  }

  const expr = form.cronExpr.trim();
  // Shape only — see the header. The server owns the real answer.
  const fields = expr.split(/\s+/u).filter(Boolean);
  if (fields.length !== 5) {
    return { ok: false, errors: { cronExpr: t('automation.cronFields') } };
  }
  return { ok: true, schedule: { kind: 'cron', expr } };
}

/**
 * The form as the two request bodies it can become.
 *
 * Both at once, because they are the same object minus what the server assigns
 * — building them separately is how a field gets added to create and forgotten
 * on update.
 */
export function toJobRequest(
  form: JobForm,
  t: TFunction,
  timeZone: string,
): JobFormResult {
  const errors: Record<string, string> = {};

  const name = form.name.trim();
  if (name === '') errors.name = t('settings.fields.required');

  const schedule = buildSchedule(form, t, timeZone);
  if (!schedule.ok) Object.assign(errors, schedule.errors);

  if (form.payloadKind === 'scheduled' && form.message.trim() === '') {
    errors.message = t('settings.fields.required');
  }
  if (form.payloadKind === 'heartbeat' && form.file.trim() === '') {
    errors.file = t('settings.fields.required');
  }
  // A delivery with nowhere to go is a job that looks configured and is not.
  if (form.deliver && form.channel.trim() === '') {
    errors.channel = t('settings.fields.required');
  }

  if (Object.keys(errors).length > 0 || !schedule.ok) {
    return { ok: false, errors };
  }

  const optional = {
    ...(form.agentId.trim() === '' ? {} : { agentId: form.agentId.trim() }),
    ...(form.workspaceId.trim() === ''
      ? {}
      : { workspaceId: form.workspaceId.trim() }),
    ...(form.sessionKey.trim() === ''
      ? {}
      : { sessionKey: form.sessionKey.trim() }),
    ...(form.channel.trim() === '' ? {} : { channel: form.channel.trim() }),
    ...(form.to.trim() === '' ? {} : { to: form.to.trim() }),
  };

  const payload: AutomationJob['payload'] =
    form.payloadKind === 'scheduled'
      ? {
          kind: 'scheduled',
          message: form.message.trim(),
          deliver: form.deliver,
          targets: {},
          ...optional,
        }
      : {
          kind: 'heartbeat',
          file: form.file.trim(),
          deliver: form.deliver,
          targets: {},
          ...(form.model.trim() === '' ? {} : { model: form.model.trim() }),
          ...optional,
        };

  const body = {
    name,
    schedule: schedule.schedule,
    payload,
    enabled: form.enabled,
    deleteAfterRun: form.deleteAfterRun,
  };

  return { ok: true, create: body, update: body };
}

/**
 * A schedule as one line, for the list.
 *
 * Through `t()` and the shared formatter. Building `Once, at …` and `Every N
 * minutes` as English string literals over a bare `toLocaleString()` slips past
 * `untranslated.test.ts`, because the sweep reads `.tsx` and this is a `.ts` —
 * which would leave the one line summarising every job in the list as the only
 * copy in the feature no locale could change, and the only timestamp ignoring
 * the install's zone.
 *
 * `count` on the interval case rather than a hand-written singular: i18next
 * resolves the plural through `Intl.PluralRules`, so a language with more than
 * two categories gets them without this function knowing which language it is.
 */
export function describeSchedule(
  schedule: AutomationSchedule,
  t: TFunction,
  locale: string,
  timeZone: string,
): string {
  switch (schedule.kind) {
    case 'at':
      return t('automation.scheduleOnce', {
        when: formatDateTime(schedule.atMs, locale, timeZone),
      });
    case 'every':
      return t('automation.scheduleEvery', {
        count: schedule.everyMs / MINUTE_MS,
      });
    case 'cron':
      return schedule.expr;
  }
}
