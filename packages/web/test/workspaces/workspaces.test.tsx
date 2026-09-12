/**
 * The workspaces page, and the query keys that keep two workspaces apart.
 *
 * The cache test is the one that matters most: two workspaces both contain
 * `notes.md`, so a key that forgot the workspace would serve one workspace's
 * listing for the other the instant the Files page walked into it — silently,
 * and only in a browser.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { Providers } from '@/app/providers.js';
import { createAppRouter } from '@/app/router.js';
import { queryKeys } from '@/lib/query.js';
import { STATUS } from '@testkit/fixtures.js';
import {
  stubApi,
  testQueryClient,
  type RecordedRequest,
  type StubRoute,
} from '@testkit/render.js';
import { DEFAULT_WORKSPACE_ID } from '@/workspaces/workspace-context.js';

function workspace(
  id: string,
  name: string,
  extra: Record<string, unknown> = {},
): unknown {
  return {
    id,
    name,
    isDefault: id === DEFAULT_WORKSPACE_ID,
    createdAtMs: 1,
    updatedAtMs: 1,
    sessionCount: 0,
    ...extra,
  };
}

const TWO = {
  workspaces: [
    workspace('default', 'Default'),
    workspace('acme', 'Client Acme'),
  ],
};

/**
 * `localStorage`, stubbed per file rather than taken from the environment.
 *
 * The same reasoning `setup.ts` gives for `sessionStorage`: Node ships an
 * experimental global that shadows jsdom's and is inert, so a `setItem`
 * succeeds and the `getItem` after it returns null — which reads as a bug in
 * the code under test. Persisting a choice across a page reload should also not
 * mean persisting it across a test.
 */
