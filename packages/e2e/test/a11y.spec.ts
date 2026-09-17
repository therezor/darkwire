/**
 * The four things the product being replaced could not do.
 *
 * None of them is a feature. Each is a property of the whole app that no single
 * component owns, which is exactly why they are asserted in a browser and not
 * anywhere else: a reflow at 200% is a statement about the layout system, a
 * focus ring is a statement about the base layer surviving every component
 * written since, and a focus trap is a statement about a library actually being
 * wired up rather than merely installed.
 *
 * The style guide is the target for the keyboard sweep because it instantiates
 * every primitive on one page. A primitive added without a variant there is a
 * primitive nothing reviews — and now also a primitive nothing tabs to.
 */

import type { Page } from '@playwright/test';

import { expect, test } from '../src/fixtures.js';
import { NARROW_VIEWPORT } from '../src/viewport.js';

/**
 * Doubles the browser's font size, the way a browser setting does.
 *
 * Every length in this UI is a rem and the root font size is never overridden,
 * so the root element's computed size *is* the browser setting as far as the
 * stylesheet is concerned. Writing it directly is the same input by a shorter
 * path than launching Chrome with a different default.
 */
async function doubleFontSize(page: Page): Promise<void> {
  await page.addStyleTag({ content: 'html { font-size: 200% }' });
}

/** True when the page itself scrolls sideways — the classic reflow failure. */
async function scrollsHorizontally(page: Page): Promise<boolean> {
  return await page.evaluate(() => {
    const root = document.documentElement;
    // A pixel of slack: a fractional layout width rounds up, and a one-pixel
    // overflow nobody can scroll to is not the bug this is looking for.
    return (
      root.scrollWidth > root.clientWidth + 1 ||
      document.body.scrollWidth > root.clientWidth + 1
    );
  });
}

/**
 * True when the *document* scrolls vertically.
 *
 * The app is a fixed-height shell: `.shell` is `100dvh` and `.shell__main` is
 * the one vertical scrollport in it. A second scrollbar on the document means
 * something has stretched the page past the viewport, and the symptom an
 * operator sees is two scrollbars side by side plus a strip of background below
 * the shell once the outer one moves.
 */
async function scrollsVertically(page: Page): Promise<boolean> {
  return await page.evaluate(() => {
    const root = document.documentElement;
    // A pixel of slack, for the same reason the horizontal check takes one.
    return root.scrollHeight > root.clientHeight + 1;
  });
}

/**
 * Everything painted past the right edge of the column that is supposed to
 * clip it.
 *
 * The companion to `scrollsHorizontally`, and it catches the *other* half of
 * the problem — the half that one misses entirely. Both of the app's two
 * columns clip their own overflow, so content too wide for them does not make
 * the page scroll: it is simply cut off mid-character, with no ellipsis and
 * nothing in the console. A page can therefore be "not scrolling sideways" and
 * still be hiding half a URL.
 *
 * Asserted against the two clipping columns rather than against a class,
 * because the next component to get this wrong will get it wrong its own way.
 * Anything portalled — a dialog, a popover, a tooltip — is outside both by
 * construction and is not being measured here.
 *
 * What *is* excused is content inside something that scrolls sideways on
 * purpose. The settings tab strip is the case: it is `overflow-x: auto` because
 * a row of eight panel names is meant to scroll rather than wrap, and on a
 * phone its last few tabs sit past the column edge by design. They are one
 * swipe away, not lost — which is the distinction this is trying to draw, and
 * it does not show up at 1440 because there the strip fits.
 */
async function escapees(page: Page): Promise<readonly string[]> {
  return await page.evaluate(() => {
    const found: string[] = [];
    for (const selector of ['aside[aria-label="Sidebar"]', 'main']) {
      const column = document.querySelector(selector);
      if (column === null) continue;
      const edge = column.getBoundingClientRect().right;

      const inItsOwnScroller = (element: Element): boolean => {
        for (
          let at = element.parentElement;
          at !== null && at !== column;
          at = at.parentElement
        ) {
          const overflow = getComputedStyle(at).overflowX;
          if (overflow === 'auto' || overflow === 'scroll') return true;
        }
        return false;
      };

      for (const element of column.querySelectorAll('*')) {
        const box = element.getBoundingClientRect();
        // A pixel of slack for a fractional layout width, and skip what is not
        // painted at all.
        if (box.width === 0 || box.right <= edge + 1) continue;
        if (inItsOwnScroller(element)) continue;
        const text = element.textContent.trim().slice(0, 40);
        found.push(`${selector} > ${element.tagName.toLowerCase()}: "${text}"`);
      }
    }
    return found;
  });
}

