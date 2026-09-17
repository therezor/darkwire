/**
 * The file browser, over the real router.
 *
 * What is worth driving here is the wiring between a path in the URL, a path in
 * a query string and a path in a signed URL — three places the same string has
 * to survive, and the browser is the only thing that proves it did. The
 * irreversible action gets its own case: delete asks first, and a cancelled
 * dialog sends nothing.
 */

import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { render, screen, waitFor, within } from '@testing-library/react';
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

/** The `modifiedAtMs` the editor loads, and therefore the one a save must send back. */
const LOADED_AT = 1_700_000_000_000;

const ROOT_LISTING = {
  path: '',
  entries: [
    {
      path: 'notes',
      name: 'notes',
      isDirectory: true,
      sizeBytes: 0,
      modifiedAtMs: 1_700_000_000_000,
    },
    {
      path: 'shot.png',
      name: 'shot.png',
      isDirectory: false,
      sizeBytes: 2048,
      modifiedAtMs: 1_700_000_000_000,
      mimeType: 'image/png',
    },
    {
      path: 'notes.md',
      name: 'notes.md',
      isDirectory: false,
      sizeBytes: 12,
      modifiedAtMs: 1_700_000_000_000,
      mimeType: 'text/markdown; charset=utf-8',
    },
  ],
};

const NOTES_LISTING = {
  path: 'notes',
  entries: [
    {
      path: 'notes/a.txt',
      name: 'a.txt',
      isDirectory: false,
      sizeBytes: 3,
      modifiedAtMs: 1_700_000_000_000,
      mimeType: 'text/plain; charset=utf-8',
    },
  ],
};

/** The workspaces directory: one folder per workspace, `default` among them. */
const WORKSPACES_LISTING = {
  path: '',
  entries: [
    {
      path: 'acme',
      name: 'acme',
      isDirectory: true,
      sizeBytes: 0,
      modifiedAtMs: 1_700_000_000_000,
    },
    {
      path: 'default',
      name: 'default',
      isDirectory: true,
      sizeBytes: 0,
      modifiedAtMs: 1_700_000_000_000,
    },
  ],
};

const SHELL_ROUTES: Record<string, StubRoute> = {
  '/api/auth/me': [200, { authenticated: true, authEnabled: false }],
  // Claimed: the setup overlay mounts above the login one and would
  // otherwise be deciding whether to open on an unstubbed request.
  '/api/setup': [200, { required: false }],
  '/api/status': [200, { ...STATUS, model: 'llama3', toolCount: 0 }],
  '/api/sessions': [200, { sessions: [], total: 0 }],
  '/api/notifications': [200, { notifications: [], unreadCount: 0, total: 0 }],
};

function mount(
  // Inside the default workspace, because that is where all but a handful of
  // these cases live. `/files` with nothing after it is the list of workspaces,
  // and the cases that want that one ask for it.
  path = '/files?workspace=default',
  overrides: Record<string, StubRoute> = {},
): {
  readonly user: ReturnType<typeof userEvent.setup>;
  readonly calls: RecordedRequest[];
  readonly router: ReturnType<typeof createAppRouter>;
} {
  const calls = stubApi({
    ...SHELL_ROUTES,
    // The listing answers per directory, which is what the navigation case
    // needs: a stub keyed only on the path could not tell the two apart.
    'GET /api/files': (request) =>
      request.query.get('workspace') === 'workspaces'
        ? [200, WORKSPACES_LISTING]
        : request.query.get('path') === 'notes'
          ? [200, NOTES_LISTING]
          : [200, ROOT_LISTING],
    'DELETE /api/files': [204, null],
    'POST /api/files/upload': [
      201,
      {
        path: 'report.txt',
        sizeBytes: 5,
        mimeType: 'text/plain; charset=utf-8',
      },
    ],
    'POST /api/files/signed-url': [
      200,
      { url: '/api/media/tok', expiresAtMs: 9_999_999_999_999 },
    ],
    '/api/media/tok': [200, 'hello from the workspace'],
    'GET /api/files/text': [
      200,
      {
        path: 'notes.md',
        content: 'hello from the workspace',
        sizeBytes: 24,
        modifiedAtMs: LOADED_AT,
        truncated: false,
      },
    ],
    'PUT /api/files/text': [
      200,
      {
        path: 'notes.md',
        name: 'notes.md',
        isDirectory: false,
        sizeBytes: 5,
        modifiedAtMs: LOADED_AT + 1000,
        mimeType: 'text/markdown; charset=utf-8',
      },
    ],
    'POST /api/files/directory': [
      201,
      {
        path: 'drafts',
        name: 'drafts',
        isDirectory: true,
        sizeBytes: 0,
        modifiedAtMs: LOADED_AT,
      },
    ],
    ...overrides,
  });

  const user = userEvent.setup();
  const router = createAppRouter();
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  render(
    <Providers client={testQueryClient()}>
      <RouterProvider router={router} />
    </Providers>,
  );

  return { user, calls, router };
}

