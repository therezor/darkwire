/**
 * The design-fidelity gate.
 *
 * The brief was a screenshot diff against the product being replaced, in dark,
 * with sub-pixel deviations everywhere except four known ones. Building it
 * turned up a fifth deviation that no tolerance can absorb: the two products
 * draw their icons from different places. The reference uses a webfont's glyph
 * set; the replacement uses an inline SVG set, because a font CDN in a
 * self-hosted product leaks every user's IP and breaks an air-gapped install —
 * which is the same decision that made the offline spec possible. Every button,
 * every sidebar row and every header therefore differs at the pixel level on a
 * screen that is otherwise exactly right, and a threshold over that number
 * reports the same large figure for a correct screen and a broken one.
 *
 * So the gate is split, and each half does the job it can actually do.
 *
 *  - **This file asserts the measurements.** The shell's three dimensions, the
 *    base of the type scale, and the surface and text ramps — read off both
 *    rendered documents the same way, so neither side is being taken from a
 *    stylesheet on trust. These are what "indistinguishable at 1×" means once
 *    you stop being able to subtract two bitmaps, and they are what had
 *    actually drifted.
 *  - **`fidelity/capture.ts` writes the pictures.** Both screens and their
 *    pixel diff, into `artifacts/fidelity/`, for the review the plan also asks
 *    for. `pnpm --filter @ghostwire/e2e baseline` is the command. It is not a
 *    threshold, and it does not fail a build.
 *
 * Dark only, because there is no light-mode reference: the product being
 * replaced has one theme. Light is held to the contrast assertion in
 * `@ghostwire/web` and the sweep in `a11y.spec.ts`, which is the whole of what
 * can be said about a theme with nothing to compare it to.
 */

import { chromium, type Page } from '@playwright/test';

import { VIEWPORT } from '../src/viewport.js';
import {
  REFERENCE_LAYOUT,
  REPLACEMENT_LAYOUT,
  readMetrics,
  type Layout,
  type Metrics,
} from '../src/fidelity/metrics.js';
import {
  referenceAvailable,
  serveReference,
  DEFAULT_ORIGINAL_ROOT,
} from '../src/fidelity/original.js';
import { expect, test } from '../src/fixtures.js';

/**
 * The shell is deliberately no longer the reference's size.
 *
 * It used to be, exactly: 272, 56, 860 and a 14px base, asserted with a zero
 * tolerance because a column sixteen pixels narrower is not a rounding
 * difference, it is a different layout. That gate did its job — it is what
 * stopped those numbers drifting silently — and then the product asked for a
 * larger UI, which is a decision the gate has no standing to veto. The tokens
 * now read 18rem, 4rem, 57.5rem and 0.9375rem: bigger on every axis, by
 * between six and fifteen percent.
 *
 * So the four dimension checks became a bracket rather than an equality. This
 * is the ceiling on that bracket, and the floor is "larger than the reference"
 * — together they say the thing actually worth asserting, which is that the
 * divergence is the one somebody chose rather than one that crept in.
 */
const SCALE_CEILING = 1.2;

/**
 * How far each *colour* measurement may be from the reference, and why.
 *
 * The channel tolerance is the one documented deviation, stated as a number.
 * The replacement's neutral axis carries a small chroma — surfaces and text
 * hold a trace of the accent hue, which is what keeps a green accent from
 * looking pasted onto a slate UI — so a grey the reference paints as (13,13,13)
 * is painted here as (9,11,10). Four of 255 on the red channel, and nothing on
 * the perceived lightness: the muted-text luminance below lands within one.
 *
 * The accent itself is not compared, and cannot be: it is a green where the
 * reference's is a gold, which is the one place this UI deliberately stops
 * matching the product it replaces. The neutral axis follows the accent's hue,
 * which is why a *grey* shows up in a tolerance at all.
 *
 * Everything else the ramp used to differ on is gone. The surface stops and the
 * text tiers are now the reference's own values converted to OKLCH, with one
 * exception recorded in `tokens.css`: the two lower text stops are lighter,
 * because the originals sit at 4.0:1 and 2.9:1 against the page and this sheet
 * holds every text token to AA.
 */
const TOLERANCE = {
  /** sRGB, 0–255. The neutral axis's trace of the accent hue, and nothing else. */
  channel: 6,
  /** Relative luminance, 0–255. Currently within one. */
  luminance: 3,
} as const;

async function measure(page: Page, layout: Layout): Promise<Metrics> {
  return await page.evaluate(readMetrics, layout);
}

test.describe('fidelity', () => {
  // Not `test.skip(condition)` inside the body: the reference is another
  // checkout beside this one, so its absence is the normal state of a CI
  // machine and of anyone who cloned only this repo. A suite that failed there
  // would be a suite people learn to ignore.
  test.skip(
    !referenceAvailable(),
    `no reference build at ${DEFAULT_ORIGINAL_ROOT}`,
  );

  test('matches the product being replaced on every measurement that can be compared', async ({
    app,
  }, testInfo) => {
    // In the body rather than beside the one above: a project name is a
    // per-test fact, and the file-level form has no test to read it from.
    test.skip(testInfo.project.name !== 'dark', 'the reference has one theme');

    const reference = await serveReference();
    const browser = await chromium.launch();
    let expected: Metrics;
    try {
      const page = await browser.newPage({
        colorScheme: 'dark',
        viewport: VIEWPORT,
      });
      await page.goto(reference.url);
      expected = await measure(page, REFERENCE_LAYOUT);
    } finally {
      await browser.close();
      await reference.close();
    }

    // The chat measure is on the composer, which is the one element both
    // products size to the same rule — the transcript's column follows it.
    const actual = await measure(app, REPLACEMENT_LAYOUT);

    // The four measurements that deliberately no longer match. Each is asserted
    // as a *direction and a bound* rather than as a target in pixels: the
    // targets live in `tokens.css`, and a test that restated them here would
    // pass by agreeing with a number it copied rather than by measuring
    // anything. Bracketed, it still catches both failures worth catching — a
    // token quietly reverted to the reference's density, and a scale that ran
    // away — without pretending to check arithmetic it cannot see.
    for (const [name, mine, theirs] of [
      ['sidebar width', actual.sidebarWidth, expected.sidebarWidth],
      ['header height', actual.headerHeight, expected.headerHeight],
      ['chat measure', actual.chatWidth, expected.chatWidth],
      ['base font size', actual.baseFontSize, expected.baseFontSize],
    ] as const) {
      const ratio = mine / theirs;
      expect
        .soft(ratio, `${name} is larger than the reference's`)
        .toBeGreaterThan(1);
      expect
        .soft(ratio, `${name} is within the intended scale`)
        .toBeLessThanOrEqual(SCALE_CEILING);
    }

    for (const [name, mine, theirs] of [
      ['the page surface', actual.surface0, expected.surface0],
      ['the raised surface', actual.surface1, expected.surface1],
      ['body text', actual.foreground, expected.foreground],
    ] as const) {
      const worst = Math.max(
        ...mine.map((value, index) => Math.abs(value - (theirs[index] ?? 0))),
      );
      expect
        .soft(worst, `${name}: worst channel`)
        .toBeLessThanOrEqual(TOLERANCE.channel);
    }

    expect
      .soft(
        Math.abs(actual.foregroundMuted - expected.foregroundMuted),
        'muted text',
      )
      .toBeLessThanOrEqual(TOLERANCE.luminance);
  });
});
