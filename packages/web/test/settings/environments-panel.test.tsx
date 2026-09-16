/**
 * The Environments panel, driven through the real router.
 *
 * Its own file rather than a block in `settings.test.tsx`, because it is the one
 * settings panel that reads none of `config.yaml`: it mounts on
 * `/api/environments` and `/api/sandboxes` alone, which is also why it renders
 * before the settings gate.
 *
 * The cases here are the two halves of what the panel is for. The **installed**
 * list is what an operator reads before binding an agent to one, so the fields
 * that decide that — the image, who it runs as, what was weakened — are asserted
 * rather than assumed. The **running** list is lifecycle, and the assertion is on
 * the request that goes over the wire rather than on what the screen says after.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { Providers } from '@/app/providers.js';
import { createAppRouter } from '@/app/router.js';
import {
  stubApi,
  testQueryClient,
  type RecordedRequest,
  type StubRoute,
} from '@testkit/render.js';
import { STATUS } from '@testkit/fixtures.js';

/**
 * One shared environment definition, as the wire carries it.
 *
 * Stated in full rather than trimmed to the fields under test: `api.environments`
 * parses the response, so a summary missing `runtime`, `workdir`, `user` or
 * `limits` is not a smaller fixture but a failed query.
 */
const ENVIRONMENT = {
  name: 'dev',
  kind: 'container',
  prompt: '',
  image: `sha256:${'a'.repeat(64)}`,
  shared: true,
  runtime: 'runc',
  workdir: '/work',
  user: '1000:1000',
  limits: { memoryMb: 2048, cpus: 2, pidsMax: 512, shmSizeMb: 256 },
  capsAdded: [],
  weakened: [],
};

const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
  '/api/setup': [200, { required: false }],
  '/api/status': [200, { ...STATUS, model: 'llama3', toolCount: 2 }],
  '/api/sessions': [200, { sessions: [], total: 0 }],
  '/api/notifications': [200, { notifications: [], unreadCount: 0, total: 0 }],
};

function mount(overrides: Record<string, StubRoute> = {}): {
  readonly user: ReturnType<typeof userEvent.setup>;
  readonly calls: RecordedRequest[];
} {
  const calls = stubApi({
    ...SHELL_ROUTES,
    '/api/environments': [200, { environments: [] }],
    '/api/sandboxes': [200, { instances: [] }],
    ...overrides,
  });

  const user = userEvent.setup();
  const router = createAppRouter();
  router.update({
    history: createMemoryHistory({
      initialEntries: ['/settings?panel=environments'],
    }),
  });
  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );

  return { user, calls };
}

describe('the environments panel', () => {
  it('says the host is where commands run when nothing is installed', async () => {
    // Not a blank panel. An install with no definitions is the common case, and
    // "nothing here" reads as a broken screen where "they run on the host" reads
    // as the true statement it is.
    mount();

    expect(
      await screen.findByText(/every agent runs its commands on the host/),
    ).toBeInTheDocument();
  });

  it('shows what an operator weighs before binding an agent to one', async () => {
    mount({
      '/api/environments': [
        200,
        {
          environments: [
            {
              ...ENVIRONMENT,
              capsAdded: ['SYS_PTRACE'],
              weakened: ['seccomp is unconfined'],
            },
          ],
        },
      ],
    });

    // By its image rather than its name: the name is also an option in the
    // warm-it form below, so a bare text match would find two.
    expect(await screen.findByText(ENVIRONMENT.image)).toBeInTheDocument();
    expect(screen.getByText(/runs as 1000:1000/)).toBeInTheDocument();
    expect(screen.getByText(/2048 MB/)).toBeInTheDocument();
    expect(screen.getByText(/SYS_PTRACE/)).toBeInTheDocument();
    // The one a picker that showed names alone would hide.
    expect(
      screen.getByText(/Hardening weakened: seccomp is unconfined/),
    ).toBeInTheDocument();
  });

  it('warns that an allow-list could not be enforced, before anything is saved', async () => {
    mount({
      '/api/environments': [
        200,
        {
          environments: [{ ...ENVIRONMENT, gatewayProblem: 'it runs as root' }],
        },
      ],
    });

    expect(
      await screen.findByText(/Cannot enforce an allow-list: it runs as root/),
    ).toBeInTheDocument();
  });

  it('warms a shared environment with a structured request', async () => {
    const instances = {
      instances: [
        {
          id: 'ghost-sbx-1',
          workspace: 'default',
          environment: 'dev',
          shared: true,
          busy: 0,
          lastUsedMs: 1,
          agents: ['operator'],
        },
      ],
    };
    const { user, calls } = mount({
      '/api/environments': [200, { environments: [ENVIRONMENT] }],
      '/api/sandboxes': [200, instances],
      'POST /api/sandboxes': [200, instances],
    });

    await user.selectOptions(
      await screen.findByLabelText('Environment'),
      'dev',
    );
    await user.click(screen.getByRole('button', { name: 'Start it now' }));

    await waitFor(() => {
      expect(
        calls.find(
          (call) => call.method === 'POST' && call.path === '/api/sandboxes',
        )?.body,
      ).toEqual({
        op: 'start',
        environment: 'dev',
        workspace: 'default',
        agent: 'operator',
        session: 'operator',
        // Warmed with no egress, which is also an agent's default: an
        // instance's network is part of its identity, so one warmed with a
        // network nobody asked for is a container nothing ever reuses.
        network: { mode: 'none', allow: [], hosts: [], dns: [] },
      });
    });
    expect(await screen.findByText(/Agents: operator/)).toBeInTheDocument();
  });
});
