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

const IMAGE = `sha256:${'a'.repeat(64)}`;

/**
 * One shared environment definition, as the wire carries it.
 *
 * Stated in full rather than trimmed to the fields under test:
 * `api.environments` parses the response, so a definition missing a field is
 * not a smaller fixture but a failed query.
 */
const DEFINITION = {
  schema: 'darkwire.environment/1',
  kind: 'container',
  name: 'dev',
  prompt: '',
  image: IMAGE,
  shared: true,
  runtime: 'runc',
  workdir: '/work',
  user: '1000:1000',
  caps: { drop: ['ALL'], add: [] },
  security: {
    noNewPrivileges: true,
    seccomp: 'default',
    readOnlyRoot: true,
    tmpfs: [],
    devices: [],
  },
  limits: { memoryMb: 2048, cpus: 2, pidsMax: 512, shmSizeMb: 256 },
  env: [],
};

const ENVIRONMENT = { name: 'dev', definition: DEFINITION, weakened: [] };

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

  it('shows what an operator weighs before opening one', async () => {
    mount({
      '/api/environments': [
        200,
        {
          environments: [
            { ...ENVIRONMENT, weakened: ['seccomp is unconfined'] },
          ],
        },
      ],
    });

    // By its image rather than its name: the name is also an option in the
    // warm-it form below, so a bare text match would find two.
    expect(await screen.findByText(IMAGE)).toBeInTheDocument();
    expect(screen.getByText(/2048 MB/)).toBeInTheDocument();
    // The one a list that showed names alone would hide.
    expect(
      screen.getByText(/Hardening weakened: seccomp is unconfined/),
    ).toBeInTheDocument();
  });

  it('opens the editor from the row, and offers a create', async () => {
    // The panel was read-only until the policy directory got a door: the only
    // way to author one was hand-written YAML.
    mount({ '/api/environments': [200, { environments: [ENVIRONMENT] }] });

    expect(
      await screen.findByRole('link', { name: 'Edit dev' }),
    ).toHaveAttribute('href', '/settings/environments/dev');
    expect(
      screen.getByRole('link', { name: /New environment/ }),
    ).toHaveAttribute('href', '/settings/environments/new');
  });

  it('lists a definition that did not parse, with no way to edit it', async () => {
    // A file on disk with a name and a problem. It belongs on the list because
    // deleting it is the operator's way out, and there is nothing to load into
    // a form.
    mount({
      '/api/environments': [
        200,
        {
          environments: [
            { name: 'broken', weakened: [], problem: 'image must be pinned' },
          ],
        },
      ],
    });

    expect(await screen.findByText('image must be pinned')).toBeInTheDocument();
    expect(
      screen.queryByRole('link', { name: 'Edit broken' }),
    ).not.toBeInTheDocument();
  });

  it('asks before removing one, and says what it costs', async () => {
    const { user } = mount({
      '/api/environments': [200, { environments: [ENVIRONMENT] }],
    });

    await user.click(
      await screen.findByRole('button', { name: 'Actions for dev' }),
    );
    await user.click(await screen.findByRole('menuitem', { name: /Delete/ }));

    expect(
      await screen.findByText(
        /Agents naming .dev. will refuse their next turn/,
      ),
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

  it('warms an environment with a structured request', async () => {
    const instances = {
      instances: [
        {
          id: 'dw-sbx-1',
          workspace: 'default',
          environment: 'dev',
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