/**
 * Every screen that has a layout of its own.
 *
 * The two settings entries are not redundant. `/settings` with no query lands
 * on the first panel, so for as long as that was the only entry here the
 * providers list — the widest thing in the app, and the one that actually broke
 * on a phone — was never on screen while any of these assertions ran. A screen
 * reached through a query parameter is still a screen.
 *
 * `/agents/default` is here for the same reason: the index was covered and the
 * editor, which is where the tool and subagent rows live, was not.
 */
const SCREENS: ReadonlyArray<{ readonly path: string; readonly name: string }> =
  [
    { path: '/', name: 'chat' },
    { path: '/agents', name: 'agents' },
    { path: '/agents/default', name: 'agent editor' },
    { path: '/workspaces', name: 'workspaces' },
    { path: '/files', name: 'files' },
    { path: '/notifications', name: 'notifications' },
    { path: '/settings', name: 'settings' },
    { path: '/settings?panel=providers', name: 'providers' },
    { path: '/settings?panel=environments', name: 'environments' },
    { path: '/tokens', name: 'style guide' },
  ];

test.describe('at 200% font size', () => {
  for (const screen of SCREENS) {
    test(`${screen.name} reflows without a horizontal scrollbar`, async ({
      app,
      harness,
    }) => {
      await app.goto(`${harness.url}${screen.path}`);
      await expect(
        app.getByRole('complementary', { name: 'Sidebar' }),
      ).toBeVisible();
      await doubleFontSize(app);

      expect(await scrollsHorizontally(app)).toBe(false);
    });
  }

  test('the composer is still usable', async ({ app }) => {
    await doubleFontSize(app);

    // Visible *and* hit-testable: an element pushed under another one is
    // `toBeVisible` and useless, and `click` is what tells them apart.
    const message = app.getByRole('textbox', { name: 'Message' });
    await expect(message).toBeVisible();
    await message.click();
    await message.fill('stream a long answer');
    await app.getByRole('button', { name: 'Send' }).click();

    await expect(
      app.getByTestId('transcript').getByText('Here is what I found.'),
    ).toBeVisible();
    expect(await scrollsHorizontally(app)).toBe(false);
  });
});

/**
 * The vertical counterpart to the reflow sweeps above, and it caught a real one.
 *
 * `.sr-only` was `position: absolute` with no offsets, so it kept its *static
 * position* — where it would have sat in flow — while its containing block was
 * the document, there being no positioned ancestor. Inside a tall scrollport
 * both are true at once: a clipped hairline landed a thousand pixels down,
 * measured against the page, and stretched the page to reach it.
 *
 * Neither existing sensor could see it. It scrolls the page *vertically*, and
 * it escapes nothing horizontally — it is one pixel wide and clipped to none.
 *
 * The agent editor is the screen that exercises this: it is the tallest, and it
 * holds the `.sr-only` labels on the tool rows. The rest are here because the
 * property is the shell's rather than any screen's, and the next thing to break
 * it will break it somewhere nobody was looking.
 */
test.describe('the shell owns the only vertical scrollbar', () => {
  for (const screen of SCREENS) {
    test(`${screen.name} does not scroll the page itself`, async ({
      app,
      harness,
    }) => {
      await app.goto(`${harness.url}${screen.path}`);
      await expect(
        app.getByRole('complementary', { name: 'Sidebar' }),
      ).toBeVisible();

      expect(await scrollsVertically(app)).toBe(false);
    });
  }
});

/**
 * The same reflow claim, made against a phone instead of a font size.
 *
 * Doubling the root font size and halving the viewport are not the same test.
 * The first shrinks the room every column has *relative to its content*, which
 * is what catches a fixed-width control; the second takes the room away
 * outright, which is what catches a layout that only ever had one arrangement.
 * The four CRUD lists passed the first for a year while the providers list
 * pushed the page sideways at 375 — five columns, one of them a URL in a cell
 * that cannot be told to give way.
 *
 * Every screen, not just the ones that were broken. The point of a gate is to
 * cover the screen nobody was thinking about.
 */
