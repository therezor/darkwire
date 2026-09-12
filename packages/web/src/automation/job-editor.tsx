/**
 * Editing one scheduled job, and reading what it has done.
 *
 * A route rather than a dialog, the same shape as the provider and agent
 * editors: a schedule, a payload, a delivery block and a run history do not fit
 * in a modal, and a modal cannot use the `SaveBar` every other settings screen
 * saves with.
 *
 * **The schedule kind switches which fields exist rather than disabling them.**
 * A cron box greyed out beside an interval box is two controls claiming to
 * describe the same thing, and the schema deliberately makes that state
 * unrepresentable — `{kind: 'cron', atMs: 5}` does not parse. The form says the
 * same thing the wire does.
 *
 * The run history below is the only place a run's *outcome* is readable, and it
 * shows warnings in their own tone: a run that succeeded with a caveat —
 * delivery asked for with no channel wired, a boot that coalesced missed
 * occurrences — must not read as a failure.
 */

import { useQuery } from '@tanstack/react-query';
import { Link, useNavigate, useParams } from '@tanstack/react-router';
import { ArrowLeft, Play } from 'lucide-react';
import { useMemo, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import {
  DEFAULT_AGENT_ID,
  DEFAULT_WORKSPACE_ID,
  type AutomationJob,
  type AutomationRun,
  type RunStatus,
} from '@ghostwire/protocol';

import { Pagination } from '@/components/crud/pagination.js';
import { usePagination } from '@/components/crud/use-pagination.js';
import { Badge } from '@/components/ui/badge.js';
import { Button } from '@/components/ui/button.js';
import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { useFormat } from '@/lib/use-format.js';
import { useAppTimezone } from '@/timezone/timezone-context.js';
import {
  FieldGrid,
  SaveBar,
  Section,
  SelectField,
  SwitchRow,
  TextField,
  TextareaField,
} from '@/components/form/controls.js';

import {
  emptyJobForm,
  toJobForm,
  toJobRequest,
  type JobForm,
  type PayloadKind,
  type ScheduleKind,
} from './job-form.js';
import {
  useAutomationJobs,
  useAutomationRuns,
  useCreateJob,
  useRunJob,
  useSaveJob,
} from './use-automation.js';

const STATUS_TONE: Readonly<
  Record<RunStatus, 'neutral' | 'success' | 'warning' | 'danger'>
> = {
  pending: 'neutral',
  ok: 'success',
  skipped: 'neutral',
  error: 'danger',
};

/**
 * Creating a job, on the page that edits one.
 *
 * The same `Editor` the edit route renders, seeded from `emptyJobForm()` instead
 * of from a stored row. **Nothing is written until Save** — which is the whole
 * reason this is a route rather than the dialog it replaced. That dialog asked
 * for a name, immediately POSTed a disabled job with its message invented from
 * that name, and navigated here; abandoning the form left the invented row
 * behind, and the operator's list filled up with jobs nobody finished writing.
 */
export function JobCreateRoute(): JSX.Element {
  return <Editor />;
}

export function JobEditorRoute(): JSX.Element {
  const { t } = useTranslation();
  const { jobId } = useParams({ from: '/automation/$jobId' });
  const jobs = useAutomationJobs();

  if (jobs.isPending) {
    return <p className="page__note">{t('automation.loadingOne')}</p>;
  }
  if (jobs.isError) {
    return (
      <p role="alert" className="page__error">
        {t('automation.loadError', { message: jobs.error.message })}
      </p>
    );
  }

  const job = jobs.data.jobs.find((candidate) => candidate.id === jobId);

  // A stale link — a bookmark to a job that was deleted, or a one-shot that
  // deleted itself after firing. Saying so beats an empty form.
  if (job === undefined) {
    return (
      <div className="stack page page--wide">
        <p role="alert" className="page__error">
          {t('automation.noSuchJob', { id: jobId })}
        </p>
        <Link to="/automation" className="page__back">
          <ArrowLeft aria-hidden="true" />
          {t('automation.backToAutomation')}
        </Link>
      </div>
    );
  }

  // Remounts on a change of job, so one job's edits cannot survive into
  // another's boxes.
  return <Editor key={job.id} job={job} />;
}

/**
 * The form, in both modes.
 *
 * `job` absent is create. The mode decides exactly three things and nothing
 * else — what the form is seeded from, whether Save POSTs or PATCHes, and
 * whether the edit-only extras render (Run now, the next-run line, the run
 * history). Every field, every validation and the `SaveBar` are shared by
 * construction rather than by resemblance, which is the point: two forms that
 * merely look alike drift the first time either is touched.
 */
function Editor({ job }: { readonly job?: AutomationJob }): JSX.Element {
  const { t } = useTranslation();
  const format = useFormat();
  // The install's zone. Every wall-clock field on this page is read and written
  // in it, so the value the picker shows and the value the row reads back are
  // the same clock rather than the browser's and the server's.
  const timeZone = useAppTimezone();
  const navigate = useNavigate();
  const creating = job === undefined;
  const [form, setForm] = useState<JobForm>(() =>
    job === undefined ? emptyJobForm(timeZone) : toJobForm(job, timeZone),
  );
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>({});
  const [dirty, setDirty] = useState(false);

  // The real list, so a job cannot name an agent that does not exist. Absent —
  // a fresh install, or the query still in flight — leaves just the default,
  // which is the one agent that always resolves.
  const agents = useQuery({
    queryKey: queryKeys.agents,
    queryFn: ({ signal }) => api.agents(signal),
  });
  const agentOptions = useMemo(() => {
    const known = agents.data?.agents ?? [];
    const options = [
      { value: DEFAULT_AGENT_ID, label: t('automation.agentDefault') },
    ];
    for (const agent of known) {
      if (agent.id === DEFAULT_AGENT_ID) continue;
      options.push({
        value: agent.id,
        label: agent.label === '' ? agent.id : agent.label,
      });
    }
    // A job bound to an agent that has since been deleted keeps its row rather
    // than silently reading as the default — the binding is still stored, and
    // hiding it would make the editor lie about what will run.
    if (
      form.agentId !== '' &&
      !options.some((option) => option.value === form.agentId)
    ) {
      options.push({
        value: form.agentId,
        label: t('automation.agentMissing', { id: form.agentId }),
      });
    }
    return options;
  }, [agents.data, form.agentId, t]);

  // The same shape as the agents above, and for the same reasons — including
  // the deleted-binding branch: a job pointing at a detached workspace keeps
  // showing it, because hiding it would make the editor lie about what runs.
  const workspaces = useQuery({
    queryKey: queryKeys.workspaces,
    queryFn: ({ signal }) => api.workspaces(signal),
  });
  const workspaceOptions = useMemo(() => {
    const known = workspaces.data?.workspaces ?? [];
    const options = [
      { value: DEFAULT_WORKSPACE_ID, label: t('automation.workspaceDefault') },
    ];
    for (const workspace of known) {
      if (workspace.id === DEFAULT_WORKSPACE_ID) continue;
      options.push({
        value: workspace.id,
        label: workspace.name === '' ? workspace.id : workspace.name,
      });
    }
    if (
      form.workspaceId !== '' &&
      !options.some((option) => option.value === form.workspaceId)
    ) {
      options.push({
        value: form.workspaceId,
        label: t('automation.workspaceMissing', { id: form.workspaceId }),
      });
    }
    return options;
  }, [workspaces.data, form.workspaceId, t]);

  // Called unconditionally — hooks cannot be conditional — and inert on create:
  // `useSaveJob('')` is never invoked, and `useAutomationRuns('')` is disabled.
  const save = useSaveJob(job?.id ?? '');
  const create = useCreateJob();
  const run = useRunJob();
  // The run history is paged on the *server*, unlike every other list in this
  // app: a job on a five-minute schedule appends to it a few hundred times a
  // day, and the panel exists to be looked back through.
  //
  // Page first, total after — the request needs a page number before there is a
  // response to count. `resetOn` is the job's id, because opening a different
  // job is the only thing here that changes which rows are being paged.
  const runPage = usePagination({ resetOn: job?.id ?? '' });
  const runs = useAutomationRuns(job?.id ?? '', runPage.page);
  const runPagination = runPage.withTotal(runs.data?.total ?? 0);

  const update = <K extends keyof JobForm>(key: K, value: JobForm[K]): void => {
    setForm((current) => ({ ...current, [key]: value }));
    setDirty(true);
  };

  const onSave = (): void => {
    const result = toJobRequest(form, t, timeZone);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});

    if (job === undefined) {
      // The first write this page has made. On success, not on the press —
      // `mutate` is fire-and-forget, and navigating early lands the editor on a
      // job the cache has never seen, which is the "no such job" path.
      create.mutate(result.create, {
        onSuccess: (created) => {
          setDirty(false);
          void navigate({
            to: '/automation/$jobId',
            params: { jobId: created.id },
          });
        },
      });
      return;
    }

    save.mutate(result.update, {
      onSuccess: () => {
        setDirty(false);
      },
    });
  };

  return (
    <div className="stack page page--wide">
      <div className="cluster page__header">
        <Link to="/automation" className="page__back">
          <ArrowLeft aria-hidden="true" />
          {t('automation.backToAutomation')}
        </Link>
        <span className="spacer" />
        {/* Nothing to run until there is a job. */}
        {job !== undefined && (
          <Button
            variant="ghost"
            disabled={run.pending}
            onClick={() => {
              run.mutate(job.id);
            }}
          >
            <Play />
            {t('automation.runNow')}
          </Button>
        )}
      </div>

      <h2 className="page__title">{job?.name ?? t('automation.newTitle')}</h2>

      <Section title={t('automation.identityTitle')}>
        <FieldGrid>
          <TextField
            label={t('automation.nameLabel')}
            value={form.name}
            error={errors.name}
            onValueChange={(value) => {
              update('name', value);
            }}
          />
        </FieldGrid>
        <SwitchRow
          label={t('automation.enabledLabel')}
          hint={t('automation.enabledHint')}
          checked={form.enabled}
          onCheckedChange={(value) => {
            update('enabled', value);
          }}
        />
      </Section>

      <Section
        title={t('automation.scheduleTitle')}
        description={t('automation.scheduleDesc')}
      >
        <FieldGrid>
          <SelectField
            label={t('automation.scheduleKind')}
            value={form.scheduleKind}
            options={[
              { value: 'at', label: t('automation.kindAt') },
              { value: 'every', label: t('automation.kindEvery') },
              { value: 'cron', label: t('automation.kindCron') },
            ]}
            onValueChange={(value) => {
              update('scheduleKind', value as ScheduleKind);
            }}
          />
          {/* Only the fields the chosen kind has. The schema refuses a stray
              `atMs` on a cron schedule, and so does the form. */}
          {form.scheduleKind === 'at' && (
            <TextField
              label={t('automation.atLabel')}
              // Named for the same reason the cron hint is: a `datetime-local`
              // carries no zone of its own, so without this the operator is
              // typing a wall clock and guessing whose.
              hint={t('automation.atHint', { zone: timeZone })}
              type="datetime-local"
              value={form.at}
              error={errors.at}
              onValueChange={(value) => {
                update('at', value);
              }}
            />
          )}
          {form.scheduleKind === 'every' && (
            <TextField
              label={t('automation.everyLabel')}
              hint={t('automation.everyHint')}
              inputMode="numeric"
              value={form.everyMinutes}
              error={errors.everyMinutes}
              onValueChange={(value) => {
                update('everyMinutes', value);
              }}
            />
          )}
          {form.scheduleKind === 'cron' && (
            <TextField
              label={t('automation.cronLabel')}
              // The hint names the install's zone, because that is the clock
              // the expression is read against and there is no per-job zone to
              // override it with. Without it the field is five numbers and no
              // answer to "nine o'clock where".
              hint={t('automation.cronHint', { zone: timeZone })}
              value={form.cronExpr}
              error={errors.cronExpr}
              onValueChange={(value) => {
                update('cronExpr', value);
              }}
            />
          )}
        </FieldGrid>
        <SwitchRow
          label={t('automation.deleteAfterRun')}
          hint={t('automation.deleteAfterRunHint')}
          checked={form.deleteAfterRun}
          onCheckedChange={(value) => {
            update('deleteAfterRun', value);
          }}
        />
        {/* The engine's own answer, not one this page computed — so there is
            nothing honest to show until the job exists and it has answered. */}
        {job !== undefined && (
          <p className="settings-field__hint">
            {job.state.nextRunAtMs > 0
              ? t('automation.nextRun', {
                  when: format.dateTime(job.state.nextRunAtMs),
                })
              : t('automation.notScheduled')}
          </p>
        )}
      </Section>

      <Section
        title={t('automation.payloadTitle')}
        description={t('automation.payloadDesc')}
      >
        <FieldGrid>
          <SelectField
            label={t('automation.payloadKind')}
            value={form.payloadKind}
            options={[
              { value: 'scheduled', label: t('automation.kindScheduled') },
              { value: 'heartbeat', label: t('automation.kindHeartbeat') },
            ]}
            onValueChange={(value) => {
              update('payloadKind', value as PayloadKind);
            }}
          />
          <SelectField
            label={t('automation.agentLabel')}
            hint={t('automation.agentHint')}
            value={form.agentId === '' ? DEFAULT_AGENT_ID : form.agentId}
            options={agentOptions}
            onValueChange={(value) => {
              // The sentinel is the default agent's own id rather than an empty
              // string: a Radix select reads `''` as "nothing chosen" and shows
              // a blank trigger, and the payload's rule is that an absent
              // `agentId` means the default — so the two map onto each other.
              update('agentId', value === DEFAULT_AGENT_ID ? '' : value);
            }}
          />
          <SelectField
            label={t('automation.workspaceLabel')}
            // The caveat only when it bites. A job pinned to a session runs in
            // *that* session's workspace, because the stored row wins over
            // anything a run claims — so this field is inert, and a hint that
            // always said so is a hint nobody reads.
            hint={
              form.sessionKey.trim() === ''
                ? t('automation.workspaceHint')
                : t('automation.workspacePinned')
            }
            value={
              form.workspaceId === '' ? DEFAULT_WORKSPACE_ID : form.workspaceId
            }
            options={workspaceOptions}
            onValueChange={(value) => {
              // Same sentinel as the agent field above, for the same reason.
              update(
                'workspaceId',
                value === DEFAULT_WORKSPACE_ID ? '' : value,
              );
            }}
          />
        </FieldGrid>

        {form.payloadKind === 'scheduled' ? (
          <TextareaField
            label={t('automation.messageLabel')}
            hint={t('automation.messageHint')}
            rows={4}
            value={form.message}
            error={errors.message}
            onValueChange={(value) => {
              update('message', value);
            }}
          />
        ) : (
          <FieldGrid>
            <TextField
              label={t('automation.fileLabel')}
              hint={t('automation.fileHint')}
              value={form.file}
              error={errors.file}
              onValueChange={(value) => {
                update('file', value);
              }}
            />
            <TextField
              label={t('automation.modelLabel')}
              hint={t('automation.modelHint')}
              value={form.model}
              onValueChange={(value) => {
                update('model', value);
              }}
            />
          </FieldGrid>
        )}

        <TextField
          label={t('automation.sessionKeyLabel')}
          hint={t('automation.sessionKeyHint')}
          value={form.sessionKey}
          onValueChange={(value) => {
            update('sessionKey', value);
          }}
        />
      </Section>

      <Section
        title={t('automation.deliveryTitle')}
        description={t('automation.deliveryDesc')}
      >
        <SwitchRow
          label={t('automation.deliverLabel')}
          hint={t('automation.deliverHint')}
          checked={form.deliver}
          onCheckedChange={(value) => {
            update('deliver', value);
          }}
        />
        {form.deliver && (
          <FieldGrid>
            <TextField
              label={t('automation.channelLabel')}
              hint={t('automation.channelHint')}
              value={form.channel}
              error={errors.channel}
              onValueChange={(value) => {
                update('channel', value);
              }}
            />
            <TextField
              label={t('automation.toLabel')}
              hint={t('automation.toHint')}
              value={form.to}
              onValueChange={(value) => {
                update('to', value);
              }}
            />
          </FieldGrid>
        )}
      </Section>

      <SaveBar
        dirty={dirty}
        saving={creating ? create.pending : save.pending}
        onSave={onSave}
        onRevert={() => {
          setForm(
            job === undefined
              ? emptyJobForm(timeZone)
              : toJobForm(job, timeZone),
          );
          setErrors({});
          setDirty(false);
        }}
      />

      {/* A job that does not exist has no history, and an empty "Runs" heading
          on a create form reads as a feature that is broken rather than one
          that has not happened. */}
      {job !== undefined && (
        <Section
          title={t('automation.historyTitle')}
          description={t('automation.historyDesc')}
        >
          {runs.isPending && (
            <p className="page__note">{t('automation.loadingRuns')}</p>
          )}
          {runs.isError && (
            <p role="alert" className="page__error">
              {t('automation.loadError', { message: runs.error.message })}
            </p>
          )}
          {runs.data !== undefined &&
            (runs.data.runs.length === 0 ? (
              <p className="page__note">{t('automation.noRuns')}</p>
            ) : (
              // Named, like every `DataList` is. It is the one list on this
              // page, so an unnamed one is announced as "list" with nothing
              // saying what is in it — and there is now a pager beneath it
              // whose label has to match something.
              <ul
                className="settings-divided-list"
                aria-label={t('automation.historyTitle')}
              >
                {runs.data.runs.map((entry) => (
                  <RunItem key={entry.id} run={entry} />
                ))}
              </ul>
            ))}
          {runs.data !== undefined && (
            <Pagination
              pagination={runPagination}
              total={runs.data.total}
              label={t('automation.historyTitle')}
            />
          )}
        </Section>
      )}
    </div>
  );
}