/**
 * Opens a row's kebab and picks Delete.
 *
 * Row actions moved behind a menu when Files, Agents and Workspaces were given
 * one vocabulary: an agent row has four actions, and four ghost icon buttons in
 * a table cell reads as a toolbar with a name attached. It also stopped Delete
 * from sitting permanently one pixel away from a harmless neighbour.
 */
async function deleteFrom(
  user: ReturnType<typeof userEvent.setup>,
  name: string,
): Promise<void> {
  await user.click(
    await screen.findByRole('button', { name: `Actions for ${name}` }),
  );
  await user.click(await screen.findByRole('menuitem', { name: 'Delete' }));
}

describe('the workspaces directory', () => {
  it('opens there when the address names no workspace', async () => {
    const { calls } = mount('/files');

    // Every workspace's folder, which is what makes all of them reachable from
    // one page. No workspace contains another, so this is the only listing that
    // shows more than one.
    expect(
      await screen.findByRole('link', { name: 'acme' }),
    ).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'default' })).toBeInTheDocument();

    // The reserved id, not a workspace: the server turns it into the directory
    // the folders sit in.
    const listing = calls.find((call) => call.path === '/api/files');
    expect(listing?.query.get('workspace')).toBe('workspaces');
  });

  it('walks into a workspace, and the address carries it', async () => {
    const { user, router } = mount('/files');

    await user.click(await screen.findByRole('link', { name: 'acme' }));

    // A directory here is a workspace, so the address names the workspace
    // rather than a path inside the tree above it.
    expect(router.state.location.searchStr).toContain('path=acme');
  });

  it('will not delete the default workspace’s folder', async () => {
    const { user } = mount('/files');

    await user.click(
      await screen.findByRole('button', { name: 'Actions for default' }),
    );
    expect(
      screen.queryByRole('menuitem', { name: 'Delete' }),
    ).not.toBeInTheDocument();
    // Everything else on the row is still there, so this is one item withheld
    // rather than a row with nothing to do to it.
    expect(
      screen.getByRole('menuitem', { name: 'Rename' }),
    ).toBeInTheDocument();
  });

  it('offers Delete on every other folder there', async () => {
    const { user } = mount('/files');

    await user.click(
      await screen.findByRole('button', { name: 'Actions for acme' }),
    );
    expect(
      screen.getByRole('menuitem', { name: 'Delete' }),
    ).toBeInTheDocument();
  });

  it('treats a directory called default inside a workspace as ordinary', async () => {
    const { user } = mount('/files?workspace=default', {
      'GET /api/files': [
        200,
        {
          path: '',
          entries: [
            {
              path: 'default',
              name: 'default',
              isDirectory: true,
              sizeBytes: 0,
              modifiedAtMs: 1_700_000_000_000,
            },
          ],
        },
      ],
    });

    await user.click(
      await screen.findByRole('button', { name: 'Actions for default' }),
    );
    expect(
      screen.getByRole('menuitem', { name: 'Delete' }),
    ).toBeInTheDocument();
  });
});

