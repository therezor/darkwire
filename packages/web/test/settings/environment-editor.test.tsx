/**
 * The environment editor, driven through the real router.
 *
 * The cases here are the ones the panel beside it cannot cover: what a create
 * actually puts on the wire, and the two refusals that have to read as sentences
 * rather than as a failed request. Everything the form merely renders is left to
 * the type checker, which already fails on a key that does not resolve.
 *
 * **The server's refusals are not reimplemented here and must not be.** An image
 * that is not digest-pinned and a capability that is never grantable come back
 * from the save as one sentence each, already written for a person; a second
 * copy in the browser is how the two come to disagree.
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

const DEFINITION = {
  schema: 'darkwire.environment/1',
  kind: 'container',
  name: 'dev',
  image: IMAGE,
  runtime: 'runc',
  workdir: '/work',
  // Hand-written, and not editable on this screen. The round-trip test below
  // is the one that says a save does not replace them with defaults.
  user: '1001:1001',
  caps: { drop: ['ALL'], add: ['CHOWN'] },
  security: {
    noNewPrivileges: true,
    seccomp: 'default',
    readOnlyRoot: true,
    tmpfs: ['/tmp:rw,nosuid,size=512m', '/home/ghost:rw,size=64m'],
    devices: [],
  },
  limits: { memoryMb: 2048, cpus: 2, pidsMax: 512, shmSizeMb: 256 },
  env: ['CARGO_HOME'],
};

const ENVIRONMENT = { name: 'dev', definition: DEFINITION, weakened: [] };

const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
  '/api/setup': [200, { required: false }],
  '/api/status': [200, { ...STATUS, model: 'llama3', toolCount: 2 }],
  '/api/sessions': [200, { sessions: [], total: 0 }],
  '/api/notifications': [200, { notifications: [], unreadCount: 0, total: 0 }],
};

function mount(
  path: string,
  overrides: Record<string, StubRoute> = {},
): {
  readonly user: ReturnType<typeof userEvent.setup>;
  readonly calls: RecordedRequest[];
} {
  const calls = stubApi({
    ...SHELL_ROUTES,
    '/api/environments': [200, { environments: [ENVIRONMENT] }],
    '/api/sandboxes': [200, { instances: [] }],
    ...overrides,
  });

  const user = userEvent.setup();
  const router = createAppRouter();
  router.update({
    history: createMemoryHistory({ initialEntries: [path] }),
  });
  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );

  return { user, calls };
}

function putBody(calls: RecordedRequest[]): unknown {
  return calls.find(
    (call) =>
      call.method === 'PUT' && call.path.startsWith('/api/environments/'),
  )?.body;
}

describe('the environment editor', () => {
  it('edits the resource budget and sends the whole definition back', async () => {
    // The whole definition, not a patch: the file *is* the policy, and a partial
    // one has no meaning. A field this form dropped would be a field the save
    // erased.
    const { user, calls } = mount('/settings/environments/dev', {
      'PUT /api/environments/dev': [200, { environments: [ENVIRONMENT] }],
    });

    const memory = await screen.findByLabelText('Memory (MB)');
    await user.clear(memory);
    await user.type(memory, '4096');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(putBody(calls)).toEqual({
        ...DEFINITION,
        limits: { ...DEFINITION.limits, memoryMb: 4096 },
      });
    });
  });

  it('will not let the name be changed, because it is the filename', async () => {
    mount('/settings/environments/dev');

    expect(await screen.findByLabelText('Name')).toBeDisabled();
  });

  it('carries the fields it does not edit back untouched', async () => {
    // The data-loss trap. The screen shows four fields, but the PUT is a whole
    // definition and the server writes what it is given: a form that dropped
    // the hardening would replace a hand-written tmpfs, uid and capability with
    // schema defaults on the first save from here, and nothing would say so.
    //
    // The tmpfs entries are the sharpest case, because each one has commas
    // inside it: anything that split them the way the agent editor splits CIDRs
    // would cut one mount into three broken ones.
    const { user, calls } = mount('/settings/environments/dev', {
      'PUT /api/environments/dev': [200, { environments: [ENVIRONMENT] }],
    });

    const memory = await screen.findByLabelText('Memory (MB)');
    await user.clear(memory);
    await user.type(memory, '256');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(putBody(calls)).toMatchObject({
        user: '1001:1001',
        caps: { drop: ['ALL'], add: ['CHOWN'] },
        security: {
          tmpfs: ['/tmp:rw,nosuid,size=512m', '/home/ghost:rw,size=64m'],
        },
        env: ['CARGO_HOME'],
      });
    });
  });

  it('shows what it does not edit, rather than hiding it', async () => {
    // "What is this container actually doing" is a question this screen should
    // answer even for the fields it leaves to the file.
    const { user } = mount('/settings/environments/dev');

    await user.click(await screen.findByText('Advanced'));

    expect(screen.getByText('1001:1001')).toBeInTheDocument();
    // Both mounts, on one readout. Matched loosely because the DOM normalises
    // the newline between them into a space.
    expect(
      screen.getByText(/\/tmp:rw,nosuid,size=512m\s+\/home\/ghost:rw,size=64m/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/policy\/environments\/dev\.yaml/),
    ).toBeInTheDocument();
  });

  it('refuses a name that is not a slug, before anything is sent', async () => {
    const { user, calls } = mount('/settings/environments/new');

    const name = await screen.findByLabelText('Name');
    await user.clear(name);
    await user.type(name, 'Not A Slug');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    expect(
      await screen.findByText(/Use lower case letters, digits and dashes/),
    ).toBeInTheDocument();
    expect(putBody(calls)).toBeUndefined();
  });

  it('will not create one over a name already installed', async () => {
    // A server-side race between two operators is not what this defends
    // against; typing the name of something on the list in front of you is.
    const { user } = mount('/settings/environments/new');

    const name = await screen.findByLabelText('Name');
    await user.clear(name);
    await user.type(name, 'dev');

    expect(await screen.findByText(/already installed/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save changes' })).toBeDisabled();
  });

  it('shows the server sentence when a save is refused', async () => {
    // The one that matters most: an image that is not digest-pinned. The
    // refusal is the server's own wording, not a second copy of the rule.
    const { user, calls } = mount('/settings/environments/dev', {
      'PUT /api/environments/dev': [
        422,
        {
          error: {
            code: 'config',
            message: 'Container image must be digest-pinned',
          },
        },
      ],
    });

    const image = await screen.findByLabelText('Image');
    await user.clear(image);
    await user.type(image, 'node:20');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(putBody(calls)).toBeDefined();
    });
    // Beside the save button, not only in the toast: the sentence is about the
    // definition on screen, and reading it should not depend on catching a
    // toast before it goes.
    const alerts = await screen.findAllByRole('alert');
    expect(
      alerts.some((alert) =>
        alert.textContent.includes('must be digest-pinned'),
      ),
    ).toBe(true);
  });

  it('says so when the definition on disk does not parse', async () => {
    // There is nothing to load into the form, and an empty one would write over
    // whatever is on disk on the first save.
    mount('/settings/environments/broken', {
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
  });
});
