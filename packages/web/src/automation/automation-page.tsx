/**
 * The scheduled jobs index.
 *
 * A page rather than a settings panel, and built out of the same chrome as
 * Agents, Files and Workspaces — the same `page page--wide` container, the same
 * `cluster page__header`, the same `list-toolbar` with `SearchFilter` and
 * `ListSort`, the same `DataList` rows and the same kebab. CRUD screens that
 * merely *look* alike drift the moment one is touched; ones that share the
 * components cannot.
 *
 * **The engine's own knobs are not here.** Whether the scheduler runs at all,
 * how many runs at once, how much history to keep — those are install-wide
 * settings and live in Settings → Automation, and the zone every schedule is
 * read and rendered in lives in Settings → Appearance. This page is the jobs,
 * which are content rather than configuration; the same split Agents makes,
 * where the agents are a page and only install-wide tool settings sit in
 * Settings.
 *
 * **What a row reports is where the last run landed, plus when the next one is
 * due.** Not whether one is in flight: keeping that honest would mean polling,
 * and "running" is exactly the transient state the e2e rule says not to build a
 * UI around. Status is a word in a badge — colour alone is the one encoding
 * some readers do not receive.
 */

import { Link, useNavigate } from '@tanstack/react-router';
import {
  CalendarClock,
  Copy,
  Pencil,
  Play,
  Plus,
  Power,
  PowerOff,
  Trash2,
} from 'lucide-react';
import { useCallback, useMemo, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { AutomationJob, RunStatus } from '@ghostwire/protocol';

import { Badge } from '@/components/ui/badge.js';
import { Button } from '@/components/ui/button.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { SearchFilter } from '@/components/ui/search-filter.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { DataList, DataListRow } from '@/components/crud/data-list.js';
import { ListSort } from '@/components/crud/list-sort.js';
import { Pagination } from '@/components/crud/pagination.js';
import { useListPage } from '@/components/crud/use-list-page.js';
import { RowActions } from '@/components/crud/row-actions.js';
import type { Comparators } from '@/components/crud/sort.js';
import { useAppLocale } from '@/i18n/i18n-context.js';
import { useFormat } from '@/lib/use-format.js';
import { useAppTimezone } from '@/timezone/timezone-context.js';
import { describeSchedule } from './job-form.js';
import {
  useAutomationJobs,
  useCreateJob,
  useRemoveJob,
  useRunJob,
  useSaveJob,
} from './use-automation.js';

type SortKey = 'name' | 'schedule' | 'status' | 'nextRun';

/** Every text column reads from A; the next-run column reads soonest first. */
const ASCENDING_FIRST: readonly SortKey[] = [
  'name',
  'schedule',
  'status',
  'nextRun',
];

/** Tones chosen so the word carries the meaning and the colour only agrees. */
const STATUS_TONE: Readonly<
  Record<RunStatus, 'neutral' | 'success' | 'warning' | 'danger'>
> = {
  pending: 'neutral',
  ok: 'success',
  skipped: 'neutral',
  error: 'danger',
};

/**
 * Built per render rather than declared once, because the schedule column sorts
 * on the text the reader can see — and that text is now translated and rendered
 * in the install's zone, neither of which a module constant can reach.
 */
function comparators(
  describe: (job: AutomationJob) => string,
): Comparators<AutomationJob, SortKey> {
  return {
    name: (a, b) => a.name.localeCompare(b.name),
    schedule: (a, b) => describe(a).localeCompare(describe(b)),
    status: (a, b) => a.state.lastStatus.localeCompare(b.state.lastStatus),
    // Unscheduled sorts last in both directions rather than as zero, which would
    // put every switched-off job at the top of "soonest first".
    nextRun: (a, b) =>
      (a.state.nextRunAtMs === 0
        ? Number.MAX_SAFE_INTEGER
        : a.state.nextRunAtMs) -
      (b.state.nextRunAtMs === 0
        ? Number.MAX_SAFE_INTEGER
        : b.state.nextRunAtMs),
  };
}

/**
 * `describeSchedule` with this render's language and zone already bound.
 *
 * One hook rather than four arguments at each of the three call sites, and it
 * keeps the row and the sort comparator using the same string by construction —
 * a list sorted on text the rows do not show is a list that looks unsorted.
 */
function useDescribeSchedule(): (job: AutomationJob) => string {
  const { t } = useTranslation();
  const { resolved } = useAppLocale();
  const timeZone = useAppTimezone();
  return useCallback(
    (job: AutomationJob) =>
      describeSchedule(job.schedule, t, resolved, timeZone),
    [t, resolved, timeZone],
  );
}

function JobRow({ job }: { readonly job: AutomationJob }): JSX.Element {
  const { t } = useTranslation();
  const format = useFormat();
  const describe = useDescribeSchedule();
  const navigate = useNavigate();
  const [pendingDelete, setPendingDelete] = useState(false);
  const save = useSaveJob(job.id);
  const remove = useRemoveJob();
  const run = useRunJob();
  const create = useCreateJob();

  /**
   * A copy, under a name that does not collide.
   *
   * Created **switched off**, whatever the original was: two jobs on one
   * schedule is almost never what a duplicate is for, and the copy firing
   * alongside its source before anyone has edited it is the surprising half.
   */
  const duplicate = (): void => {
    create.mutate({
      name: t('automation.copyOf', { name: job.name }),
      schedule: job.schedule,
      payload: job.payload,
      enabled: false,
      deleteAfterRun: job.deleteAfterRun,
    });
  };

  return (
    <>
      <DataListRow
        primary={
          <Link
            to="/automation/$jobId"
            params={{ jobId: job.id }}
            className="data-list__open"
            aria-label={t('automation.editAria', { name: job.name })}
          >
            <CalendarClock />
            <span className="truncate">{job.name}</span>
          </Link>
        }
        meta={
          <>
            <span className="data-list__code">{describe(job)}</span>
            <Badge tone={STATUS_TONE[job.state.lastStatus]}>
              {t(`automation.status.${job.state.lastStatus}`)}
            </Badge>
            <Badge tone={job.enabled ? 'success' : 'neutral'}>
              {job.enabled ? t('automation.enabled') : t('automation.disabled')}
            </Badge>
            {/* The engine's own answer to "when", rather than anything this
                page computed — which is what makes it worth showing at all. */}
            <span>
              {job.state.nextRunAtMs > 0
                ? t('automation.nextRun', {
                    when: format.dateTime(job.state.nextRunAtMs),
                  })
                : t('automation.notScheduled')}
            </span>
          </>
        }
        actions={
          <RowActions label={job.name}>
            <DropdownMenuItem
              onSelect={() => {
                void navigate({
                  to: '/automation/$jobId',
                  params: { jobId: job.id },
                });
              }}
            >
              <Pencil />
              {t('automation.edit')}
            </DropdownMenuItem>
            <DropdownMenuItem
              disabled={run.pending}
              onSelect={() => {
                run.mutate(job.id);
              }}
            >
              <Play />
              {t('automation.runNow')}
            </DropdownMenuItem>
            <DropdownMenuItem
              disabled={create.pending}
              onSelect={() => {
                duplicate();
              }}
            >
              <Copy />
              {t('automation.duplicate')}
            </DropdownMenuItem>
            {/* Reversible where Delete is not, so it does not ask: switching it
                back on is the same click. */}
            <DropdownMenuItem
              disabled={save.pending}
              onSelect={() => {
                save.mutate({ enabled: !job.enabled });
              }}
            >
              {job.enabled ? <PowerOff /> : <Power />}
              {job.enabled ? t('automation.disable') : t('automation.enable')}
            </DropdownMenuItem>
            <DropdownMenuItem
              className="menu__item--danger"
              onSelect={() => {
                setPendingDelete(true);
              }}
            >
              <Trash2 />
              {t('automation.delete')}
            </DropdownMenuItem>
          </RowActions>
        }
      />

      <ConfirmDialog
        open={pendingDelete}
        onOpenChange={setPendingDelete}
        title={t('automation.deleteTitle')}
        description={t('automation.deleteHint', { name: job.name })}
        confirmLabel={t('automation.delete')}
        pending={remove.pending}
        onConfirm={() => {
          // Closed on success, not on the press: a delete that failed should
          // leave the question on screen with its error.
          remove.mutate(job.id, {
            onSuccess: () => {
              setPendingDelete(false);
            },
          });
        }}
      />
    </>
  );
}

export function AutomationRoute(): JSX.Element {
  const { t } = useTranslation();
  const describe = useDescribeSchedule();

  const jobs = useAutomationJobs();

  // Both memoised because `useListPage` compares them: `jobs.data?.jobs ?? []`
  // is a fresh array on every render, and the comparators are built from a
  // formatter that changes with the locale.
  const all = useMemo(() => jobs.data?.jobs ?? [], [jobs.data]);
  const compare = useMemo(() => comparators(describe), [describe]);

  // The job list arrives whole — `automation.list` is unpaged — so the page is
  // a slice of what is already here rather than a second request. The run
  // history inside a job is the one that pages on the server, because a job on
  // a five-minute schedule outgrows any single response.
  const { filter, setFilter, sort, setSort, matched, pagination, rows } =
    useListPage({
      rows: all,
      initialSort: { key: 'nextRun', descending: false },
      haystack: (job) => `${job.name} ${job.payload.kind}`,
      comparators: compare,
      tiebreak: (a, b) => a.name.localeCompare(b.name),
    });

  return (
    <div className="stack page page--wide">
      <div className="cluster page__header">
        <h1 className="page__title">{t('automation.title')}</h1>
        <span className="spacer" />
        {/* A link, not a dialog: creating a job is the same form as editing
            one, and nothing is written until it is saved. */}
        <Button asChild>
          <Link to="/automation/new">
            <Plus />
            {t('automation.newJob')}
          </Link>
        </Button>
      </div>

      <p className="page__note">{t('automation.note')}</p>

      <div className="row list-toolbar">
        <SearchFilter
          value={filter}
          label={t('automation.filter')}
          onValueChange={setFilter}
        />
        <ListSort
          options={[
            { key: 'nextRun', label: t('automation.nextRunColumn') },
            { key: 'name', label: t('common.name') },
            { key: 'schedule', label: t('automation.scheduleColumn') },
            { key: 'status', label: t('common.status') },
          ]}
          sort={sort}
          ascendingFirst={ASCENDING_FIRST}
          onChange={setSort}
        />
      </div>

      {jobs.isPending && (
        <p className="page__note">{t('automation.loading')}</p>
      )}
      {jobs.isError && (
        <p role="alert" className="page__error">
          {t('automation.loadError', { message: jobs.error.message })}
        </p>
      )}

      {jobs.isSuccess &&
        (matched.length === 0 ? (
          <p className="page__note">
            {filter === ''
              ? t('automation.none')
              : t('automation.noMatch', { filter })}
          </p>
        ) : (
          <DataList label={t('automation.title')}>
            {rows.map((job) => (
              <JobRow key={job.id} job={job} />
            ))}
          </DataList>
        ))}

      {jobs.isSuccess && (
        <Pagination
          pagination={pagination}
          total={matched.length}
          label={t('automation.title')}
        />
      )}
    </div>
  );
}