test.describe('on a phone', () => {
  for (const screen of SCREENS) {
    test(`${screen.name} fits a narrow viewport`, async ({ app, harness }) => {
      await app.goto(`${harness.url}${screen.path}`);
      // Resized here rather than through `test.use`, matching the composer's
      // narrow spec: the `app` fixture waits for the inline sidebar, which
      // below the shell's `md` breakpoint is a drawer and never appears. Boot
      // wide, then narrow.
      await expect(
        app.getByRole('complementary', { name: 'Sidebar' }),
      ).toBeVisible();
      await app.setViewportSize(NARROW_VIEWPORT);

      // The durable signal that the shell has actually taken the new width:
      // the sidebar has become a drawer and the button that opens it exists.
      // Waiting on that rather than on a timeout is what keeps this from
      // measuring a layout mid-reflow.
      await expect(
        app.getByRole('button', { name: 'Open menu' }),
      ).toBeVisible();

      // Both sensors, because each misses what the other catches: the page can
      // scroll sideways, or a column can clip the overflow and hide it instead.
      expect(await scrollsHorizontally(app)).toBe(false);
      expect(
        await escapees(app),
        'nothing should extend past the column that clips it',
      ).toEqual([]);
    });
  }

  /**
   * The sweep above walks every screen holding the fixtures' own data, and the
   * fixtures are polite: the seeded endpoint is `http://127.0.0.1:11434`, which
   * fits a phone whatever the layout does. A gate that only ever sees content
   * that fits is not measuring anything — removing the guard that makes this
   * work leaves it green.
   *
   * So this one supplies the string the bug was actually about. An endpoint is
   * an identifier with no spaces in it, and the host is deliberately one
   * unbroken run: a browser will happily break a URL after a hyphen, a dot or a
   * slash, so `a-self-hosted-inference-box.internal.example.com` wraps on its
   * own and proves nothing. What has no break opportunity at all is a long
   * single-label host, and its min-content width is the whole line — which in a
   * table cell is what pushed the page sideways, and is exactly what a card has
   * to be able to break.
   *
   * Measured on the element rather than on the page, and that is the point.
   * Neither sensor above can see this one: the span is a flex item with
   * `min-width: 0`, so it *shrinks* and the text spills out of a box that is
   * itself well inside the column — no element escapes, nothing scrolls, and
   * the reader sees a URL cut off mid-host. Comparing the span's scroll width
   * to its client width asks the only question that distinguishes the two:
   * whether the text fits the box it was given.
   */
  test('a long endpoint wraps instead of widening the page', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        providers: {
          ollama: {
            type: 'ollama',
            apiBase:
              'http://selfhostedinferenceboxonthelanwithalonghostname:11434/v1',
          },
        },
      },
    });

    await app.goto(`${harness.url}/settings?panel=providers`);
    await expect(
      app.getByRole('complementary', { name: 'Sidebar' }),
    ).toBeVisible();
    await app.setViewportSize(NARROW_VIEWPORT);
    await expect(app.getByRole('button', { name: 'Open menu' })).toBeVisible();

    // On screen at all, and the page still fits. The first half matters: a row
    // that hid the endpoint would pass the second half trivially, and hiding it
    // is what the column-shedding version of this list did.
    const endpoint = app.getByText('selfhostedinferencebox', { exact: false });
    await expect(endpoint).toBeVisible();

    // A pixel of slack, for the same reason the two sweeps above allow one.
    const spill = await endpoint.evaluate(
      (element) => element.scrollWidth - element.clientWidth,
    );
    expect(
      spill,
      'the endpoint should wrap inside its card rather than spill out of it',
    ).toBeLessThanOrEqual(1);
    expect(await scrollsHorizontally(app)).toBe(false);
  });
});

test.describe('at 1×', () => {
  test.use({
    harnessOptions: {
      // A session key is 36 characters and the sidebar column is 17rem, so the
      // list is the narrowest place in the app where text has to give way. Two
      // of them, one titled and one not, because a title and a bare key are
      // rendered by the same line.
      sessions: [
        { key: 'a-session-key-long-enough-to-overflow', turns: ['hello'] },
        {
          key: 'b-session-key-long-enough-to-overflow',
          title: 'A title that is also far too long to fit in the column',
          turns: ['hello'],
        },
      ],
    },
  });

  for (const screen of SCREENS) {
    test(`${screen.name} keeps its content inside the columns`, async ({
      app,
      harness,
    }) => {
      await app.goto(`${harness.url}${screen.path}`);
      await expect(
        app.getByRole('complementary', { name: 'Sidebar' }),
      ).toBeVisible();

      // The bug this exists for: Radix's `ScrollArea` renders the viewport's
      // content inside a `display: table` wrapper so that content wider than
      // the viewport can define a scroll width. A table shrink-wraps to its
      // widest cell, so everything under it is sized by the longest line
      // instead of by the column — and `truncate` has no width to truncate
      // against. What the user sees is a session key running out of the
      // sidebar and being cut off mid-character by the aside's own `overflow`,
      // with no ellipsis and nothing in the console. `escapees` is where the
      // measurement lives, and the phone block above uses the same one.
      expect(
        await escapees(app),
        'nothing should extend past the column that clips it',
      ).toEqual([]);
    });
  }
});

