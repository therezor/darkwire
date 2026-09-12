/**
 * Both screens, side by side, and the difference between them.
 *
 * `pnpm --filter @ghostwire/e2e baseline` writes three files per screen into
 * `artifacts/fidelity/`: the reference, the replacement, and a diff map with
 * every changed pixel marked. It prints the percentage and exits zero.
 *
 * It exits zero deliberately, and the percentage is not a score. Two things
 * make it uninterpretable as one. The first is that the products draw their
 * icons from different sources — a webfont's glyph set on one side, an inline
 * SVG set on the other, because a font CDN in a self-hosted product leaks every
 * user's IP and breaks an air-gapped install — so every icon in the chrome
 * differs on a screen that is otherwise exactly right. The second is that the
 * figure is dominated by how much of the screen is empty: an idle chat view is
 * mostly background, and reads as a low single-digit percentage whether the
 * content on it matches or not. A threshold over that would pass a broken
 * screen and fail a correct one with equal confidence.
 *
 * `tests/fidelity.spec.ts` is the gate, and it measures lengths and colours.
 * This is the picture a person looks at.
 *
 * That is not a smaller job than the diff was. "Are these the same screen?" is
 * a question a person answers in a second and a subtraction cannot answer at
 * all — what the subtraction is good for is telling that person where to look.
 */

import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { chromium, type Browser, type Page } from '@playwright/test';
import pixelmatch from 'pixelmatch';
import { PNG } from 'pngjs';

import {
  startHarness,
  PASSWORD,
  USERNAME,
  type Harness,
} from '../harness/server.js';
import { VIEWPORT } from '../viewport.js';
import {
  DEFAULT_ORIGINAL_ROOT,
  referenceAvailable,
  serveReference,
} from './original.js';

const OUT = join(
  dirname(fileURLToPath(import.meta.url)),
  '..',
  '..',
  'artifacts',
  'fidelity',
);

interface Screen {
  readonly name: string;
  /** The path to open on the replacement. */
  readonly route: string;
  /**
   * Run against the reference to bring this screen to the front.
   *
   * The reference is one document with every panel already in it, shown and
   * hidden by class. Nothing is *added* here — the classes are the ones its own
   * scripts would have set.
   */
  readonly reveal?: (page: Page) => Promise<void>;
}

const SCREENS: readonly Screen[] = [
  { name: 'chat', route: '/' },
  {
    name: 'files',
    route: '/files',
    reveal: async (page) => {
      await page.evaluate(() => {
        document.querySelector('#fs-modal')?.classList.add('active');
      });
    },
  },
  {
    name: 'context',
    route: '/settings',
    reveal: async (page) => {
      await page.evaluate(() => {
        document.querySelector('#context-modal')?.classList.add('active');
      });
    },
  },
];

async function shoot(page: Page): Promise<PNG> {
  // Animations paused, or two runs a second apart disagree about where a
  // fade-in had got to.
  await page.emulateMedia({ reducedMotion: 'reduce' });
  return PNG.sync.read(await page.screenshot());
}

async function captureReference(
  browser: Browser,
  url: string,
  screen: Screen,
): Promise<PNG> {
  const page = await browser.newPage({
    colorScheme: 'dark',
    viewport: VIEWPORT,
  });
  try {
    await page.goto(url, { waitUntil: 'load' });
    await screen.reveal?.(page);
    return await shoot(page);
  } finally {
    await page.close();
  }
}

async function captureReplacement(
  browser: Browser,
  harness: Harness,
  screen: Screen,
): Promise<PNG> {
  const context = await browser.newContext({
    colorScheme: 'dark',
    viewport: VIEWPORT,
  });
  try {
    const page = await context.newPage();
    await page.request.post(`${harness.url}/api/auth/login`, {
      data: { username: USERNAME, password: PASSWORD },
    });
    await page.goto(`${harness.url}${screen.route}`);
    await page.getByRole('complementary', { name: 'Sidebar' }).waitFor();
    // The fonts, or the first screen is measured mid-swap against a fallback.
    await page.evaluate(async () => {
      await document.fonts.ready;
    });
    return await shoot(page);
  } finally {
    await context.close();
  }
}

async function main(): Promise<void> {
  if (!referenceAvailable()) {
    process.stderr.write(
      `No reference build at ${DEFAULT_ORIGINAL_ROOT}.\n` +
        'Set GHOSTAI_FIDELITY_ORIGINAL to the product being replaced.\n',
    );
    process.exitCode = 1;
    return;
  }

  mkdirSync(OUT, { recursive: true });
  // Fonts on, unlike the gate: this is the picture, and the reference rendered
  // in a fallback font with its icons spelled out as words is a worse likeness
  // of it than one with the fonts.
  const reference = await serveReference({ webfonts: true });
  const harness = await startHarness();
  const browser = await chromium.launch();

  try {
    for (const screen of SCREENS) {
      const theirs = await captureReference(browser, reference.url, screen);
      const mine = await captureReplacement(browser, harness, screen);

      writeFileSync(
        join(OUT, `${screen.name}.reference.png`),
        PNG.sync.write(theirs),
      );
      writeFileSync(
        join(OUT, `${screen.name}.replacement.png`),
        PNG.sync.write(mine),
      );

      if (theirs.width !== mine.width || theirs.height !== mine.height) {
        process.stdout.write(
          `${screen.name}: sizes differ (${String(theirs.width)}×${String(theirs.height)} vs ` +
            `${String(mine.width)}×${String(mine.height)}); no diff written\n`,
        );
        continue;
      }

      const diff = new PNG({ width: theirs.width, height: theirs.height });
      const changed = pixelmatch(
        theirs.data,
        mine.data,
        diff.data,
        theirs.width,
        theirs.height,
        {
          // Loose, because the subject is layout rather than antialiasing: a
          // threshold tight enough to catch a shifted border would light up every
          // glyph edge on the page.
          threshold: 0.2,
          includeAA: false,
        },
      );
      writeFileSync(join(OUT, `${screen.name}.diff.png`), PNG.sync.write(diff));

      const share = ((changed / (theirs.width * theirs.height)) * 100).toFixed(
        1,
      );
      process.stdout.write(`${screen.name}: ${share}% of pixels differ\n`);
    }
  } finally {
    await browser.close();
    await harness.close();
    await reference.close();
  }

  process.stdout.write(
    `\nWritten to ${OUT}\nReview the pairs; the gate is fidelity.spec.ts.\n`,
  );
}

await main();
