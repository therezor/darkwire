/**
 * The session's background jobs, above the composer, each with a Stop.
 *
 * Fed by `session.status`, which the server sends whenever a job starts or
 * ends and on every attach, so a reload sees what is still running and nothing
 * here polls. It renders nothing while no job runs.
 */

import { Square } from 'lucide-react';
import { type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { RunningJob } from '@darkwire/protocol';

import { Button } from '@/components/ui/button.js';
import { stopJob } from '@/lib/connection.js';
import { formatDuration } from '@/lib/format.js';
import { useElapsed } from '@/lib/use-elapsed.js';
import { useTurnStore } from '@/state/turn.js';

export function JobsStrip(): JSX.Element | null {
  const { t } = useTranslation();
  const jobs = useTurnStore((state) => state.jobs);
  if (jobs.length === 0) return null;

  return (
    <section className="tasks" aria-label={t('jobs.title')}>
      <div className="tasks__summary">
        <span className="tasks__title">{t('jobs.title')}</span>
        <span className="tasks__count">{jobs.length}</span>
      </div>
      <ul className="tasks__list">
        {jobs.map((job) => (
          <JobRow key={job.id} job={job} />
        ))}
      </ul>
    </section>
  );
}

function JobRow({ job }: { readonly job: RunningJob }): JSX.Element {
  const { t } = useTranslation();
  const offsetMs = useTurnStore((state) => state.clockOffsetMs);
  // The server's start, on the server's clock, so a tab that reloaded mid-job
  // shows how long it has really been running.
  const elapsedMs = useElapsed(true, Date.now() + offsetMs - job.startedAtMs);

  return (
    <li className="tasks__item">
      <span className="jobs__label truncate">{job.label}</span>
      <span className="tasks__count">{formatDuration(elapsedMs)}</span>
      <Button
        variant="ghost"
        size="sm"
        aria-label={t('jobs.stopLabel', { label: job.label })}
        onClick={() => {
          stopJob(job.id);
        }}
      >
        <Square aria-hidden="true" />
        {t('jobs.stop')}
      </Button>
    </li>
  );
}