test.describe('keyboard', () => {
  test('every stop on the style guide takes a visible ring', async ({
    app,
    harness,
  }) => {
    await app.goto(`${harness.url}/tokens`);
    await expect(app.getByRole('heading', { level: 1 })).toBeVisible();

    // Enough presses to walk out of the shell and well into the page. The
    // assertion is on every stop, so the count only decides how much is
    // covered, never whether a failure is real.
    const unringed: string[] = [];
    for (let i = 0; i < 60; i += 1) {
      await app.keyboard.press('Tab');
      const stop = await app.evaluate(() => {
        const element = document.activeElement;
        if (element === null || element === document.body) return null;
        const style = getComputedStyle(element);
        const described = `${element.tagName.toLowerCase()}${
          element.getAttribute('aria-label') === null
            ? ''
            : `[${element.getAttribute('aria-label') ?? ''}]`
        }`;
        // The base layer draws an `outline`. A component that manages its own
        // ring would draw a `box-shadow` instead, and both count — what does
        // not count is neither.
        const ringed =
          (style.outlineStyle !== 'none' &&
            parseFloat(style.outlineWidth) > 0) ||
          style.boxShadow !== 'none';
        return { described, ringed };
      });

      if (stop === null) continue;
      if (!stop.ringed) unringed.push(stop.described);
    }

    expect(unringed, 'every focus stop should carry a visible ring').toEqual(
      [],
    );
  });

  test('a dialog traps focus, closes on Escape and gives focus back', async ({
    app,
    harness,
  }) => {
    await app.goto(`${harness.url}/tokens`);

    const trigger = app.getByRole('button', { name: 'Open dialog' });
    await trigger.click();

    const dialog = app.getByRole('dialog');
    await expect(dialog).toBeVisible();

    // Tab all the way round the dialog's own controls. If focus escaped, one of
    // these stops would land outside it.
    for (let i = 0; i < 8; i += 1) {
      await app.keyboard.press('Tab');
      const inside = await dialog.evaluate(
        (element) =>
          document.activeElement !== null &&
          element.contains(document.activeElement),
      );
      expect(inside, `focus left the dialog after ${String(i + 1)} tabs`).toBe(
        true,
      );
    }

    await app.keyboard.press('Escape');
    await expect(dialog).toBeHidden();
    // Back to the button that opened it, rather than to the top of the page.
    await expect(trigger).toBeFocused();
  });

  test('the notification bell states its count and returns focus', async ({
    app,
  }) => {
    // The count is drawn as a dot, so the button's name is the only place a
    // screen reader can learn there is anything to look at.
    const bell = app.getByRole('button', { name: /^Notifications/u });
    await bell.click();

    await expect(app.getByRole('link', { name: 'See all' })).toBeVisible();

    await app.keyboard.press('Escape');
    await expect(app.getByRole('link', { name: 'See all' })).toBeHidden();
    await expect(bell).toBeFocused();
  });

  test('every message action is reachable and named', async ({ app }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('button', { name: 'Send' }).click();
    await expect(
      app.getByTestId('transcript').getByText('Here is what I found.'),
    ).toBeVisible({
      timeout: 15_000,
    });

    // Revealed with `opacity` and `:focus-within` rather than `display: none`,
    // so they keep their place in the tab order — which is the whole reason a
    // keyboard user can reach them at all.
    for (const name of [
      'Copy message',
      'Regenerate the answer',
      'Turn details',
    ]) {
      const action = app.getByRole('button', { name }).first();
      await expect(action).toBeVisible();
      await action.focus();
      await expect(action).toBeFocused();
    }
  });
});