function RunItem({ run }: { readonly run: AutomationRun }): JSX.Element {
  const { t } = useTranslation();
  const format = useFormat();

  return (
    <li className="settings-divided-list__text">
      <div className="cluster">
        <Badge tone={STATUS_TONE[run.status]}>
          {t(`automation.status.${run.status}`)}
        </Badge>
        <span className="settings-divided-list__name">
          {format.dateTime(run.startedAtMs)}
        </span>
      </div>
      {run.skipReason !== undefined && (
        <p className="settings-divided-list__detail">{run.skipReason}</p>
      )}
      {run.error !== undefined && (
        <p className="settings-field__error">{run.error}</p>
      )}
      {run.output !== undefined && run.output !== '' && (
        <p className="settings-divided-list__detail">{run.output}</p>
      )}
      {/* A caveat, in its own tone. A run that succeeded with a warning must
          not read as one that failed. */}
      {run.warnings.map((warning) => (
        <p key={warning} className="settings-divided-list__detail">
          <Badge tone="warning">{t('automation.warning')}</Badge> {warning}
        </p>
      ))}

      {/* The way in to the turn itself.
          `output` is the answer the run produced, which is the wrong thing to
          read when the question is why it produced that answer — or, for a run
          still `pending`, why it has produced nothing. The transcript is where
          the tool calls, the refusals and an approval still waiting all are, and
          until this link existed there was no route to it from anywhere: the
          session is not something anyone can guess the key of. */}
      {run.sessionKey !== undefined && run.sessionKey !== '' && (
        <Link
          to="/"
          search={{ session: run.sessionKey }}
          className="settings-divided-list__detail settings-divided-list__detail--link"
        >
          {t('automation.openSession')}
        </Link>
      )}
    </li>
  );
}
