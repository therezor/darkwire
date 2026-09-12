/**
 * Workspaces, in a browser, against the real stack.
 *
 * The unit suites already prove the pieces: the jail clamps, the registry
 * refuses a path, the routes scope by workspace, the query keys carry it. What
 * only a browser can show is that they are wired to each other — that walking
 * into a workspace's folder actually moves the Files page, and that a file
 * uploaded into one workspace is not visible from another.
 *
 * That last assertion is the one worth the cost of a browser. A missing
 * workspace segment in a React Query key is invisible to every layer below the
 * cache: the server answers correctly, the client asks correctly, and the user
 * still sees the wrong workspace's files because an entry was served from
 * memory. It fails here and nowhere else.
 */

import type { Page } from '@playwright/test';

import { expect, test } from '../src/fixtures.js';

/** Every workspace as the registry has it, reduced to what an assertion reads. */
async function workspacesOf(
  app: Page,
  url: string,
): Promise<Array<{ id: string; name: string }>> {
  const response = await app.request.get(`${url}/api/workspaces`);
  const body = (await response.json()) as {
    workspaces: Array<{ id: string; name: string }>;
  };
  return body.workspaces.map((row) => ({ id: row.id, name: row.name }));
}

/**
 * Opens a named workspace on the Files page by clicking into its folder.
 *
 * There is no workspace control here to drive: the page opens at the default
 * workspace, which is the parent of every named one, so they are ordinary
 * folders and the tree *is* the navigation.
 */
async function openFolder(app: Page, id: string): Promise<void> {
  await app.getByRole('link', { name: id, exact: true }).click();
}