test.describe('no theme is second-class', () => {
  for (const screen of SCREENS) {
    test(`${screen.name} renders no invisible text`, async ({
      app,
      harness,
    }) => {
      await app.goto(`${harness.url}${screen.path}`);
      await expect(
        app.getByRole('complementary', { name: 'Sidebar' }),
      ).toBeVisible();

      // The unit suite proves every *token pairing* meets AA in both themes,
      // by reading `tokens.css`. What it cannot see is a component that paired
      // two tokens the palette never intended to meet, or a surface that ended
      // up nested inside itself. This is the browser half of that, and it
      // measures what was painted rather than what was declared.
      //
      // The bar is 3:1 — the AA floor for large text, and here a floor for
      // "legible at all" rather than a second copy of the AA suite. Anything
      // below it in a UI whose every colour comes from a checked palette is a
      // pairing nobody chose.
      const collisions = await app.evaluate(() => {
        // Chrome's computed `color` is `oklch(…)`, because that is what the
        // stylesheet says and computed values preserve the notation. Parsing
        // the numbers out of it and treating them as RGB is the trap: it
        // reports lightness against a hue angle. A 1×1 canvas resolves any CSS
        // colour the browser accepts to the sRGB bytes it would actually paint.
        const probe = document.createElement('canvas');
        probe.width = 1;
        probe.height = 1;
        const ctx = probe.getContext('2d', { willReadFrequently: true });

        const toRgb = (color: string): [number, number, number, number] => {
          if (ctx === null) return [0, 0, 0, 0];
          ctx.clearRect(0, 0, 1, 1);
          ctx.fillStyle = color;
          ctx.fillRect(0, 0, 1, 1);
          const [r = 0, g = 0, b = 0, a = 0] = ctx.getImageData(
            0,
            0,
            1,
            1,
          ).data;
          return [r, g, b, a / 255];
        };

        const channel = (value: number): number => {
          const srgb = value / 255;
          return srgb <= 0.03928
            ? srgb / 12.92
            : Math.pow((srgb + 0.055) / 1.055, 2.4);
        };
        const luminance = ([r, g, b]: [number, number, number]): number =>
          0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
        const ratio = (
          a: [number, number, number],
          b: [number, number, number],
        ): number => {
          const [light, dark] = [luminance(a), luminance(b)].sort(
            (x, y) => y - x,
          ) as [number, number];
          return (light + 0.05) / (dark + 0.05);
        };
        /** Composites a partly transparent colour over what is behind it. */
        const over = (
          [r, g, b, a]: [number, number, number, number],
          base: [number, number, number],
        ): [number, number, number] => [
          r * a + base[0] * (1 - a),
          g * a + base[1] * (1 - a),
          b * a + base[2] * (1 - a),
        ];

        const found: string[] = [];
        const page = toRgb(
          getComputedStyle(document.documentElement).backgroundColor,
        );
        const pageRgb: [number, number, number] = [page[0], page[1], page[2]];

        for (const element of document.querySelectorAll<HTMLElement>(
          'body *',
        )) {
          // Only elements holding text of their own, or every ancestor reports
          // its descendants' words as its own.
          const own = [...element.childNodes]
            .filter((node) => node.nodeType === Node.TEXT_NODE)
            .map((node) => node.textContent ?? '')
            .join('')
            .trim();
          if (own === '') continue;
          if (element.closest('[hidden], [aria-hidden="true"]') !== null) {
            continue;
          }

          const style = getComputedStyle(element);
          if (style.visibility === 'hidden' || style.display === 'none') {
            continue;
          }
          if (element.getClientRects().length === 0) continue;

          // Every ancestor's opacity multiplies, and a faded-out overlay is not
          // a contrast bug.
          let opacity = 1;
          for (
            let node: HTMLElement | null = element;
            node !== null;
            node = node.parentElement
          ) {
            opacity *= parseFloat(getComputedStyle(node).opacity);
          }
          if (opacity < 0.95) continue;

          // Composite every backdrop from the page up, so a translucent hover
          // overlay counts as part of what is behind the text.
          const layers: Array<[number, number, number, number]> = [];
          for (
            let node: HTMLElement | null = element;
            node !== null;
            node = node.parentElement
          ) {
            const fill = toRgb(getComputedStyle(node).backgroundColor);
            if (fill[3] > 0) layers.push(fill);
          }
          let background = pageRgb;
          for (const layer of layers.reverse()) {
            background = over(layer, background);
          }

          // Text this element does not paint. A syntax-highlighted box sets its
          // textarea's colour to `transparent` once the coloured layer behind
          // it is current: the caret stays, the glyphs come from the layer, and
          // that layer's own spans are measured here in their own right.
          // Compositing a zero-alpha colour over its backdrop gives 1:1, which
          // reported every prompt box on the agent editor as invisible text —
          // but only when the colouring had landed before the sweep looked,
          // which is what made it a flake rather than a failure.
          const ownColour = toRgb(style.color);
          if (ownColour[3] === 0) continue;

          const text = over(ownColour, background);
          if (ratio(text, background) < 3) {
            found.push(
              `${element.tagName.toLowerCase()}: "${own.slice(0, 40)}"`,
            );
          }
        }
        return found;
      });

      expect(
        collisions,
        'every painted string should clear 3:1 against what is behind it',
      ).toEqual([]);
    });
  }
});
