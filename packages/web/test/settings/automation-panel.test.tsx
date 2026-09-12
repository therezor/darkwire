/**
 * Settings → Automation, through the real router.
 *
 * The cases that matter are the two this panel could get wrong without looking
 * wrong: that it saves **only** the `scheduler` branch — `ConfigPatch` is a
 * deep-partial precisely so one panel's save does not rewrite another's fields
 * — and that it points at Appearance for the timezone, which used to live here
 * and is now the install's one zone rather than a scheduler knob.
 *
 * The jobs are not asserted here because they are not here: they are a page,
 * covered by `automation/automation.test.tsx`.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { ConfigSchema } from '@ghostwire/protocol';

import { Providers } from '@/app/providers.js';
import { createAppRouter } from '@/app/router.js';
import {
  stubApi,
  testQueryClient,
  type RecordedRequest,
  type StubRoute,
} from '@testkit/render.js';
import { STATUS } from '@testkit/fixtures.js';

const SETTINGS = { config: ConfigSchema.parse({}), credentialsPresent: {} };

const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
  '/api/setup': [200, { required: false }],
  '/api/status': [200, STATUS],
  '/api/sessions': [200, { sessions: [], total: 0 }],
  '/api/notifications': [200, { notifications: [], unreadCount: 0, total: 0 }],
};

function mount(): {
  readonly user: ReturnType<typeof userEvent.setup>;
  readonly calls: RecordedRequest[];
} {
  const calls = stubApi({
    ...SHELL_ROUTES,
    '/api/settings': [200, SETTINGS],
    'PATCH /api/settings': [200, SETTINGS],
  });

  const user = userEvent.setup();
  const router = createAppRouter();
  router.update({
    history: createMemoryHistory({
      initialEntries: ['/settings?panel=automation'],
    }),
  });
  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );

  return { user, calls };
}

const patchesOf = (calls: RecordedRequest[]): RecordedRequest[] =>
  calls.filter((call) => call.method === 'PATCH');

describe('the Automation panel', () => {
  it('holds the engine and not the jobs', async () => {
    mount();

    expect(await screen.findByLabelText('Concurrent runs')).toBeInTheDocument();
    // The jobs are a page, not a panel — there is no list and no create here.
    expect(
      screen.queryByRole('list', { name: 'Automation' }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'New job' }),
    ).not.toBeInTheDocument();
  });

  it('sends the operator to Appearance for the timezone instead of dropping it', async () => {
    // The knob moved rather than went away. Someone who remembers it being here
    // must be told where it went — a silent gap reads as a removed feature.
    mount();
    await screen.findByLabelText('Concurrent runs');
    expect(screen.queryByLabelText('Default timezone')).not.toBeInTheDocument();
    expect(
      screen.getByText(/timezone moved to Settings . Appearance/u),
    ).toBeInTheDocument();
  });

  it('saves one scheduler patch and nothing else', async () => {
    const { user, calls } = mount();

    const concurrency = await screen.findByLabelText('Concurrent runs');
    await user.clear(concurrency);
    await user.type(concurrency, '4');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    const patch = patchesOf(calls)[0]?.body as {
      scheduler?: Record<string, unknown>;
    };
    expect(patch.scheduler).toMatchObject({ concurrency: 4 });
    // Gone from this branch entirely — it is `ui.timezone` now, and a scheduler
    // patch that still carried it would fail to parse.
    expect(patch.scheduler).not.toHaveProperty('timezone');
    expect(Object.keys(patch)).toEqual(['scheduler']);
  });

  it('carries every knob in the save, not just the one that moved', async () => {
    // The patch replaces the `scheduler` branch wholesale, so a save that
    // mentioned only the edited field would reset the others to their defaults.
    const { user, calls } = mount();

    await user.click(await screen.findByLabelText('Catch up on boot'));
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    expect(
      (patchesOf(calls)[0]?.body as { scheduler: Record<string, unknown> })
        .scheduler,
    ).toEqual({
      enabled: true,
      catchUpOnBoot: false,
      concurrency: 2,
      runRetention: 200,
    });
  });

  it('refuses a concurrency below one before it reaches the wire', async () => {
    const { user, calls } = mount();

    const concurrency = await screen.findByLabelText('Concurrent runs');
    await user.clear(concurrency);
    await user.type(concurrency, '0');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Must be at least 1',
    );
    expect(patchesOf(calls)).toHaveLength(0);
  });

  it('reverts to what was stored', async () => {
    const { user, calls } = mount();

    const retention = await screen.findByLabelText('Runs kept per job');
    await user.clear(retention);
    await user.type(retention, '5');
    await user.click(screen.getByRole('button', { name: 'Revert' }));

    expect(await screen.findByLabelText('Runs kept per job')).toHaveValue(
      '200',
    );
    expect(patchesOf(calls)).toHaveLength(0);
  });
});