describe('the file browser', () => {
  it('lists the workspace root and asks for it as `.`', async () => {
    const { calls } = mount();

    expect(
      await screen.findByRole('link', { name: 'notes.md' }),
    ).toBeInTheDocument();
    expect(screen.getByText('2.0 kB')).toBeInTheDocument();

    // `''` would reach the jail as an empty path: the query parameter's default
    // only applies when it is absent.
    const listing = calls.find((call) => call.path === '/api/files');
    expect(listing?.query.get('path')).toBe('.');
  });

  it('navigates into a directory, and back out through the breadcrumb', async () => {
    const { user, router } = mount();

    // A link, like every other CRUD row in the app: opening one changes the
    // address, so it has to survive a middle-click and a new tab.
    const folder = await screen.findByRole('link', { name: 'notes' });
    expect(folder).toHaveAttribute(
      'href',
      expect.stringContaining('path=notes'),
    );

    await user.click(folder);
    expect(
      await screen.findByRole('link', { name: 'a.txt' }),
    ).toBeInTheDocument();
    expect(router.state.location.searchStr).toContain('path=notes');

    // The workspace's own crumb, which is its folder. Clicking it goes to the
    // top of *this* workspace and not out of it: `workspace` survives and only
    // `path` goes.
    const trail = screen.getByRole('navigation', { name: 'Breadcrumb' });
    await user.click(within(trail).getByRole('link', { name: 'default' }));

    expect(
      await screen.findByRole('link', { name: 'notes.md' }),
    ).toBeInTheDocument();
    expect(router.state.location.searchStr).not.toContain('path=');
    expect(router.state.location.searchStr).toContain('workspace=default');
  });

  it('opens a file through the address, so its row is a link like every other', async () => {
    const { user, router } = mount();

    const file = await screen.findByRole('link', { name: 'notes.md' });
    // The href is the whole point of the row being an `<a>`: a dialog held in
    // component state cannot be opened in a second tab or reloaded back into.
    expect(file).toHaveAttribute(
      'href',
      expect.stringContaining('file=notes.md'),
    );

    await user.click(file);
    expect(await screen.findByRole('dialog')).toHaveTextContent('notes.md');
    expect(router.state.location.searchStr).toContain('file=notes.md');
  });

  it('opens the file the address names on a cold load', async () => {
    mount('/files?workspace=default&file=notes.md');

    expect(
      await screen.findByRole('textbox', { name: 'Contents of notes.md' }),
    ).toBeVisible();
  });

  it('takes the file back out of the address when the dialog closes', async () => {
    const { user, router } = mount('/files?workspace=default&file=notes.md');
    await screen.findByRole('textbox', { name: 'Contents of notes.md' });

    await user.keyboard('{Escape}');

    await waitFor(() => {
      expect(router.state.location.searchStr).not.toContain('file=');
    });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('offers the whole empty directory as somewhere to drop a file', async () => {
    mount('/files?workspace=default', {
      'GET /api/files': [200, { path: '', entries: [] }],
    });

    // Not a sentence under a rule: with no rows there is nothing to aim a file
    // at, so the target is the thing that has to be visible.
    expect(
      await screen.findByText('This directory is empty.'),
    ).toBeInTheDocument();
    expect(document.querySelector('.file-drop--empty')).not.toBeNull();
  });

  it('shows the trail down to the directory it is in', async () => {
    mount('/files?workspace=default&path=notes');

    const trail = await screen.findByRole('navigation', { name: 'Breadcrumb' });
    expect(
      within(trail).getByRole('link', { name: 'default' }),
    ).toBeInTheDocument();
    // The current directory is text, not a link to where it already is.
    expect(
      within(trail).queryByRole('link', { name: 'notes' }),
    ).not.toBeInTheDocument();
    expect(trail).toHaveTextContent('notes');
  });

  it('uploads into the directory being looked at', async () => {
    const { user, calls } = mount('/files?workspace=default&path=notes');
    await screen.findByRole('link', { name: 'a.txt' });

    const file = new File(['hello'], 'report.txt', { type: 'text/plain' });
    await user.upload(screen.getByLabelText('Upload files'), file);

    await waitFor(() => {
      expect(calls.some((call) => call.path === '/api/files/upload')).toBe(
        true,
      );
    });

    const upload = calls.find((call) => call.path === '/api/files/upload');
    // The full destination, not a name the server would have to place.
    expect(upload?.query.get('path')).toBe('notes/report.txt');
    // The raw bytes: a base64 or multipart envelope would inflate every upload
    // to describe what `Content-Type` already says.
    expect(upload?.body).toBeInstanceOf(File);
  });

  it('asks before deleting, and sends nothing when the answer is no', async () => {
    const { user, calls } = mount();
    await screen.findByRole('link', { name: 'notes.md' });

    await deleteFrom(user, 'notes.md');
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('There is no undo');

    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(calls.some((call) => call.method === 'DELETE')).toBe(false);
  });

  it('deletes the file the dialog named', async () => {
    const { user, calls } = mount();
    await screen.findByRole('link', { name: 'notes.md' });

    await deleteFrom(user, 'notes.md');
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }));

    await waitFor(() => {
      expect(calls.some((call) => call.method === 'DELETE')).toBe(true);
    });
    expect(
      calls.find((call) => call.method === 'DELETE')?.query.get('path'),
    ).toBe('notes.md');
  });

  it('counts what a folder holds before asking to delete it', async () => {
    const { user, calls } = mount();
    await screen.findByRole('link', { name: 'notes.md' });

    await deleteFrom(user, 'notes');
    const dialog = await screen.findByRole('dialog');

    // "Delete drafts?" and "Delete drafts and the things in it?" are different
    // questions, and only one of them is the one being asked.
    expect(dialog).toHaveTextContent('Delete this folder?');
    expect(await within(dialog).findByText(/1 item/)).toBeInTheDocument();

    await user.click(within(dialog).getByRole('button', { name: 'Delete' }));

    await waitFor(() => {
      expect(calls.some((call) => call.method === 'DELETE')).toBe(true);
    });
    const request = calls.find((call) => call.method === 'DELETE');
    expect(request?.query.get('path')).toBe('notes');
    // The flag exists to stop a stray request from recursing, not to make the
    // UI ask twice — the dialog already said what goes.
    expect(request?.query.get('recursive')).toBe('true');
  });

  it('does not ask a file to recurse', async () => {
    const { user, calls } = mount();
    await screen.findByRole('link', { name: 'notes.md' });

    await deleteFrom(user, 'notes.md');
    await user.click(
      within(await screen.findByRole('dialog')).getByRole('button', {
        name: 'Delete',
      }),
    );

    await waitFor(() => {
      expect(calls.some((call) => call.method === 'DELETE')).toBe(true);
    });
    expect(
      calls.find((call) => call.method === 'DELETE')?.query.get('recursive'),
    ).toBeNull();
  });

  it('previews an image through a signed URL rather than a public route', async () => {
    const { user, calls } = mount();

    await user.click(await screen.findByRole('link', { name: 'shot.png' }));

    const image = await screen.findByRole('img', { name: 'shot.png' });
    expect(image).toHaveAttribute('src', '/api/media/tok');

    // The path travels in a body, not a query string: a workspace path in a URL
    // is written to every access log between here and the server.
    const signing = calls.find((call) => call.path === '/api/files/signed-url');
    expect(signing?.method).toBe('POST');
    expect(signing?.body).toEqual({ path: 'shot.png', workspaceId: 'default' });
  });

  it('reads a text file into the page rather than framing it', async () => {
    const { user } = mount();

    await user.click(await screen.findByRole('link', { name: 'notes.md' }));

    // The workspace holds model-authored files; an iframe would execute one.
    expect(
      await screen.findByDisplayValue(/hello from the workspace/),
    ).toBeInTheDocument();
    expect(document.querySelector('iframe')).toBeNull();
  });

  it('says so rather than failing when a directory cannot be listed', async () => {
    mount('/files?workspace=default', {
      'GET /api/files': [
        404,
        { error: { code: 'not_found', message: 'No such file: gone' } },
      ],
    });

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'No such file: gone',
    );
  });
});

