/**
 * Adding, editing and removing an MCP server, in a browser.
 *
 * **No assertion here touches a connection state**, and that is the whole
 * design of this file rather than an omission. Whether a row reads `Connecting`,
 * `Unreachable` or `Connected` depends on when a background dial settles
 * against a server that does not exist, and a browser test has no way to hold
 * that still — it is exactly the shape of the `approvals.spec.ts` flake that was
 * red in CI four runs running while green on every laptop. Those states are
 * asserted in `packages/web/test/settings/mcp.test.tsx`, where the status
 * response is a fixture.
 *
 * What is left is what only a browser can show: that the panel, the editor, the
 * two patch shapes and the settings route are wired to each other. Every
 * assertion is on **durable** state read back through the API.
 */

import type { Page } from '@playwright/test';

import { expect, test } from '../src/fixtures.js';

interface McpEntry {
  readonly type?: string;
  readonly command?: string;
  readonly args?: readonly string[];
  readonly url?: string;
  readonly enabled?: boolean;
  readonly enabledTools?: readonly string[];
}

interface SettingsView {
  readonly config: {
    readonly tools: { readonly mcpServers: Record<string, McpEntry> };
  };
}

const serversOf = async (
  app: Page,
  url: string,
): Promise<Record<string, McpEntry>> => {
  const response = await app.request.get(`${url}/api/settings`);
  return ((await response.json()) as SettingsView).config.tools.mcpServers;
};

/**
 * One server's card.
 *
 * Scoped to the MCP servers list rather than swept off the page: the shell has
 * lists of its own — the sidebar's sessions, any open menu — and a bare
 * `getByRole('listitem')` picks them up too.
 */
const serverRow = (app: Page, name: string) =>
  app
    .getByRole('list', { name: 'MCP servers' })
    .getByRole('listitem')
    .filter({ hasText: name });

/** One entry, so a spec that edits has something to open. */
async function seed(app: Page, url: string): Promise<void> {
  await app.request.patch(`${url}/api/settings`, {
    data: {
      tools: {
        mcpServers: { files: { command: 'not-a-real-command', args: ['--x'] } },
      },
    },
  });
}

test.describe('MCP servers', () => {
  test('the MCP servers panel is a form, and promises nothing else', async ({
    app,
    harness,
  }) => {
    await app.goto(`${harness.url}/settings?panel=mcp`);

    await expect(
      app.getByRole('link', { name: 'New MCP server' }),
    ).toBeVisible();
    // This screen used to end in a "Still to come" list naming OAuth, extensions
    // and skills. It is gone: a settings panel advertising a form an operator
    // cannot open is one they check twice, and skills were never going to get
    // a screen at all — a skill is a folder in the workspace.
    for (const gone of ['Still to come', 'OAuth connections', 'Skills']) {
      await expect(app.getByText(gone)).toHaveCount(0);
    }
  });

  test('creating one writes nothing until Save', async ({ app, harness }) => {
    await app.goto(`${harness.url}/settings?panel=mcp`);
    await app.getByRole('link', { name: 'New MCP server' }).click();

    await app.getByLabel('Name').fill('files');
    await app.getByLabel('Command').fill('not-a-real-command');

    // Nothing yet: an abandoned editor must not leave a server behind.
    expect(await serversOf(app, harness.url)).toEqual({});

    await app.getByRole('button', { name: 'Save changes' }).click();

    await expect
      .poll(async () => (await serversOf(app, harness.url)).files)
      .toMatchObject({ type: 'stdio', command: 'not-a-real-command' });
  });

  test('the row survives a reload, because it is configuration', async ({
    app,
    harness,
  }) => {
    await seed(app, harness.url);
    await app.goto(`${harness.url}/settings?panel=mcp`);

    await expect(serverRow(app, 'files')).toBeVisible();
    await app.reload();
    await expect(serverRow(app, 'files')).toBeVisible();
  });

  test('the editor opens on the stored settings and saves a change', async ({
    app,
    harness,
  }) => {
    await seed(app, harness.url);
    await app.goto(`${harness.url}/settings/mcp/files`);

    await expect(app.getByLabel('Command')).toHaveValue('not-a-real-command');
    await app.getByLabel('Enabled tools').fill('read_file');
    await app.getByRole('button', { name: 'Save changes' }).click();

    await expect
      .poll(async () => (await serversOf(app, harness.url)).files?.enabledTools)
      .toEqual(['read_file']);
  });

  test('switching the transport clears the half that no longer applies', async ({
    app,
    harness,
  }) => {
    // The config would refuse itself otherwise: an entry naming both a command
    // and a url is not a server, and the merge would have left the old one.
    await seed(app, harness.url);
    await app.goto(`${harness.url}/settings/mcp/files`);

    await app.getByRole('combobox', { name: 'Transport' }).click();
    await app.getByRole('option', { name: 'Streamable HTTP' }).click();
    await app.getByLabel('URL').fill('http://127.0.0.1:39999/mcp');
    await app.getByRole('button', { name: 'Save changes' }).click();

    await expect
      .poll(async () => (await serversOf(app, harness.url)).files)
      .toMatchObject({
        type: 'streamableHttp',
        url: 'http://127.0.0.1:39999/mcp',
        command: '',
        args: [],
      });
  });

  test('switching one off keeps it, and deleting asks first', async ({
    app,
    harness,
  }) => {
    await seed(app, harness.url);
    await app.goto(`${harness.url}/settings?panel=mcp`);

    await app.getByRole('button', { name: 'Actions for files' }).click();
    await app.getByRole('menuitem', { name: 'Disable' }).click();
    await expect
      .poll(async () => (await serversOf(app, harness.url)).files?.enabled)
      .toBe(false);

    await app.getByRole('button', { name: 'Actions for files' }).click();
    await app.getByRole('menuitem', { name: 'Delete' }).click();
    await expect(app.getByRole('dialog')).toContainText('files');
    await app.getByRole('button', { name: 'Delete' }).click();

    // Gone from the tree, not merely from the screen.
    await expect
      .poll(async () => await serversOf(app, harness.url))
      .toEqual({});
  });
});
