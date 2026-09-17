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

import { DEFAULT_PLATFORM_NOTES } from '@darkwire/protocol';

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
  prompt: 'Alpine 3.23. The shell is ash, not bash.',
  runtime: 'runc',
  workdir: '/work',
  // Hand-written, and behind the Advanced disclosure. The round-trip test below
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

  it('keeps what the definition says about its image, and can be edited', async () => {
    // Read by a model rather than by the engine, and the only field on this
    // screen that is. It was carried by nothing before it was rendered, so a
    // save from here deleted a hand-written one.
    const { user, calls } = mount('/settings/environments/dev', {
      'PUT /api/environments/dev': [200, { environments: [ENVIRONMENT] }],
    });

    const notes = await screen.findByLabelText('Running commands');
    expect(notes).toHaveValue('Alpine 3.23. The shell is ash, not bash.');

    await user.clear(notes);
    await user.type(notes, 'Debian 13. bash, git, ripgrep.');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(putBody(calls)).toMatchObject({
        prompt: 'Debian 13. bash, git, ripgrep.',
      });
    });
  });

  describe('resolving an image', () => {
    /** What the service answers for a tag it had to fetch. */
    const PULLED = {
      reference: 'node:22',
      image: `node@sha256:${'c'.repeat(64)}`,
      pulled: true,
    };

    it('fills the box with the digest, so nobody runs docker inspect', async () => {
      // The step that made adding a container a research project. The rule does
      // not move: what lands in the box is still a digest.
      const { user } = mount('/settings/environments/dev', {
        'POST /api/sandboxes': [200, PULLED],
      });

      const image = await screen.findByLabelText('Image');
      await user.clear(image);
      await user.type(image, 'node:22');
      await user.click(screen.getByRole('button', { name: 'Resolve' }));

      await waitFor(() => {
        expect(image).toHaveValue(PULLED.image);
      });
      expect(screen.getByText(/was pulled/)).toBeInTheDocument();
    });

    it('sends the reference as a management op, not as an exec', async () => {
      const { user, calls } = mount('/settings/environments/dev', {
        'POST /api/sandboxes': [200, PULLED],
      });

      const image = await screen.findByLabelText('Image');
      await user.clear(image);
      await user.type(image, 'node:22');
      await user.click(screen.getByRole('button', { name: 'Resolve' }));

      await waitFor(() => {
        expect(
          calls.some(
            (call) =>
              call.method === 'POST' &&
              call.path === '/api/sandboxes' &&
              JSON.stringify(call.body) ===
                JSON.stringify({ op: 'resolveImage', reference: 'node:22' }),
          ),
        ).toBe(true);
      });
    });

    it('says what the engine said when there is no such image', async () => {
      const { user } = mount('/settings/environments/dev', {
        'POST /api/sandboxes': [
          422,
          { error: { code: 'tool', message: 'manifest unknown' } },
        ],
      });

      const image = await screen.findByLabelText('Image');
      await user.clear(image);
      await user.type(image, 'node:nope');
      await user.click(screen.getByRole('button', { name: 'Resolve' }));

      expect(await screen.findByText(/manifest unknown/)).toBeInTheDocument();
    });
  });

  it('says to leave the heading out, since it is placed under one', async () => {
    const { user } = mount('/settings/environments/dev');

    const notes = await screen.findByLabelText('Running commands');
    await user.clear(notes);
    await user.type(notes, '## What is here');

    expect(screen.getByText(/Leave the heading out/)).toBeInTheDocument();
  });

  it('edits the hardening behind the disclosure, and sends it', async () => {
    // The readout this replaced protected nothing: the route takes a whole
    // definition, so the browser could already write every one of these. What
    // refuses a bad one is the server, and it still does.
    const { user, calls } = mount('/settings/environments/dev', {
      'PUT /api/environments/dev': [200, { environments: [ENVIRONMENT] }],
    });

    await user.click(await screen.findByText('Advanced'));

    const uid = screen.getByLabelText('Runs as');
    await user.clear(uid);
    await user.type(uid, '1002:1002');

    // Each mount is a line, and each line has commas inside it. Splitting on
    // those would cut one mount into three broken ones.
    const tmpfs = screen.getByLabelText('Writable temporary mounts');
    await user.clear(tmpfs);
    await user.type(
      tmpfs,
      '/tmp:rw,nosuid,size=512m{Enter}/var/tmp:rw,size=16m',
    );

    await user.click(screen.getByLabelText('Read-only root filesystem'));
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(putBody(calls)).toMatchObject({
        user: '1002:1002',
        security: {
          readOnlyRoot: false,
          tmpfs: ['/tmp:rw,nosuid,size=512m', '/var/tmp:rw,size=16m'],
        },
      });
    });
  });

  it('refuses a max-processes box that is not a number', async () => {
    // The two numeric boxes under the disclosure validate the way the two above
    // it do. Their errors were unreachable while the section was a readout.
    const { user, calls } = mount('/settings/environments/dev');

    await user.click(await screen.findByText('Advanced'));

    const pids = screen.getByLabelText('Max processes');
    await user.clear(pids);
    await user.type(pids, 'lots');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    expect(
      await screen.findByText('Enter a number of zero or more.'),
    ).toBeInTheDocument();
    expect(putBody(calls)).toBeUndefined();
  });

  it('opens a new environment on the wording it would have inherited', async () => {
    // An empty box was the default, invisibly. Seeding it means the text a new
    // environment sends is the text on screen, and narrowing it is deleting.
    mount('/settings/environments/new');

    const notes = await screen.findByLabelText('Running commands');
    expect(notes).toHaveValue(DEFAULT_PLATFORM_NOTES);
    // The body without its heading, so the editor's own warning does not fire
    // on a form nobody has typed in yet.
    expect(screen.queryByText(/Leave the heading out/)).not.toBeInTheDocument();
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