// The editor

/** Opens `notes.md` and hands back its textarea. */
async function openEditor(
  user: ReturnType<typeof userEvent.setup>,
): Promise<HTMLElement> {
  await user.click(await screen.findByRole('link', { name: 'notes.md' }));
  return await screen.findByRole('textbox', { name: 'Contents of notes.md' });
}

describe('the file editor', () => {
  it('opens read-only, because opening a file is usually reading one', async () => {
    const { user } = mount();

    const textarea = await openEditor(user);

    // This tree is what an agent has been writing to. A textarea that is
    // already live is one stray keystroke away from editing the evidence.
    expect(textarea).toHaveAttribute('readonly');
    expect(screen.getByRole('button', { name: 'Edit' })).toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Save' }),
    ).not.toBeInTheDocument();
  });

  it('saves the edited text with the timestamp it loaded', async () => {
    const { user, calls } = mount();
    const textarea = await openEditor(user);

    await user.click(screen.getByRole('button', { name: 'Edit' }));
    await user.clear(textarea);
    await user.type(textarea, 'edits');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => {
      expect(calls.some((call) => call.method === 'PUT')).toBe(true);
    });

    // The timestamp is the whole guard: without it, an editor left open through
    // a turn silently deletes whatever that turn wrote.
    expect(calls.find((call) => call.method === 'PUT')?.body).toEqual({
      path: 'notes.md',
      content: 'edits',
      workspaceId: 'default',
      expectedModifiedAtMs: LOADED_AT,
    });
  });

  it('keeps the edits and explains when the file moved under it', async () => {
    const { user } = mount('/files?workspace=default', {
      'PUT /api/files/text': [
        409,
        {
          error: {
            code: 'bad_request',
            message: 'Changed since it was read: notes.md',
          },
        },
      ],
    });
    const textarea = await openEditor(user);

    await user.click(screen.getByRole('button', { name: 'Edit' }));
    await user.clear(textarea);
    await user.type(textarea, 'mine');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'changed on disk',
    );
    // The edits are still there to copy out. Dropping them on a conflict would
    // punish the reader for the agent's write.
    expect(textarea).toHaveValue('mine');
  });

  it('will not edit a truncated file, because saving a prefix deletes the rest', async () => {
    const { user } = mount('/files?workspace=default', {
      'GET /api/files/text': [
        200,
        {
          path: 'notes.md',
          content: 'the first part only',
          sizeBytes: 900_000,
          modifiedAtMs: LOADED_AT,
          truncated: true,
        },
      ],
    });

    const textarea = await openEditor(user);

    expect(textarea).toHaveAttribute('readonly');
    expect(
      screen.queryByRole('button', { name: 'Edit' }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(/only the first part was read/),
    ).toBeInTheDocument();
  });

  it('asks before closing on unsaved edits, and closing is what Escape does', async () => {
    const { user, calls } = mount();
    const textarea = await openEditor(user);

    await user.click(screen.getByRole('button', { name: 'Edit' }));
    await user.type(textarea, ' and more');
    await user.keyboard('{Escape}');

    expect(await screen.findByText('Discard your edits?')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Keep editing' }));
    expect(calls.some((call) => call.method === 'PUT')).toBe(false);
  });

  /**
   * The reason this route exists rather than a second read through the signed
   * URL: the server's MIME table is small, so `.py` and `.ts` are
   * `application/octet-stream` and would have been declared unpreviewable by a
   * browser-side type check.
   */
  it('offers the editor for a file the MIME table does not know', async () => {
    const { user, calls } = mount('/files?workspace=default', {
      'GET /api/files': [
        200,
        {
          path: '',
          entries: [
            {
              path: 'script.py',
              name: 'script.py',
              isDirectory: false,
              sizeBytes: 12,
              modifiedAtMs: LOADED_AT,
              mimeType: 'application/octet-stream',
            },
          ],
        },
      ],
      'GET /api/files/text': [
        200,
        {
          path: 'script.py',
          content: 'print("hi")',
          sizeBytes: 12,
          modifiedAtMs: LOADED_AT,
          truncated: false,
        },
      ],
    });

    await user.click(await screen.findByRole('link', { name: 'script.py' }));

    expect(await screen.findByDisplayValue('print("hi")')).toBeInTheDocument();
    expect(calls.some((call) => call.path === '/api/files/text')).toBe(true);
  });

  it('offers a download instead when the bytes turn out not to be text', async () => {
    const { user } = mount('/files?workspace=default', {
      'GET /api/files/text': [
        400,
        {
          error: { code: 'bad_request', message: 'Not a text file: notes.md' },
        },
      ],
    });

    await user.click(await screen.findByRole('link', { name: 'notes.md' }));

    expect(await screen.findByText(/not text/)).toBeInTheDocument();
    expect(
      screen.queryByRole('textbox', { name: /Contents of/ }),
    ).not.toBeInTheDocument();
  });
});

