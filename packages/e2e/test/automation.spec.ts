/**
 * Automation, in a browser, against the real stack.
 *
 * The unit suites already prove the pieces: the cron parser lands on the right
 * instant, the store pages, the engine coalesces a boot backlog, the form
 * builds the right body. What only a browser can show is that they are wired to
 * each other — that a job written in the panel reaches the store with a
 * schedule the timer can honour, and that pressing "Run now" produces a real
 * turn through the real hub against the real loop.
 *
 * **Every assertion is on durable state read back through the API.** A run's
 * `pending` badge, a spinner, a toast — all of those are the transient states
 * the repo rule was written about, and the one that broke CI four runs running.
 * The durable facts here are: the job exists with the schedule it was given,
 * and a run has *settled* into a terminal status.
 */

import type { Page } from '@playwright/test';

import { expect, test } from '../src/fixtures.js';

interface JobView {
  readonly id: string;
  readonly name: string;
  readonly enabled: boolean;
  readonly schedule: { readonly kind: string; readonly expr?: string };
  readonly payload: { readonly kind: string; readonly message?: string };
  readonly state: { readonly nextRunAtMs: number; readonly lastStatus: string };
}

interface RunView {
  readonly id: string;
  readonly status: string;
  readonly output?: string;
  readonly sessionKey?: string;
}

const jobsOf = async (app: Page, url: string): Promise<JobView[]> => {
  const response = await app.request.get(`${url}/api/automation/jobs`);
  return ((await response.json()) as { jobs: JobView[] }).jobs;
};

const runsOf = async (
  app: Page,
  url: string,
  jobId: string,
): Promise<RunView[]> => {
  const response = await app.request.get(
    `${url}/api/automation/jobs/${jobId}/runs`,
  );
  return ((await response.json()) as { runs: RunView[] }).runs;
};

/** Seeded through the API, never the UI — the spec under test is the panel. */
async function seedJob(
  app: Page,
  url: string,
  over: Record<string, unknown> = {},
): Promise<JobView> {
  const response = await app.request.post(`${url}/api/automation/jobs`, {
    data: {
      name: 'Nightly build',
      // Far enough out that the timer cannot fire it mid-spec and make the run
      // list a moving target. No `tz` — a job has no zone of its own; the
      // install's `ui.timezone` reads every expression, and sending one here is
      // a 422 rather than a key the server ignores.
      schedule: { kind: 'cron', expr: '0 9 1 1 *' },
      payload: { kind: 'scheduled', message: 'say hello' },
      ...over,
    },
  });
  return (await response.json()) as JobView;
}

const jobRow = (app: Page, name: string) =>
  app
    .getByRole('list', { name: 'Automation' })
    .getByRole('listitem')
    .filter({ hasText: name });

test('Automation is a page, reached from the sidebar before Settings', async ({
  app,
  harness,
}) => {
  await app.goto(harness.url);

  const nav = app.getByRole('navigation');
  await nav.getByRole('link', { name: 'Automation' }).click();

  await expect(app).toHaveURL(/\/automation$/u);
  await expect(app.getByRole('link', { name: 'New job' })).toBeVisible();
});

test('the engine settings are in Settings, and the jobs are not', async ({
  app,
  harness,
}) => {
  // The split: install-wide knobs are a settings panel, the jobs an operator
  // keeps are a page. Settings → Automation must hold the first and none of
  // the second.
  await app.goto(`${harness.url}/settings?panel=automation`);

  await expect(app.getByLabel('Concurrent runs')).toBeVisible();
  await expect(app.getByRole('list', { name: 'Automation' })).toHaveCount(0);
  await expect(app.getByRole('link', { name: 'New job' })).toHaveCount(0);
});

test('creating a job writes it to the store, from the create page', async ({
  app,
  harness,
}) => {
  await app.goto(`${harness.url}/automation`);

  await app.getByRole('link', { name: 'New job' }).click();
  await expect(app).toHaveURL(/\/automation\/new$/u);

  await app.getByLabel('Name').fill('Nightly build');
  await app.getByLabel('Message').fill('check the build');
  await app.getByRole('button', { name: 'Save changes' }).click();

  // Durable: the row exists in the store, with what was typed rather than with
  // anything a dialog invented.
  await expect
    .poll(async () => (await jobsOf(app, harness.url)).map((job) => job.name))
    .toEqual(['Nightly build']);
  await expect
    .poll(async () => (await jobsOf(app, harness.url))[0]?.payload.message)
    .toBe('check the build');
});

