/**
 * The text editor's geometry, in a real browser.
 *
 * This is the one thing no unit test can hold. The editor is a transparent
 * `<textarea>` over a coloured `<pre>`, and whether the two cover the same area
 * is a question about *layout* — jsdom has no layout engine, so a component
 * test cannot tell a correct stack from one where the colour layer stops at the
 * fold and the transparent text below it renders as nothing at all.
 *
 * That was the bug: `.code-editor__code` is a flex item stretched to the
 * scroller's own height, the textarea grows past it (`field-sizing: content`),
 * and the highlight layer — `position: absolute; inset: 0` against that box —
 * ended one screenful down. Past the fold there was no colour and the textarea
 * text was transparent, so a long file scrolled into an empty rectangle.
 */

import { expect, test } from '../src/fixtures.js';

/** Long enough to scroll several times over, and syntactic enough to colour. */
const LONG_FILE = Array.from(
  { length: 300 },
  (unused, index) =>
    `export const value${String(index)} = ${String(index)}; // line ${String(index)}`,
).join('\n');

test.describe('the text editor', () => {
  test('paints the whole file, not just the first screenful', async ({
    app,
    harness,
  }) => {
    await app.request.put(`${harness.url}/api/files/text`, {
      data: { path: 'long.ts', content: LONG_FILE, workspaceId: 'default' },
    });

    await app.goto(`${harness.url}/files`);
    await app.getByRole('link', { name: 'long.ts', exact: true }).click();

    const editor = app.locator('.code-editor');
    await expect(editor).toBeVisible();

    // The colouring is async — it lands on idle — so wait for the layer rather
    // than racing it. Its presence is what makes the textarea text transparent,
    // which is what makes the geometry below load-bearing.
    const highlight = editor.locator('.code-editor__highlight');
    await expect(highlight).toBeAttached();

    const geometry = await editor.evaluate((scroller) => {
      const heightOf = (selector: string): number =>
        scroller.querySelector<HTMLElement>(selector)?.offsetHeight ?? 0;
      return {
        scrollHeight: scroller.scrollHeight,
        clientHeight: scroller.clientHeight,
        highlight: heightOf('.code-editor__highlight'),
        input: heightOf('.code-editor__input'),
        gutter: heightOf('.code-editor__gutter'),
      };
    });

    // The file is genuinely taller than the box, or the test proves nothing.
    expect(geometry.scrollHeight).toBeGreaterThan(geometry.clientHeight * 2);

    // The assertion the bug failed: the colour layer is as tall as the text it
    // is colouring. Before the fix this was one viewport while `input` was the
    // whole file.
    expect(geometry.highlight).toBeGreaterThanOrEqual(geometry.input - 2);

    // And the gutter runs the whole way too, or the numbers stop at the fold.
    expect(geometry.gutter).toBeGreaterThanOrEqual(geometry.input - 2);
  });

  test('still shows text after scrolling to the bottom', async ({
    app,
    harness,
  }) => {
    await app.request.put(`${harness.url}/api/files/text`, {
      data: { path: 'long.ts', content: LONG_FILE, workspaceId: 'default' },
    });

    await app.goto(`${harness.url}/files`);
    await app.getByRole('link', { name: 'long.ts', exact: true }).click();

    const editor = app.locator('.code-editor');
    await expect(editor).toBeVisible();
    await expect(editor.locator('.code-editor__highlight')).toBeAttached();

    await editor.evaluate((node) => {
      node.scrollTop = node.scrollHeight;
    });

    // Something is painted where the reader is looking. `value299` is the last
    // line of the file, and the only thing that can render it down there is a
    // highlight layer that reached the bottom. Scoped to that layer because the
    // textarea holds the same string and would match too.
    const last = editor
      .locator('.code-editor__highlight')
      .getByText('value299', { exact: true });
    await expect(last).toBeVisible();

    // And it is inside the part of the box the reader can see, rather than
    // merely existing somewhere below it.
    const visible = await editor.evaluate((node) => {
      const scroller = node.getBoundingClientRect();
      const spans = [...node.querySelectorAll('.code-editor__highlight span')];
      const target = spans.find((span) => span.textContent === 'value299');
      if (target === undefined) return false;
      const box = target.getBoundingClientRect();
      return box.top >= scroller.top && box.bottom <= scroller.bottom;
    });
    expect(visible).toBe(true);
  });
});

/**
 * Renaming, end to end.
 *
 * Worth a browser because it is the one file operation whose *result* is a
 * different row in a listing rather than a response body: the dialog sends two
 * paths, the server moves the tree, and the page has to re-fetch to show it. A
 * component test can prove the request; only this can prove the round trip.
 */
test.describe('renaming', () => {
  test('renames a folder and everything inside it comes along', async ({
    app,
    harness,
  }) => {
    await app.request.put(`${harness.url}/api/files/text`, {
      data: { path: 'drafts/one.md', content: 'first', workspaceId: 'default' },
    });

    await app.goto(`${harness.url}/files`);
    await app.getByRole('button', { name: 'Actions for drafts' }).click();
    await app.getByRole('menuitem', { name: 'Rename' }).click();

    const field = app.getByRole('textbox', { name: 'New name' });
    await field.fill('published');
    await app.getByRole('button', { name: 'Rename' }).click();

    // The durable result: the listing holds the new name and not the old one.
    await expect(
      app.getByRole('link', { name: 'published', exact: true }),
    ).toBeVisible();
    await expect(
      app.getByRole('link', { name: 'drafts', exact: true }),
    ).toHaveCount(0);

    // And the tree moved rather than being recreated empty.
    const moved = await app.request.get(
      `${harness.url}/api/files/text?path=published/one.md&workspace=default`,
    );
    expect(moved.ok()).toBe(true);
    expect(((await moved.json()) as { content: string }).content).toBe('first');
  });

  test('refuses to rename onto something that is already there', async ({
    app,
    harness,
  }) => {
    await app.request.put(`${harness.url}/api/files/text`, {
      data: { path: 'keep.md', content: 'keep me', workspaceId: 'default' },
    });
    await app.request.put(`${harness.url}/api/files/text`, {
      data: { path: 'other.md', content: 'and me', workspaceId: 'default' },
    });

    await app.goto(`${harness.url}/files`);
    await app.getByRole('button', { name: 'Actions for other.md' }).click();
    await app.getByRole('menuitem', { name: 'Rename' }).click();

    await app.getByRole('textbox', { name: 'New name' }).fill('keep.md');
    await app.getByRole('button', { name: 'Rename' }).click();

    // The dialog stays open holding the name that was rejected — a failed
    // rename is a name to correct, not a screen to be sent back to. Asserted
    // rather than the error toast beside it, which auto-dismisses: a state that
    // is only visible because the machine was slow does not belong in an
    // `expect`, and this spec failed exactly that way under a loaded runner
    // while passing on its own.
    await expect(app.getByRole('textbox', { name: 'New name' })).toHaveValue(
      'keep.md',
    );

    // The durable half: neither file moved and neither was overwritten. A
    // rename that silently replaced the file already there would be a loss the
    // operator could not see afterwards.
    const untouched: ReadonlyArray<readonly [string, string]> = [
      ['keep.md', 'keep me'],
      ['other.md', 'and me'],
    ];

    for (const [path, content] of untouched) {
      const response = await app.request.get(
        `${harness.url}/api/files/text?path=${path}&workspace=default`,
      );
      expect(response.ok(), `${path} should still be there`).toBe(true);
      expect(((await response.json()) as { content: string }).content).toBe(
        content,
      );
    }
  });
});