// Creating, filtering and sorting

describe('creating entries', () => {
  it('creates a folder in the directory being looked at', async () => {
    const { user, calls } = mount('/files?workspace=default&path=notes');
    await screen.findByRole('link', { name: 'a.txt' });

    await user.click(screen.getByRole('button', { name: 'New' }));
    await user.click(
      await screen.findByRole('menuitem', { name: 'New folder' }),
    );
    await user.type(
      screen.getByRole('textbox', { name: 'Folder name' }),
      'drafts',
    );
    await user.click(screen.getByRole('button', { name: 'Create' }));

    await waitFor(() => {
      expect(calls.some((call) => call.path === '/api/files/directory')).toBe(
        true,
      );
    });
    // The full destination, not a name the server would have to place.
    expect(
      calls.find((call) => call.path === '/api/files/directory')?.body,
    ).toEqual({
      path: 'notes/drafts',
      workspaceId: 'default',
    });
  });

  it('creates an empty file and opens it', async () => {
    const { user, calls } = mount();
    await screen.findByRole('link', { name: 'notes.md' });

    await user.click(screen.getByRole('button', { name: 'New' }));
    await user.click(await screen.findByRole('menuitem', { name: 'New file' }));
    await user.type(
      screen.getByRole('textbox', { name: 'File name' }),
      'notes.md',
    );
    await user.click(screen.getByRole('button', { name: 'Create' }));

    const write = await waitFor(() => {
      const call = calls.find((entry) => entry.method === 'PUT');
      expect(call).toBeDefined();
      return call;
    });

    // Empty, and with no timestamp: there is nothing yet to conflict with, and
    // a placeholder line would be content the reader did not write.
    expect(write?.body).toEqual({
      path: 'notes.md',
      content: '',
      workspaceId: 'default',
    });
    expect(
      await screen.findByRole('textbox', { name: 'Contents of notes.md' }),
    ).toBeVisible();
  });
});

