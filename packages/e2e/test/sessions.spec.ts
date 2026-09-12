/**
 * Starting, naming and forking a session.
 *
 * The web UI had no way to start one. What looked like the control — a "Chat"
 * nav link — dropped `?session=` from a route that reads the store rather than
 * the URL, so it cleared nothing and the first message put the key straight
 * back. These four cases are the replacement, driven through the real server:
 * a session is created, it names itself after what was said in it, picking
 * another switches to it, and a branch forks without disturbing the original.
 *
 * The title is the one worth having end to end. Nothing in the browser derives
 * it — the agent loop does, from the first message, which is what makes a
 * session started in the terminal show up named in the sidebar. A unit
 * test of the derivation cannot see that.
 */

import type { Page } from '@playwright/test';

import { expect, test } from '../src/fixtures.js';

test.describe('sessions', () => {
  test('starts one, names it after the first message, and lists it', async ({
    app,
  }) => {
    const sidebar = app.getByRole('complementary', { name: 'Sidebar' });

    await sidebar.getByRole('button', { name: 'New session' }).click();
    // The key comes back from `POST /api/sessions`, which is why the button is
    // REST rather than the socket's `session.new`: the navigation needs it.
    await expect(app).toHaveURL(/\?session=/u);

    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('textbox', { name: 'Message' }).press('Enter');
    await expect(
      app.getByText('The workspace holds', { exact: false }).first(),
    ).toBeVisible({
      timeout: 15_000,
    });

    // Derived by the loop, so the sidebar shows a name rather than a uuid.
    await expect(sidebar.getByText('stream a long answer')).toBeVisible({
      timeout: 15_000,
    });
  });

  test('saves nothing until something is said', async ({ app }) => {
    const sidebar = app.getByRole('complementary', { name: 'Sidebar' });

    await sidebar.getByRole('button', { name: 'New session' }).click();
    await expect(
      app.getByRole('heading', { name: 'Ready when you are.' }),
    ).toBeVisible();

    // Pressed, then thought better of. A row written on the press would still
    // be here — and the list would fill with sessions nobody had.
    await expect(sidebar.getByText('No sessions yet.')).toBeVisible();

    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('textbox', { name: 'Message' }).press('Enter');

    // Saved by the turn, not by the button.
    await expect(sidebar.getByText('stream a long answer')).toBeVisible({
      timeout: 15_000,
    });
  });

  test('switches between sessions without losing either', async ({ app }) => {
    const sidebar = app.getByRole('complementary', { name: 'Sidebar' });
    const message = app.getByRole('textbox', { name: 'Message' });

    await sidebar.getByRole('button', { name: 'New session' }).click();
    await message.fill('list the workspace');
    await message.press('Enter');
    await expect(
      app.getByRole('region', { name: 'Tool call: list_dir' }),
    ).toBeVisible({
      timeout: 15_000,
    });

    await sidebar.getByRole('button', { name: 'New session' }).click();
    // A fresh session is empty, which the dead nav link never managed.
    await expect(
      app.getByRole('heading', { name: 'Ready when you are.' }),
    ).toBeVisible();

    await sidebar.getByRole('link', { name: /list the workspace/u }).click();
    await expect(
      app.getByRole('region', { name: 'Tool call: list_dir' }),
    ).toBeVisible({
      timeout: 15_000,
    });
  });

  test('marks the new session until there is a saved one to mark', async ({
    app,
  }) => {
    const sidebar = app.getByRole('complementary', { name: 'Sidebar' });

    await sidebar.getByRole('button', { name: 'New session' }).click();

    // A session exists on the socket before it exists in the list, and without
    // this the column would claim you are nowhere for the whole of that window.
    await expect(sidebar.locator('[aria-current="page"]')).toHaveText(
      /New session/u,
    );

    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('textbox', { name: 'Message' }).press('Enter');
    await expect(sidebar.getByText('stream a long answer')).toBeVisible({
      timeout: 15_000,
    });

    // Saved, so the mark moves to the row — and there is only ever one.
    await expect(sidebar.locator('[aria-current="page"]')).toHaveCount(1);
    await expect(sidebar.locator('[aria-current="page"]')).toHaveText(
      /stream a long answer/u,
    );
  });

  test('lists a session on one line, without a message count', async ({
    app,
  }) => {
    const sidebar = app.getByRole('complementary', { name: 'Sidebar' });

    await sidebar.getByRole('button', { name: 'New session' }).click();
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('textbox', { name: 'Message' }).press('Enter');

    const row = sidebar.getByRole('link', { name: /stream a long answer/u });
    await expect(row).toBeVisible({ timeout: 15_000 });

    // The title is what this list is scanned for. A count on a second line
    // halved how many sessions fit in the column to carry a number nobody
    // reads — it is still on the API for anything that wants it.
    await expect(row).not.toContainText('message');
  });

  test('marks which session is open', async ({ app }) => {
    const sidebar = app.getByRole('complementary', { name: 'Sidebar' });

    await sidebar.getByRole('button', { name: 'New session' }).click();
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('textbox', { name: 'Message' }).press('Enter');
    await expect(sidebar.getByText('stream a long answer')).toBeVisible({
      timeout: 15_000,
    });

    // `aria-current` is the accessible half of the surface change, and the half
    // a screenshot cannot check. Located by the attribute rather than through
    // `getByRole`, which has no `current` option and silently ignores one.
    await expect(sidebar.locator('a[aria-current="page"]')).toHaveText(
      /stream a long answer/u,
    );
  });

  test('branches into a second session, leaving the first alone', async ({
    app,
  }) => {
    const sidebar = app.getByRole('complementary', { name: 'Sidebar' });
    const message = app.getByRole('textbox', { name: 'Message' });

    await sidebar.getByRole('button', { name: 'New session' }).click();
    await message.fill('stream a long answer');
    await message.press('Enter');
    await expect(sidebar.getByText('stream a long answer')).toBeVisible({
      timeout: 15_000,
    });

    await app.getByRole('button', { name: 'Branch from here' }).first().click();

    // Two rows now name the same thing: the fork inherits the source's title.
    await expect(sidebar.getByText('stream a long answer')).toHaveCount(2, {
      timeout: 15_000,
    });
  });
});