test.describe('workspaces', () => {
  test('a workspace is created under a folder of its own choosing', async ({
    app,
    harness,
  }) => {
    // The two are separate answers, and the registry has always stored them in
    // separate columns — what only a browser shows is that the second box
    // actually reaches the request rather than being decoration over a slug the
    // name derived anyway.
    await app.goto(`${harness.url}/workspaces`);

    await app.getByRole('link', { name: 'New workspace' }).click();
    await app.getByLabel('Name').fill('Client Acme (2024 rebuild)');
    await app.getByLabel('Folder').fill('acme24');
    await app.getByRole('button', { name: 'Save changes' }).click();

    // The durable state, not the toast that announces it: the registry holds
    // both halves, separately, as they were typed.
    await expect
      .poll(async () => await workspacesOf(app, harness.url))
      .toContainEqual({
        id: 'acme24',
        name: 'Client Acme (2024 rebuild)',
      });

    // And Save lands on the workspace it made, which is the page that can now
    // move or remove it.
    await expect(app).toHaveURL(/\/workspaces\/acme24$/u);
  });

  test('an abandoned create makes no directory at all', async ({
    app,
    harness,
  }) => {
    // The reason create is a page rather than the dialog it replaced: that
    // dialog ran the `mkdir` the moment it was submitted.
    const before = await workspacesOf(app, harness.url);

    await app.goto(`${harness.url}/workspaces/new`);
    await app.getByLabel('Name').fill('Never finished');
    await app.getByRole('link', { name: 'Workspaces' }).first().click();

    expect(await workspacesOf(app, harness.url)).toEqual(before);
  });

  test('the row opens an editor, and the name changes without the folder moving', async ({
    app,
    harness,
  }) => {
    await app.request.post(`${harness.url}/api/workspaces`, {
      data: { name: 'Acme', id: 'acme' },
    });
    await app.goto(`${harness.url}/workspaces`);

    await app.getByRole('link', { name: 'Edit Acme' }).click();
    await expect(app).toHaveURL(/\/workspaces\/acme$/u);
    await expect(app.getByLabel('Folder')).toHaveValue('acme');

    await app.getByLabel('Name').fill('Acme Ltd');
    await app.getByRole('button', { name: 'Save changes' }).click();

    await expect
      .poll(async () => await workspacesOf(app, harness.url))
      .toContainEqual({
        id: 'acme',
        name: 'Acme Ltd',
      });
  });

  test('moving the folder renames the directory and takes the sessions', async ({
    app,
    harness,
  }) => {
    // The assertion only a real stack can make: the tree on disk, the registry
    // row and the sessions that named the old folder all end up agreeing. Each
    // of the three is a different store, and nothing below this level sees more
    // than one of them.
    await app.request.post(`${harness.url}/api/workspaces`, {
      data: { name: 'Acme', id: 'acme' },
    });
    await app.request.put(`${harness.url}/api/files/text`, {
      data: { path: 'brief.md', content: 'the brief', workspaceId: 'acme' },
    });
    await app.request.post(`${harness.url}/api/sessions`, {
      data: { key: 'web-acme-1', workspaceId: 'acme' },
    });

    await app.goto(`${harness.url}/workspaces/acme`);
    await app.getByLabel('Folder').fill('acme24');
    await app.getByRole('button', { name: 'Save changes' }).click();

    // The durable state: the editor is now on the new folder's URL.
    await expect(app).toHaveURL(/\/workspaces\/acme24$/u);
    await expect
      .poll(async () => await workspacesOf(app, harness.url))
      .toContainEqual({
        id: 'acme24',
        name: 'Acme',
      });

    // The files came with it, and are reachable under the new folder alone.
    await app.goto(`${harness.url}/files?workspace=acme24`);
    await expect(
      app.getByRole('link', { name: 'brief.md', exact: true }),
    ).toBeVisible();

    // And so did the session, which would otherwise resolve to a folder
    // that is not there any more.
    const sessions = await app.request.get(
      `${harness.url}/api/sessions?workspace=acme24`,
    );
    expect(
      (
        (await sessions.json()) as { sessions: Array<{ key: string }> }
      ).sessions.map((row) => row.key),
    ).toContain('web-acme-1');
  });

  test('the default workspace has no folder to move', async ({
    app,
    harness,
  }) => {
    await app.goto(`${harness.url}/workspaces/default`);

    // Its directory *is* the root every other workspace sits inside, so the box
    // is inert rather than absent, states `/`, and says why it cannot move.
    await expect(app.getByLabel('Folder')).toBeDisabled();
    await expect(app.getByLabel('Folder')).toHaveValue('/');
    await expect(app.getByText(/no folder of its own to move/)).toBeVisible();
  });

  test('the Files tree walks into a workspace, and they do not see each other', async ({
    app,
    harness,
  }) => {
    // Two workspaces, made through the API the manager uses. The manager has
    // its own component test; what this spec is here for is what happens after.
    for (const name of ['Acme', 'Research']) {
      const created = await app.request.post(`${harness.url}/api/workspaces`, {
        data: { name, id: name.toLowerCase() },
      });
      expect(created.ok(), `creating ${name} should succeed`).toBe(true);
    }

    await app.request.put(`${harness.url}/api/files/text`, {
      data: { path: 'acme-only.md', content: 'a', workspaceId: 'acme' },
    });
    await app.request.put(`${harness.url}/api/files/text`, {
      data: { path: 'research-only.md', content: 'r', workspaceId: 'research' },
    });

    await app.goto(`${harness.url}/files`);

    // Default is the parent of the others, so it sees them as folders — which
    // is the layout working, not a leak.
    await expect(
      app.getByRole('link', { name: 'acme', exact: true }),
    ).toBeVisible();

    await openFolder(app, 'acme');
    await expect(
      app.getByRole('link', { name: 'acme-only.md', exact: true }),
    ).toBeVisible();
    await expect(
      app.getByRole('link', { name: 'research-only.md', exact: true }),
    ).toHaveCount(0);

    // Straight to the other one by URL, which is what a link out of the
    // workspaces manager does. The assertion the query key exists for: a cached
    // listing from the previous workspace would still be on screen here.
    await app.goto(`${harness.url}/files?workspace=research`);
    await expect(
      app.getByRole('link', { name: 'research-only.md', exact: true }),
    ).toBeVisible();
    await expect(
      app.getByRole('link', { name: 'acme-only.md', exact: true }),
    ).toHaveCount(0);
  });

  test('the composer picker moves the conversation, and the next turn follows', async ({
    app,
    harness,
  }) => {
    // The whole point of the feature, and the one assertion only a browser can
    // make: the picker writes the binding, and the *next* turn resolves its jail
    // from that binding rather than from the workspace the session was born in.
    const created = await app.request.post(`${harness.url}/api/workspaces`, {
      data: { name: 'Research', id: 'research' },
    });
    expect(created.ok()).toBe(true);
    await app.request.put(`${harness.url}/api/files/text`, {
      data: {
        path: 'research-only.md',
        content: 'r',
        workspaceId: 'research',
      },
    });
    // The picker's listing is fetched on mount, so a workspace created after
    // the page loaded is not in it yet.
    await app.reload();

    // A first turn, so the session has a row for the picker to move.
    await app.getByRole('textbox', { name: 'Message' }).fill('list the files');
    await app.getByRole('button', { name: 'Send' }).click();
    await expect(
      app.getByRole('region', { name: 'Tool call: list_dir' }),
    ).toBeVisible();

    const key = new URL(app.url()).searchParams.get('session') ?? '';
    expect(key).not.toBe('');

    await app
      .getByRole('button', { name: /^Workspace for this session: / })
      .click();
    await app.getByRole('menuitemradio', { name: 'Research' }).click();

    // The durable half: the binding the server holds. Not the toast, and not
    // the moment the sidebar re-scopes.
    await expect
      .poll(async () => {
        const response = await app.request.get(
          `${harness.url}/api/sessions/${key}`,
        );
        return ((await response.json()) as { workspaceId: string }).workspaceId;
      })
      .toBe('research');

    // The half that proves it reached the jail: the next turn's `list_dir` sees
    // the new workspace's files and not the old one's.
    await app.getByRole('textbox', { name: 'Message' }).fill('list the files');
    await app.getByRole('button', { name: 'Send' }).click();

    const cards = app.getByRole('region', { name: 'Tool call: list_dir' });
    await expect(cards).toHaveCount(2);
    const second = cards.nth(1);
    await second.getByRole('button', { expanded: false }).click();
    await expect(second.getByText('research-only.md')).toBeVisible();
    await expect(second.getByText('notes.md')).toHaveCount(0);
  });

  test('the workspace survives a reload, because it is in the URL', async ({
    app,
    harness,
  }) => {
    const created = await app.request.post(`${harness.url}/api/workspaces`, {
      data: { name: 'Acme', id: 'acme' },
    });
    expect(created.ok()).toBe(true);
    await app.request.put(`${harness.url}/api/files/text`, {
      data: { path: 'acme-only.md', content: 'a', workspaceId: 'acme' },
    });

    await app.goto(`${harness.url}/files?workspace=acme`);
    await expect(
      app.getByRole('link', { name: 'acme-only.md', exact: true }),
    ).toBeVisible();

    await app.reload();
    await expect(
      app.getByRole('link', { name: 'acme-only.md', exact: true }),
    ).toBeVisible();
  });

  test('a workspace with sessions cannot be deleted until they move', async ({
    app,
    harness,
  }) => {
    await app.request.post(`${harness.url}/api/workspaces`, {
      data: { name: 'Acme', id: 'acme' },
    });
    await app.request.post(`${harness.url}/api/sessions`, {
      data: { key: 'web-acme-1', workspaceId: 'acme' },
    });

    const refused = await app.request.delete(
      `${harness.url}/api/workspaces/acme`,
    );
    expect(refused.status()).toBe(409);
    // The count is what the dialog turns into its offer.
    expect(
      (
        (await refused.json()) as {
          error: { details: { sessionCount: number } };
        }
      ).error.details.sessionCount,
    ).toBe(1);

    const moved = await app.request.post(
      `${harness.url}/api/workspaces/acme/sessions/move`,
      {
        data: { to: 'default' },
      },
    );
    expect(moved.ok()).toBe(true);
    expect(
      (await app.request.delete(`${harness.url}/api/workspaces/acme`)).status(),
    ).toBe(204);
  });

  test('the sidebar has no workspace control, and a way to the manager', async ({
    app,
  }) => {
    // The switcher that used to sit at the top of this column is gone: it
    // scoped the session list, which made a conversation moved to another
    // workspace vanish from it. Which workspace a conversation uses is chosen
    // in the composer now, beside the agent.
    const sidebar = app.getByRole('complementary', { name: 'Sidebar' });

    await expect(
      sidebar.getByRole('button', { name: /^Workspace: /u }),
    ).toHaveCount(0);

    // And the manager is still reachable, which it would not be without a row:
    // the switcher's menu used to be the only door.
    await sidebar.getByRole('link', { name: 'Workspaces' }).click();
    await expect(
      app.getByRole('heading', { name: 'Workspaces' }),
    ).toBeVisible();
  });
});