describe('renaming', () => {
  /** Opens a row's kebab and picks Rename. */
  async function renameFrom(
    user: ReturnType<typeof userEvent.setup>,
    name: string,
  ): Promise<void> {
    await user.click(
      await screen.findByRole('button', { name: `Actions for ${name}` }),
    );
    await user.click(await screen.findByRole('menuitem', { name: 'Rename' }));
  }

  it('renames a folder, sending both ends of the move', async () => {
    const { user, calls } = mount('/files?workspace=default', {
      'POST /api/files/move': [
        200,
        {
          path: 'archive',
          name: 'archive',
          isDirectory: true,
          sizeBytes: 0,
          modifiedAtMs: 1,
        },
      ],
    });

    await renameFrom(user, 'notes');
    const field = await screen.findByRole('textbox', { name: 'New name' });
    // Prefilled, because a rename starts from the current name far more often
    // than from nothing.
    expect(field).toHaveValue('notes');

    await user.clear(field);
    await user.type(field, 'archive{Enter}');

    await waitFor(() => {
      expect(calls.some((call) => call.path === '/api/files/move')).toBe(true);
    });
    expect(calls.find((call) => call.path === '/api/files/move')?.body).toEqual(
      {
        from: 'notes',
        to: 'archive',
        workspaceId: 'default',
      },
    );
  });

  it('says what happens to a folder’s contents, which is the question', async () => {
    const { user } = mount();

    await renameFrom(user, 'notes');
    expect(
      await screen.findByText(/Everything inside moves with it/),
    ).toBeVisible();
  });

  it('keeps a renamed file in the parent it was already in', async () => {
    // Joined to the entry's own parent rather than to the directory on screen.
    // They agree today; a rename that quietly relocated a row would not be
    // visible until something could list an entry from elsewhere.
    const { user, calls } = mount('/files?workspace=default&path=notes', {
      'POST /api/files/move': [
        200,
        {
          path: 'notes/b.txt',
          name: 'b.txt',
          isDirectory: false,
          sizeBytes: 1,
          modifiedAtMs: 1,
        },
      ],
    });

    await renameFrom(user, 'a.txt');
    const field = await screen.findByRole('textbox', { name: 'New name' });
    await user.clear(field);
    await user.type(field, 'b.txt{Enter}');

    await waitFor(() => {
      expect(calls.some((call) => call.path === '/api/files/move')).toBe(true);
    });
    expect(
      calls.find((call) => call.path === '/api/files/move')?.body,
    ).toMatchObject({
      from: 'notes/a.txt',
      to: 'notes/b.txt',
    });
  });

  it('reports a refusal rather than pretending it worked', async () => {
    const { user, calls } = mount('/files?workspace=default', {
      'POST /api/files/move': [
        409,
        {
          error: {
            code: 'conflict',
            message: 'Already exists: shot.png',
            retryable: false,
          },
        },
      ],
    });

    await renameFrom(user, 'notes');
    const field = await screen.findByRole('textbox', { name: 'New name' });
    await user.clear(field);
    await user.type(field, 'shot.png{Enter}');

    await waitFor(() => {
      expect(calls.some((call) => call.path === '/api/files/move')).toBe(true);
    });
    expect(await screen.findByText(/Could not rename it/)).toBeVisible();
  });
});

