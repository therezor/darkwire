/**
 * Settings → Appearance, through the real router.
 *
 * The first test this panel has ever had, which is worth saying: the theme and
 * the language have shipped untested here since the panel was written, and the
 * timezone would have joined them. What the cases below hold are the three
 * things that make the timezone control correct rather than merely present.
 *
 * **It saves a concrete zone, never the `system` sentinel.** That is the whole
 * reason `SYSTEM_TZ` is not a storable value: the server resolves a rule to the
 * *host* zone and a browser resolves it to the *reader's*, so storing the rule
 * would recreate the two-answers problem one install-wide zone exists to end.
 *
 * **It patches only `ui`.** `ConfigPatch` is deep-partial precisely so one
 * panel's save does not rewrite another's fields.
 *
 * **The theme still saves nothing.** It is `localStorage` and the DOM, and a
 * PATCH from that select would mean the config had grown a field it has not.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor, within } from '@testing-library/react';
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
      initialEntries: ['/settings?panel=appearance'],
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

/** Opens a Radix select and picks an option by its visible label. */
async function choose(
  user: ReturnType<typeof userEvent.setup>,
  label: string,
  option: string | RegExp,
): Promise<void> {
  await user.click(select(label));
  await user.click(await screen.findByRole('option', { name: option }));
}

/**
 * The select trigger by its accessible name.
 *
 * `getByLabelText` is ambiguous here: `Section` stamps its title as an
 * `aria-label`, so the Language section and the Language field answer to the
 * same string. Asking for the `combobox` role names the control rather than
 * whatever else happens to carry the word.
 */
const select = (name: string): HTMLElement =>
  screen.getByRole('combobox', { name });

/**
 * The `Date and time` section, as a query scope.
 *
 * There are two `SaveBar`s on this panel now — the language's and the
 * timezone's — and both spell their button "Save changes". `Section` renders a
 * `<section aria-label>`, which is a `region`, so scoping by its title is the
 * one query that says *which* save is meant.
 */
function timeSection(): HTMLElement {
  return screen.getByRole('region', { name: 'Date and time' });
}

describe('the Appearance panel', () => {
  it('offers the three preferences an install looks and reads like', async () => {
    mount();

    await screen.findByRole('combobox', { name: 'Language' });
    expect(select('Language')).toBeInTheDocument();
    expect(select('Timezone')).toBeInTheDocument();
    expect(select('Colour scheme')).toBeInTheDocument();
  });

  it('shows the stored zone rather than whatever the browser is set to', async () => {
    // Settled rather than immediate: the panel renders before `GET /api/settings`
    // answers, and until it does the select shows this browser's zone. That is
    // the intended fallback — what matters is that the install's answer wins
    // once it arrives, and does not stay stuck on what the browser said first.
    mount();
    await screen.findByRole('combobox', { name: 'Timezone' });
    await waitFor(() => {
      expect(
        screen.getByRole('combobox', { name: 'Timezone' }),
      ).toHaveTextContent('UTC');
    });
  });

  it('saves the chosen zone into `ui` and nothing else', async () => {
    const { user, calls } = mount();

    await screen.findByRole('combobox', { name: 'Timezone' });
    await choose(user, 'Timezone', 'Asia/Tokyo');
    await user.click(
      within(timeSection()).getByRole('button', { name: 'Save changes' }),
    );

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    const patch = patchesOf(calls)[0]?.body as Record<string, unknown>;
    expect(patch).toEqual({ ui: { timezone: 'Asia/Tokyo' } });
  });

  it('resolves `System` to a real zone rather than storing the rule', async () => {
    // The case the whole design depends on. A stored `system` would be resolved
    // by the server to the host zone and by a browser to the reader's — two
    // answers to "whose clock", which is what this setting exists to collapse.
    const { user, calls } = mount();

    await screen.findByRole('combobox', { name: 'Timezone' });
    await choose(user, 'Timezone', /^System \(/u);
    await user.click(
      within(timeSection()).getByRole('button', { name: 'Save changes' }),
    );

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    const patch = patchesOf(calls)[0]?.body as { ui: { timezone: string } };
    expect(patch.ui.timezone).not.toBe('system');
    // Whatever this runtime is in, it is a zone `Intl` accepts — which is the
    // only property that matters and the only one a CI runner can promise.
    expect(
      () => new Intl.DateTimeFormat('en', { timeZone: patch.ui.timezone }),
    ).not.toThrow();
  });

  it('names the zone that `System` would resolve to, before the click', async () => {
    // An option reading only "System" would hide that the save writes a
    // concrete zone — and that the answer therefore stops following this
    // browser the moment it is stored.
    const { user } = mount();

    await user.click(await screen.findByRole('combobox', { name: 'Timezone' }));
    const system = await screen.findByRole('option', { name: /^System \(/u });
    expect(system).toHaveTextContent(/System \(.+\)/u);
  });

  it('sends nothing when the theme changes, because it is this browser′s alone', async () => {
    const { user, calls } = mount();

    await screen.findByRole('combobox', { name: 'Colour scheme' });
    await choose(user, 'Colour scheme', 'Light');

    expect(patchesOf(calls)).toHaveLength(0);
    expect(document.documentElement.dataset.theme).toBe('light');
  });

  it('reverts the zone without asking the server', async () => {
    const { user, calls } = mount();

    await screen.findByRole('combobox', { name: 'Timezone' });
    await choose(user, 'Timezone', 'Asia/Tokyo');
    await user.click(
      within(timeSection()).getByRole('button', { name: 'Revert' }),
    );

    expect(
      await screen.findByRole('combobox', { name: 'Timezone' }),
    ).toHaveTextContent('UTC');
    expect(patchesOf(calls)).toHaveLength(0);
  });
});