test('the install timezone reschedules an existing cron job', async ({
  app,
  harness,
}) => {
  // The behaviour the one-zone design turns on, end to end: a cron expression
  // is a wall-clock time, so its stored instant is only valid against the zone
  // it was computed in. Changing the zone in Account is therefore a
  // reschedule, not a display tweak — and `settings.patch` does it on the save
  // rather than leaving each job on a stale instant until it next fires.
  const seeded = await seedJob(app, harness.url);
  const before = seeded.state.nextRunAtMs;
  expect(before).toBeGreaterThan(0);

  await app.goto(`${harness.url}/settings?panel=account`);
  await app.getByRole('combobox', { name: 'Timezone' }).click();
  await app.getByRole('option', { name: 'Asia/Tokyo', exact: true }).click();
  await app
    .getByRole('region', { name: 'Date and time' })
    .getByRole('button', { name: 'Save changes' })
    .click();

  // The settled value, never the saving state: the config carries the new zone
  // and the job's next run has moved with it.
  await expect
    .poll(async () => (await jobsOf(app, harness.url))[0]?.state.nextRunAtMs)
    .not.toBe(before);

  // And the list renders that instant in the zone that produced it, with the
  // zone named — an unlabelled clock is the half of this that was missing.
  await app.goto(`${harness.url}/automation`);
  await expect(jobRow(app, 'Nightly build')).toContainText('GMT+9');
});

test('an abandoned create writes nothing at all', async ({ app, harness }) => {
  // The reason create is a page rather than the dialog it replaced: that dialog
  // POSTed a job with an invented message the moment it was submitted.
  await app.goto(`${harness.url}/automation/new`);
  await app.getByLabel('Name').fill('Never finished');
  await app.getByRole('link', { name: 'Back to Automation' }).click();

  await expect(app).toHaveURL(/\/automation$/u);
  expect(await jobsOf(app, harness.url)).toHaveLength(0);
});

test('editing the schedule saves it and the engine recomputes when it fires', async ({
  app,
  harness,
}) => {
  const seeded = await seedJob(app, harness.url);
  await app.goto(`${harness.url}/automation/${seeded.id}`);

  await app.getByLabel('Cron expression').fill('30 6 * * 1-5');
  await app.getByRole('button', { name: 'Save changes' }).click();

  await expect
    .poll(async () => (await jobsOf(app, harness.url))[0]?.schedule.expr)
    .toBe('30 6 * * 1-5');
  // The engine's own answer, which is what proves the parser ran server-side
  // rather than the string simply being stored.
  await expect
    .poll(async () => (await jobsOf(app, harness.url))[0]?.state.nextRunAtMs)
    .toBeGreaterThan(0);
});

test('a cron expression the server cannot honour is refused, and the job is unchanged', async ({
  app,
  harness,
}) => {
  const seeded = await seedJob(app, harness.url);
  await app.goto(`${harness.url}/automation/${seeded.id}`);

  // Five fields, so the panel's shape check passes it — and nonsense, so the
  // server's parser is the thing that refuses.
  await app.getByLabel('Cron expression').fill('99 * * * *');
  await app.getByRole('button', { name: 'Save changes' }).click();

  // The durable fact, and the only one asserted here: the refusal left the
  // stored schedule alone. The error *message* is a toast — it fades, and it is
  // announced in a live region as well as drawn, so matching its text picks up
  // two nodes and fails under load in a way it does not alone. That wording is
  // covered in `automation-panel.test.tsx`, where the state holds still.
  await expect
    .poll(async () => (await jobsOf(app, harness.url))[0]?.schedule.expr)
    .toBe('0 9 1 1 *');
});

test('running a job on demand produces a real turn that settles', async ({
  app,
  harness,
}) => {
  const seeded = await seedJob(app, harness.url);
  await app.goto(`${harness.url}/automation`);

  await jobRow(app, 'Nightly build')
    .getByRole('button', { name: /Actions for/u })
    .click();
  await app.getByRole('menuitem', { name: 'Run now' }).click();

  // The durable end state, never the `pending` it passes through: the run row
  // reaching a terminal status is what says the hub, the loop and the store
  // were all actually wired to each other.
  await expect
    .poll(async () => (await runsOf(app, harness.url, seeded.id))[0]?.status, {
      timeout: 15_000,
    })
    .toBe('ok');
  await expect
    .poll(async () => (await runsOf(app, harness.url, seeded.id))[0]?.output)
    .not.toBe('');
  // And the job's own state carries the outcome, which is what the list shows.
  await expect
    .poll(async () => (await jobsOf(app, harness.url))[0]?.state.lastStatus)
    .toBe('ok');
});

test('a job runs in the workspace it names', async ({ app, harness }) => {
  // The chain only a real composition root exercises: the payload's workspace
  // reaches `SchedulerConnectOptions`, the hub carries it into `ensureSession`,
  // and the run's session is created there rather than in the default.
  const created = await app.request.post(`${harness.url}/api/workspaces`, {
    data: { name: 'Research', id: 'research' },
  });
  expect(created.ok()).toBe(true);

  const seeded = await seedJob(app, harness.url, {
    payload: {
      kind: 'scheduled',
      message: 'say hello',
      workspaceId: 'research',
    },
  });
  await app.request.post(`${harness.url}/api/automation/jobs/${seeded.id}/run`);

  await expect
    .poll(async () => (await runsOf(app, harness.url, seeded.id))[0]?.status, {
      timeout: 15_000,
    })
    .toBe('ok');

  const key = (await runsOf(app, harness.url, seeded.id))[0]?.sessionKey ?? '';
  expect(key).not.toBe('');
  const session = await app.request.get(`${harness.url}/api/sessions/${key}`);
  expect(((await session.json()) as { workspaceId: string }).workspaceId).toBe(
    'research',
  );
});