describe('reading a large directory', () => {
  it('filters by name without asking the server again', async () => {
    const { user, calls } = mount();
    await screen.findByRole('link', { name: 'notes.md' });
    const before = calls.length;

    await user.type(
      screen.getByRole('searchbox', { name: 'Filter by name' }),
      'shot',
    );

    expect(screen.getByRole('link', { name: 'shot.png' })).toBeInTheDocument();
    expect(
      screen.queryByRole('link', { name: 'notes.md' }),
    ).not.toBeInTheDocument();
    // One directory is already loaded; filtering it is not a round trip.
    expect(calls.length).toBe(before);
  });

  it('sorts by a column and announces which one', async () => {
    const { user } = mount();
    await screen.findByRole('link', { name: 'notes.md' });

    await user.click(screen.getByRole('button', { name: /Sort by/ }));
    await user.click(
      await screen.findByRole('menuitemradio', { name: 'Size' }),
    );

    // "Which is biggest" is what a size column is asked, so it opens largest
    // first — the trigger names the column and the direction together.
    expect(
      screen.getByRole('button', { name: 'Sort by Size, Descending' }),
    ).toBeInTheDocument();

    // Directories stay first in every order — they are where to go next, not
    // small files — and the files behind them are largest first.
    // The name off each row, by the cell it sits in rather than by role: a
    // directory opens through a link and a file through a button, so there is
    // no one role that names every row in the order they are being read in.
    const ordered = within(screen.getByRole('list', { name: 'Files' }))
      .getAllByRole('listitem')
      .map(
        (row) => row.querySelector('.data-list__primary')?.textContent ?? '',
      );
    expect(ordered).toEqual(['notes', 'shot.png', 'notes.md']);
  });
});