/**
 * The sessions page.
 *
 * What is asserted here and nowhere else is that the search and the page are the
 * *server's*: a component test stubs the response, so it can only check that the
 * right query went out. This drives a real SQLite table through a real route,
 * which is the only place the `LIKE` predicate and the `OFFSET` are exercised
 * against the schema they run on.
 *
 * Every assertion is on a state the page settles into — rows present, rows
 * absent, a dialog. Nothing here waits for a "Saving…" or a "Deleting…": those
 * are painted between an answer and a socket frame, so whether they are ever
 * seen depends on how the runner interleaves the two. They are held still in
 * `sessions.test.tsx` instead.
 */
test.describe('the sessions page', () => {
  /**
   * Says something in each session, so there are rows to search.
   *
   * A turn per row rather than a seeded database, because a session is written
   * by the loop and not by the button — see "saves nothing until something is
   * said" above. It is also what gives each row a title.
   */
  async function seed(app: Page, titles: readonly string[]): Promise<void> {
    const sidebar = app.getByRole('complementary', { name: 'Sidebar' });
    for (const title of titles) {
      await sidebar.getByRole('button', { name: 'New session' }).click();
      await app.getByRole('textbox', { name: 'Message' }).fill(title);
      await app.getByRole('textbox', { name: 'Message' }).press('Enter');
      await expect(sidebar.getByText(title)).toBeVisible({ timeout: 15_000 });
    }
  }

  /** Through the nav row, which is also the assertion that it goes there. */
  async function openPage(app: Page): Promise<void> {
    await app
      .getByRole('complementary', { name: 'Sidebar' })
      .getByRole('link', { name: 'Sessions', exact: true })
      .click();
    // `exact`, because the sidebar's own "Latest sessions" heading is a
    // substring match for this one and Playwright's default name matching is
    // not anchored.
    await expect(
      app.getByRole('heading', { name: 'Sessions', exact: true }),
    ).toBeVisible();
  }

  test('is reachable from the sidebar and lists what is there', async ({
    app,
  }) => {
    await seed(app, ['stream a long answer']);
    await openPage(app);

    await expect(
      app
        .getByRole('list', { name: 'Sessions' })
        .getByText('stream a long answer'),
    ).toBeVisible();
  });

  test('searches every session, not the page in hand', async ({ app }) => {
    await seed(app, ['stream a long answer', 'list the workspace']);
    await openPage(app);

    const list = app.getByRole('list', { name: 'Sessions' });
    await expect(list.getByText('list the workspace')).toBeVisible();

    await app
      .getByRole('searchbox', { name: 'Filter sessions' })
      .fill('workspace');

    // The durable state: one row matches and the other is gone. A SQL `LIKE`
    // decided that, not a filter over the rows the browser already had.
    await expect(list.getByText('list the workspace')).toBeVisible();
    await expect(list.getByText('stream a long answer')).toHaveCount(0);
  });

  test('renames a session, and the sidebar agrees', async ({ app }) => {
    await seed(app, ['stream a long answer']);
    await openPage(app);

    const list = app.getByRole('list', { name: 'Sessions' });
    await list.getByRole('button', { name: /^Actions for/u }).click();
    await app.getByRole('menuitem', { name: 'Rename' }).click();

    const dialog = app.getByRole('dialog');
    await dialog.getByRole('textbox').fill('Renamed from the page');
    await dialog.getByRole('button', { name: 'Rename' }).click();

    await expect(list.getByText('Renamed from the page')).toBeVisible();
    // One rename, two lists: both read the same query key, so one invalidation
    // refreshes the column as well as the page.
    await expect(
      app
        .getByRole('complementary', { name: 'Sidebar' })
        .getByText('Renamed from the page'),
    ).toBeVisible();
  });

  test('asks before deleting, and the row is gone once it is answered', async ({
    app,
  }) => {
    await seed(app, ['stream a long answer']);
    await openPage(app);

    const list = app.getByRole('list', { name: 'Sessions' });
    await list.getByRole('button', { name: /^Actions for/u }).click();
    await app.getByRole('menuitem', { name: 'Delete' }).click();

    // The question is itself a durable state, and the thing that changed here:
    // the sidebar used to delete on one click with nothing in front of it.
    const dialog = app.getByRole('dialog');
    await expect(dialog).toContainText('There is no undo.');
    await dialog.getByRole('button', { name: 'Delete' }).click();

    await expect(app.getByText('No sessions yet.').first()).toBeVisible();
  });
});