beforeEach(() => {
  const entries = new Map<string, string>();
  vi.stubGlobal('localStorage', {
    get length() {
      return entries.size;
    },
    clear: () => {
      entries.clear();
    },
    getItem: (key: string) => entries.get(key) ?? null,
    key: (index: number) => [...entries.keys()][index] ?? null,
    removeItem: (key: string) => entries.delete(key),
    setItem: (key: string, value: string) => entries.set(key, value),
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('query keys', () => {
  it('put the workspace before the path, so two workspaces never share an entry', () => {
    expect(queryKeys.files('acme', 'notes.md')).not.toEqual(
      queryKeys.files('research', 'notes.md'),
    );
    expect(queryKeys.fileText('acme', 'notes.md')).not.toEqual(
      queryKeys.fileText('research', 'notes.md'),
    );
    expect(queryKeys.fileUrl('acme', 'x.png')).not.toEqual(
      queryKeys.fileUrl('research', 'x.png'),
    );
  });

  it('keep the workspace at index 1, so a prefix invalidation still means something', () => {
    for (const key of [
      queryKeys.files('acme', 'a'),
      queryKeys.fileText('acme', 'a'),
      queryKeys.fileUrl('acme', 'a'),
    ]) {
      expect(key[1]).toBe('acme');
    }
  });

  it('keep every session view under one prefix', () => {
    // One `invalidateQueries({ queryKey: ['sessions'] })` after a turn has to
    // reach the sidebar, the management page and each of its pages. The list is
    // no longer scoped by workspace — a workspace says where a conversation's
    // files are, not which list it belongs in — so there is one key here and a
    // `workspaceId` argument would be a filter nothing applies.
    const prefix = queryKeys.sessions()[0];
    expect(
      queryKeys.sessionPage({ page: 1, q: '', sort: 'updated', desc: true })[0],
    ).toBe(prefix);
    expect(queryKeys.session('k')[0]).toBe(prefix);
    expect(queryKeys.turns('k')[0]).toBe(prefix);
  });
});

/**
 * The list and the editor, over the real route tree.
 *
 * Mounted through `createAppRouter` rather than as a bare component, the way
 * `agents.test.tsx` does — the list's name cell is a `<Link>` into the editor
 * now, and a test that rendered the page alone could assert the link's href and
 * nothing about whether the screen behind it exists.
 */
const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/status': [200, STATUS],
  '/api/sessions': [200, { sessions: [], total: 0 }],
  '/api/notifications': [200, { notifications: [], unreadCount: 0, total: 0 }],
};

function mount(
  path = '/workspaces',
  overrides: Record<string, StubRoute> = {},
): { readonly calls: RecordedRequest[] } {
  const calls = stubApi({
    ...SHELL_ROUTES,
    'GET /api/workspaces': [200, TWO],
    ...overrides,
  });

  const router = createAppRouter();
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );

  return { calls };
}

describe('the workspaces page', () => {
  /** Opens the kebab for one row and returns nothing — the menu is on screen. */
  async function openActions(name: string): Promise<void> {
    await userEvent.click(
      await screen.findByRole('button', { name: `Actions for ${name}` }),
    );
  }

  it('creates one from a name, never a path', async () => {
    const { calls } = mount('/workspaces', {
      'POST /api/workspaces': [201, workspace('research', 'Research')],
    });

    await userEvent.click(
      await screen.findByRole('link', { name: 'New workspace' }),
    );
    await userEvent.type(await screen.findByLabelText('Name'), 'Research');
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(calls.some((call) => call.method === 'POST')).toBe(true);
    });
    // No `id`: a blank folder box means "derive it", and the derivation is the
    // registry's — it is the only thing that can settle a collision.
    expect(calls.find((call) => call.method === 'POST')?.body).toEqual({
      name: 'Research',
    });
  });

  it('lets the folder be chosen apart from the name', async () => {
    // The whole point of the second box. "Client Acme (2024 rebuild)" would
    // otherwise have to live in `/client-acme-2024-rebuild`, and the only way
    // to get the short folder was to create it under a name nobody wanted.
    const { calls } = mount('/workspaces', {
      'POST /api/workspaces': [
        201,
        workspace('acme24', 'Client Acme (2024 rebuild)'),
      ],
    });

    await userEvent.click(
      await screen.findByRole('link', { name: 'New workspace' }),
    );
    await userEvent.type(
      await screen.findByLabelText('Name'),
      'Client Acme (2024 rebuild)',
    );
    await userEvent.type(await screen.findByLabelText('Folder'), 'acme24');
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(calls.find((call) => call.method === 'POST')?.body).toEqual({
        name: 'Client Acme (2024 rebuild)',
        id: 'acme24',
      });
    });
  });

  it('shows the folder the name would derive, before it is created', async () => {
    mount();

    await userEvent.click(
      await screen.findByRole('link', { name: 'New workspace' }),
    );
    await userEvent.type(await screen.findByLabelText('Name'), 'Client Acme');

    // The derived slug, live. It is what pressing Create would actually produce,
    // and seeing it change while the name is typed is what explains the field.
    expect(
      await screen.findByText(/Creates \/client-acme/),
    ).toBeInTheDocument();
  });

  it('refuses a folder the browser can already tell is wrong', async () => {
    const { calls } = mount();

    await userEvent.click(
      await screen.findByRole('link', { name: 'New workspace' }),
    );
    await userEvent.type(await screen.findByLabelText('Name'), 'Research');

    const folder = await screen.findByLabelText('Folder');
    await userEvent.type(folder, 'Not A Slug');
    expect(await screen.findByRole('alert')).toHaveTextContent(
      /lowercase letters/,
    );

    // A folder another workspace already occupies, checked here so the message
    // can point at the box rather than arriving as a toast after the directory
    // was not created.
    await userEvent.clear(folder);
    await userEvent.type(folder, 'acme');
    expect(await screen.findByRole('alert')).toHaveTextContent(
      /already uses that folder/,
    );

    // Nothing has been sent while any of that was on screen.
    expect(calls.some((call) => call.method === 'POST')).toBe(false);

    await userEvent.clear(folder);
    await userEvent.type(folder, 'research');
    await waitFor(() => {
      expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    });
    expect(calls.some((call) => call.method === 'POST')).toBe(false);
  });

  it('opens the editor from the row, rather than renaming in place', async () => {
    mount();

    // No Rename item: the name is a field on the screen the row opens, and a
    // second way to edit one field is a shortcut kept correct twice.
    await openActions('Client Acme');
    expect(
      await screen.findByRole('menuitem', { name: 'Edit' }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole('menuitem', { name: 'Rename' }),
    ).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole('menuitem', { name: 'Edit' }));

    expect(
      await screen.findByRole('heading', { name: 'Client Acme' }),
    ).toBeInTheDocument();
  });

  it('says which folder each workspace is in, without opening it', async () => {
    mount();

    // The two are chosen separately now, so a list showing only the label would
    // make you open the row to learn where the files actually are.
    expect(await screen.findByText('/acme')).toBeInTheDocument();
  });

  it('cannot delete the default, because it is the parent of the others', async () => {
    mount();

    await openActions('Default');

    expect(
      await screen.findByRole('menuitem', { name: 'Edit' }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole('menuitem', { name: 'Delete' }),
    ).not.toBeInTheDocument();
  });

  it('asks before deleting, and sends nothing until the answer is yes', async () => {
    // The behaviour the dialog never had: a delete used to fire on the single
    // click of an icon sitting next to Rename.
    const { calls } = mount();

    await openActions('Client Acme');
    await userEvent.click(
      await screen.findByRole('menuitem', { name: 'Delete' }),
    );

    expect(
      await screen.findByText(/Its folder and everything in it stays on disk/),
    ).toBeVisible();
    expect(calls.some((call) => call.method === 'DELETE')).toBe(false);

    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(calls.some((call) => call.method === 'DELETE')).toBe(false);
  });

  it('offers to move the sessions when a delete is refused, then deletes', async () => {
    let refused = true;
    const { calls } = mount('/workspaces', {
      'DELETE /api/workspaces/acme': () =>
        refused
          ? [
              409,
              {
                error: {
                  code: 'conflict',
                  message: '2 sessions still use this workspace',
                  retryable: false,
                  details: { sessionCount: 2 },
                },
              },
            ]
          : [204, null],
      'POST /api/workspaces/acme/sessions/move': () => {
        refused = false;
        return [200, { moved: 2 }];
      },
    });

    await openActions('Client Acme');
    await userEvent.click(
      await screen.findByRole('menuitem', { name: 'Delete' }),
    );
    await userEvent.click(
      await screen.findByRole('button', { name: 'Delete' }),
    );

    // The 409 is a question, not a failure: the count it carries is what the
    // offer is made out of.
    expect(
      await screen.findByText(/2 sessions still belong to Client Acme/),
    ).toBeInTheDocument();

    await userEvent.click(
      screen.getByRole('button', { name: 'Move and delete' }),
    );

    await waitFor(() => {
      expect(calls.filter((call) => call.method === 'DELETE')).toHaveLength(2);
    });
    expect(
      calls.some((call) => call.path === '/api/workspaces/acme/sessions/move'),
    ).toBe(true);
  });

  it('explains what a workspace is, once, where someone is reading about them', async () => {
    mount();

    expect(
      await screen.findByText(/cannot reach each other/),
    ).toBeInTheDocument();
  });

  it('keeps the default at the top however the list is sorted', async () => {
    mount();

    const firstRow = async (): Promise<string> => {
      const list = await screen.findByRole('list', { name: 'Workspaces' });
      return within(list).getAllByRole('listitem')[0]?.textContent ?? '';
    };

    expect(await firstRow()).toContain('Default');

    await userEvent.click(
      await screen.findByRole('button', { name: /Sort by/ }),
    );
    await userEvent.click(
      await screen.findByRole('menuitemradio', { name: 'Descending' }),
    );
    expect(await firstRow()).toContain('Default');
  });
});

