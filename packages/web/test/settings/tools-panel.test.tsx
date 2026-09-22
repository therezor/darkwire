/**
 * Settings → Tools, through the real router.
 *
 * Three things this panel could get wrong without looking wrong: the durations
 * are seconds on screen and seconds on the wire, so a number typed here means
 * what it says; the user agent's placeholder is the string the **server** says
 * it sends, not a copy that could go stale; and the SearXNG URL is only
 * collected, and only required, when that backend is chosen.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { ConfigSchema } from '@darkwire/protocol';

import { Providers } from '@/app/providers.js';
import { createAppRouter } from '@/app/router.js';
import {
  stubApi,
  testQueryClient,
  type RecordedRequest,
  type StubRoute,
} from '@testkit/render.js';
import { STATUS } from '@testkit/fixtures.js';

const DEFAULT_AGENT =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 ' +
  '(KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36';

const SETTINGS = {
  config: ConfigSchema.parse({}),
  credentialsPresent: {},
  defaultWebUserAgent: DEFAULT_AGENT,
};

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
    history: createMemoryHistory({ initialEntries: ['/settings?panel=tools'] }),
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

describe('the Tools panel', () => {
  it('shows the durations in seconds, not milliseconds', async () => {
    mount();
    // 20 seconds, not 20000. Nobody reasons about a fetch in milliseconds.
    expect(await screen.findByLabelText('Fetch timeout')).toHaveValue('20');
    expect(screen.getByLabelText('Read timeout')).toHaveValue('15');
    expect(screen.getByLabelText('Cache lifetime')).toHaveValue('900');
  });

  /**
   * The placeholder is the default an operator gets by typing nothing, so it
   * has to be what this build actually sends. Served rather than copied, which
   * is what this asserts: a hardcoded string would pass a test that built its
   * own expectation from the same constant.
   */
  it('offers the default user agent the server reports, as a placeholder', async () => {
    mount();
    const field = await screen.findByLabelText('User agent');
    expect(field).toHaveValue('');
    expect(field).toHaveAttribute('placeholder', DEFAULT_AGENT);
    // A textarea, because the placeholder is far too long to read in an input.
    expect(field.tagName).toBe('TEXTAREA');
  });

  it('asks for a URL only when SearXNG is chosen, and refuses an empty one', async () => {
    const { user, calls } = mount();
    await screen.findByLabelText('Fetch timeout');
    expect(screen.queryByLabelText('SearXNG URL')).not.toBeInTheDocument();

    await user.click(screen.getByLabelText('Search backend'));
    await user.click(await screen.findByRole('option', { name: 'SearXNG' }));

    const url = await screen.findByLabelText('SearXNG URL');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));
    // Refused before anything is sent: an instance with no URL reaches nothing,
    // and the first search is a worse place to find that out.
    expect(patchesOf(calls)).toHaveLength(0);

    await user.type(url, 'https://searx.example.test');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    const patch = patchesOf(calls)[0]?.body as {
      tools?: { web?: Record<string, unknown> };
    };
    expect(patch.tools?.web?.searchProvider).toBe('searxng');
    expect(patch.tools?.web?.searchUrl).toBe('https://searx.example.test');
  });

  it('saves the seconds it was given, not a converted number', async () => {
    const { user, calls } = mount();
    const field = await screen.findByLabelText('Fetch timeout');
    await user.clear(field);
    await user.type(field, '45');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(patchesOf(calls)).toHaveLength(1);
    });
    const patch = patchesOf(calls)[0]?.body as {
      tools?: { web?: Record<string, unknown> };
    };
    expect(patch.tools?.web?.timeoutSeconds).toBe(45);
    // One branch of the tree, so a panel's save does not rewrite another's.
    expect(Object.keys(patch)).toEqual(['tools']);
  });
});