test('a run leaves a session that is listed like any other', async ({
  app,
  harness,
}) => {
  // Listed rather than hidden. A scheduled run that goes wrong is diagnosed by
  // reading its turn, and while these were excluded from the unscoped listing
  // the run history beside the job showed the output without linking to the
  // session that produced it — so there was no way in at all.
  const seeded = await seedJob(app, harness.url);
  await app.request.post(`${harness.url}/api/automation/jobs/${seeded.id}/run`);

  await expect
    .poll(async () => (await runsOf(app, harness.url, seeded.id))[0]?.status, {
      timeout: 15_000,
    })
    .toBe('ok');

  const unscoped = await app.request.get(`${harness.url}/api/sessions`);
  const listed = (
    (await unscoped.json()) as { sessions: Array<{ origin: string }> }
  ).sessions;
  expect(listed.some((session) => session.origin === 'automation')).toBe(true);

  // Provenance survives as a column, so a caller that wants only these still
  // has one question to ask.
  const scoped = await app.request.get(
    `${harness.url}/api/sessions?origin=automation`,
  );
  const automation = ((await scoped.json()) as { sessions: unknown[] })
    .sessions;
  expect(automation.length).toBeGreaterThan(0);

  // And the run history is a way in to it. The link is the whole path from
  // "this run went wrong" to the turn that says why.
  await app.goto(`${harness.url}/automation/${seeded.id}`);
  await app.getByRole('link', { name: 'Open session' }).first().click();
  await expect(app.getByTestId('transcript')).toBeVisible();
});

/**
 * A job's history outgrows one response long before anything else here does: a
 * five-minute schedule appends ~288 runs a day, and until now the panel showed
 * the newest 50 with no way to reach the rest.
 *
 * Seeded straight into the store rather than run through the loop — 26 real
 * turns would take minutes and prove nothing this does not. What the browser is
 * being asked is whether the panel pages, and the offset it sends is exercised
 * against the real SQL either way.
 */
test('pages a run history longer than one page', async ({ app, harness }) => {
  const seeded = await seedJob(app, harness.url);
  for (let index = 0; index < 30; index += 1) {
    const started = await app.request.post(
      `${harness.url}/api/_test/automation/runs`,
      { data: { jobId: seeded.id } },
    );
    const run = (await started.json()) as { id: string };
    await app.request.post(
      `${harness.url}/api/_test/automation/runs/${run.id}/finish`,
      { data: { status: 'ok', output: `run ${String(index)}` } },
    );
  }

  await app.goto(`${harness.url}/automation/${seeded.id}`);

  const pager = app.getByRole('navigation', { name: 'Runs' });
  await expect(pager).toBeVisible();
  // The count is the durable statement that the total came from a `COUNT(*)`
  // rather than from the length of the page in front of it.
  await expect(pager).toContainText('Showing 1–25 of 30');

  const history = app.getByRole('list', { name: 'Runs' });
  await expect(history.getByRole('listitem')).toHaveCount(25);

  await pager.getByRole('button', { name: 'Next page' }).click();

  await expect(pager).toContainText('Showing 26–30 of 30');
  // Five rows on the last page rather than another full one, which is the
  // offset having reached the SQL rather than being applied to a page the
  // browser already held.
  await expect(history.getByRole('listitem')).toHaveCount(5);
});

test('deleting a job takes its history with it', async ({ app, harness }) => {
  const seeded = await seedJob(app, harness.url);
  await app.goto(`${harness.url}/automation`);

  await jobRow(app, 'Nightly build')
    .getByRole('button', { name: /Actions for/u })
    .click();
  await app.getByRole('menuitem', { name: 'Delete' }).click();
  await app.getByRole('button', { name: 'Delete' }).last().click();

  await expect
    .poll(async () => (await jobsOf(app, harness.url)).length)
    .toBe(0);
  const runs = await app.request.get(
    `${harness.url}/api/automation/jobs/${seeded.id}/runs`,
  );
  expect(runs.status()).toBe(404);
});

test('the scheduler settings save and take effect without a restart', async ({
  app,
  harness,
}) => {
  await app.goto(`${harness.url}/settings?panel=automation`);

  await app.getByLabel('Concurrent runs').fill('4');
  await app.getByRole('button', { name: 'Save changes' }).click();

  await expect
    .poll(async () => {
      const response = await app.request.get(`${harness.url}/api/settings`);
      const body = (await response.json()) as {
        config: { scheduler: { concurrency: number } };
      };
      return body.config.scheduler.concurrency;
    })
    .toBe(4);
});