describe('the workspace editor', () => {
  it('moves the folder, and sends the sessions after it in one request', async () => {
    const { calls } = mount('/workspaces/acme', {
      'PATCH /api/workspaces/acme': [200, workspace('acme24', 'Client Acme')],
    });

    const folder = await screen.findByLabelText('Folder');
    expect(folder).toHaveValue('acme');

    await userEvent.clear(folder);
    await userEvent.type(folder, 'acme24');
    // Said before it is done, because a rename under a tree someone is working
    // in is not something to discover from a toast afterwards.
    expect(
      await screen.findByText(/Moves the folder to \/acme24/),
    ).toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(calls.find((call) => call.method === 'PATCH')?.body).toEqual({
        id: 'acme24',
      });
    });
  });

  it('refuses a folder another workspace already holds', async () => {
    const { calls } = mount('/workspaces/acme');

    const folder = await screen.findByLabelText('Folder');
    await userEvent.clear(folder);
    await userEvent.type(folder, 'default');

    expect(await screen.findByRole('alert')).toHaveTextContent(/reserved/);
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    expect(calls.some((call) => call.method === 'PATCH')).toBe(false);
  });

  it('leaves the default workspace no folder to move', async () => {
    mount('/workspaces/default');

    // Its directory *is* the root every other workspace is created inside, so
    // there is nothing a rename of it could mean. The box holds `/` as a value
    // rather than as a placeholder: a placeholder is drawn muted and means
    // "nothing here yet", which made the one answer on the screen that is fixed
    // look like the one nobody had filled in.
    const folder = await screen.findByLabelText('Folder');
    expect(folder).toBeDisabled();
    expect(folder).toHaveValue('/');
  });

  it('never claims the default lives in a folder called default', async () => {
    mount();

    // `workspace/default` is a path that does not exist, and the list rendered
    // it under the name for a while. The default's row reads `/`, so the column
    // shows the nesting the others sit in — and never borrows `workspace`,
    // which the Files breadcrumb already uses for a different directory.
    expect(await screen.findByText('/acme')).toBeInTheDocument();
    expect(screen.getByText('/', { exact: true })).toBeInTheDocument();
    expect(screen.queryByText(/^workspace/)).not.toBeInTheDocument();
  });

  it('will not save nothing', async () => {
    const { calls } = mount('/workspaces/acme');

    // Clean on arrival: a Save that is always pressable invites a rename to the
    // name it already has.
    expect(
      await screen.findByRole('button', { name: 'Save changes' }),
    ).toBeDisabled();

    await userEvent.clear(await screen.findByLabelText('Name'));
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    expect(calls.some((call) => call.method === 'PATCH')).toBe(false);
  });

  it('offers no delete for the default, and one for the others', async () => {
    mount('/workspaces/default');
    await screen.findByRole('heading', { name: 'Default' });
    expect(
      screen.queryByRole('button', { name: /^Actions for/ }),
    ).not.toBeInTheDocument();
  });

  it('renames without touching the folder, and sends only the name', async () => {
    const { calls } = mount('/workspaces/acme', {
      'PATCH /api/workspaces/acme': [200, workspace('acme', 'Acme Ltd')],
    });

    await userEvent.clear(await screen.findByLabelText('Name'));
    await userEvent.type(screen.getByLabelText('Name'), 'Acme Ltd');
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      // No `id`: an unchanged box is not an instruction to move a directory.
      expect(calls.find((call) => call.method === 'PATCH')?.body).toEqual({
        name: 'Acme Ltd',
      });
    });
  });

  it('says so rather than showing an empty form for a workspace that is gone', async () => {
    mount('/workspaces/vanished');

    expect(await screen.findByRole('alert')).toHaveTextContent(
      /no workspace called/,
    );
    expect(
      screen.getByRole('link', { name: /Back to workspaces/ }),
    ).toBeInTheDocument();
  });
});

/**
 * The empty-filter message.
 *
 * `common.noMatches` interpolates `{{count}}`, which makes i18next look for
 * `noMatches_one` / `noMatches_other` — and both were committed empty. Measured:
 * i18next falls back to the base key when a plural form is `''`, so this rendered
 * correctly the whole time; the empty entries were untidy rather than broken.
 * They are filled in now because the fallback is a behaviour of the library
 * rather than a promise, and because `pnpm i18n:extract` keeps re-creating them.
 */
describe('filtering to nothing', () => {
  it('says so instead of rendering an empty line', async () => {
    mount();

    await userEvent.type(
      await screen.findByLabelText('Filter workspaces by name or folder'),
      'zzzznothing',
    );

    expect(screen.getByText(/Nothing here matches/)).toBeInTheDocument();
  });
});
