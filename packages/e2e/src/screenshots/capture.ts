/**
 * The pictures in the documentation, taken from the real thing.
 *
 * `pnpm screenshots` boots the same harness the browser suite runs against —
 * the real server, the real bundle, the real turn — drives each screen to the
 * state worth showing, and writes a PNG per screen per colour scheme into
 * `docs/screenshots/`. Those files are committed, because a README on GitHub
 * cannot run a build step.
 *
 * It is a sibling of `../fidelity/capture.ts` rather than a mode of it. That
 * one exists to diff this product against the one it replaces and refuses to
 * run without `DARKWIRE_FIDELITY_ORIGINAL`; this one must work on any clone. The
 * two share the harness, which is the part worth sharing, and nothing else.
 *
 * ## Why so much of this file is about holding the screen still
 *
 * A screenshot committed to git is a diff every time it changes, so anything
 * that varies between two runs of the same code is noise a reviewer has to read
 * past. Two consecutive runs on one machine produce byte-identical files, and
 * five separate things had to be pinned down to get there:
 *
 *  - **The clock.** `relativeSpan` reports `now` only for the first minute and
 *    `1m ago` after it, and four screens read it — so whether two runs agree
 *    depended on how long the run took. Frozen page-side, which is where the
 *    components read it. See `frozenNow`.
 *  - **Row ordering under a millisecond tie.** The one that actually caused the
 *    churn, and the one nothing about a screenshot suggests you would hit. Seed
 *    rows written in a loop share a timestamp often but not always, so a list
 *    ordered `time DESC, id ASC` reshuffles between runs whenever the two rules
 *    disagree. See `SEED` and `seedNotifications`; the two are solved
 *    differently because only one of them has ids anybody chose.
 *  - **The highlighter.** Shiki is a separate chunk and the code block renders
 *    as plain monospace until it lands. The text is on screen before the colour
 *    is, so waiting for the text catches an unstyled block about half the time;
 *    this waits for `.code-block__line`, which exists only once the tokens do.
 *  - **Motion.** Fades, the caret, progress ticks. `animations: 'disabled'` and
 *    `caret: 'hide'` cover the first two; the third is why every capture is of a
 *    *settled* turn — except the approval, which is settled on the gate.
 *  - **Fonts.** `document.fonts.ready`, or the first screen is measured mid-swap
 *    against a fallback.
 *
 * What is deliberately not chased: two different machines will not agree,
 * because font rasterisation is the platform's. These are regenerated when the
 * UI changes, not diffed in CI.
 */

import { mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  chromium,
  type Browser,
  type BrowserContext,
  type Page,
} from '@playwright/test';

import {
  startHarness,
  PASSWORD,
  USERNAME,
  type Harness,
  type HarnessOptions,
  type SeedNotification,
} from '../harness/server.js';
import { VIEWPORT } from '../viewport.js';

/**
 * `docs/screenshots/`, five levels up from here.
 *
 * Under `docs/` rather than beside the e2e artifacts because
 * `packages/e2e/.gitignore` ignores `artifacts/` wholesale — and a git negation
 * inside an ignored *directory* does not re-include anything, so a file written
 * there could not be committed without restructuring an ignore that is right as
 * it stands. `docs/` is also one relative segment from the pages that embed
 * them.
 */
const OUT = join(
  dirname(fileURLToPath(import.meta.url)),
  '..',
  '..',
  '..',
  '..',
  'docs',
  'screenshots',
);

/**
 * Every page believes it is the instant it opened, for as long as it is open.
 *
 * Frozen, because `relativeSpan` flips from `now` to `1m ago` at sixty seconds
 * and four screens read it — so without this, whether two runs agree depends on
 * how long the run took.
 *
 * Frozen to the *real* clock rather than to a chosen epoch, which is where this
 * went wrong first. A page that believes it is some fixed date does not merely
 * disagree with reality, it disagrees with the *server*: the approval gate's
 * deadline is a Node-side `Date.now() + approvalTimeoutMs`, and a browser seven
 * months behind renders the countdown as `4899h 22m` rather than `5m`. Anything
 * the server timestamped has to be read against the clock the server used.
 *
 * Per page rather than per run for the same reason at a smaller scale: with one
 * instant for the whole script, the approval countdown would show how long the
 * run had been going by the time it reached that screen.
 *
 * The absolute value differs between runs. What is rendered from it does not,
 * beyond a second on the one screen showing a live countdown.
 */
const frozenNow = (): number => Date.now();

type Theme = 'dark' | 'light';

/** Dark first: the UI is dark-first, and so is the README's lead image. */
const THEMES: readonly Theme[] = ['dark', 'light'];

interface Screen {
  /** The file's basename. `chat` becomes `chat.dark.png` and `chat.light.png`. */
  readonly name: string;
  /** Opened after sign-in, relative to the harness origin. */
  readonly route: string;
  /**
   * Drives the screen to the state worth photographing, and returns once it has
   * settled. A screen with nothing to do beyond loading omits it.
   */
  readonly settle?: (page: Page) => Promise<void>;
}

/**
 * Sessions in the sidebar, so the shell is not photographed empty.
 *
 * **Seeded in reverse key order, and that is load-bearing.** The sidebar is
 * `updated DESC, key ASC`, and three sessions written in a loop sometimes share
 * a millisecond and sometimes do not — so on a tie the tie-break decides, and
 * with keys ascending in seed order the two rules disagree and the list
 * reshuffles between runs. Seeding last-first makes `key ASC` agree with
 * `updated DESC`, so both orderings give the same list and the tie stops
 * mattering. Rename one of these and the order becomes a coin flip again.
 */
const SEED: HarnessOptions = {
  sessions: [
    { key: 'web:research', title: 'Comparing local models' },
    { key: 'web:parser', title: 'Refactoring the parser' },
    {
      key: 'web:default',
      title: 'Reading the workspace',
      turns: [
        'What is in the workspace?',
        'One note file and a TypeScript source.',
      ],
    },
  ],
};

/**
 * The notification archive, written one at a time with a gap.
 *
 * Not through `HarnessOptions`, and not in a loop, for the reason above with
 * one difference that makes it worse: notifications order by
 * `created_at_ms DESC, id ASC` and the id is a **random UUID**, so two rows
 * sharing a millisecond come back in a different order on *every* run rather
 * than occasionally. There is no key to name my way out of, so the timestamps
 * are separated instead. Ten milliseconds on a run that takes minutes.
 */
async function seedNotifications(harness: Harness): Promise<void> {
  const archive: readonly SeedNotification[] = [
    { title: 'Scheduled job finished', body: 'The nightly digest ran.' },
    {
      title: 'A tool call needs approval',
      body: 'exec wants to run node --version.',
      level: 'warning',
    },
  ];

  for (const notification of archive) {
    const response = await fetch(`${harness.url}/api/_test/notifications`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        authorization: `Bearer ${harness.token}`,
      },
      body: JSON.stringify(notification),
    });
    if (!response.ok) {
      throw new Error(
        `Seeding a notification answered ${String(response.status)}.`,
      );
    }
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

/**
 * What gets typed into the composer, and why these sentences.
 *
 * `harness/script.ts` routes on a keyword — `stream`, `list`, `run` — and the
 * specs pass it bare imperatives, which is right for a test and wrong for a
 * picture: the first message becomes the session title, so `stream a long
 * answer` ends up in the sidebar of the image on the front page. These carry
 * the same keyword inside a sentence somebody might actually send. First match
 * wins, so none of them may contain a second route's word.
 */
const PROMPTS = {
  answer: 'Read notes.md and stream me a summary',
  tool: 'List what is in the workspace',
  approval: 'Run node --version and tell me the runtime',
} as const;

/** Sends a message and returns when the turn has settled back to Send. */
async function ask(page: Page, message: string): Promise<void> {
  await page.getByRole('textbox', { name: 'Message' }).fill(message);
  await page.getByRole('button', { name: 'Send' }).click();
  await page.getByRole('button', { name: 'Send' }).waitFor();
}

const SCREENS: readonly Screen[] = [
  {
    name: 'chat',
    route: '/',
    settle: async (page) => {
      await ask(page, PROMPTS.answer);
      // The highlighter's chunk has landed. The text arrives before the colour
      // does, so waiting on the text photographs plain monospace half the time.
      await page.locator('pre code .code-block__line').first().waitFor();
    },
  },
  {
    name: 'chat-tool-call',
    route: '/',
    settle: async (page) => {
      await ask(page, PROMPTS.tool);
      const card = page.getByRole('region', { name: 'Tool call: ls' });
      await card.waitFor();
      // Expanded, because a collapsed card shows that a tool ran and not what
      // it returned — and the second is the thing worth a picture.
      await card.getByRole('button', { expanded: false }).click();
      await card.getByText('notes.md').waitFor();
    },
  },
  {
    name: 'chat-approval',
    route: '/',
    settle: async (page) => {
      // Not `ask`: this turn deliberately does *not* settle. `exec` is `ask`,
      // so the loop is parked on the approval gate, and that is the state being
      // photographed — the arguments on screen before anything ran.
      await page
        .getByRole('textbox', { name: 'Message' })
        .fill(PROMPTS.approval);
      await page.getByRole('button', { name: 'Send' }).click();
      await page.getByRole('region', { name: 'Tool call: exec' }).waitFor();
      await page.getByText('node').first().waitFor();
    },
  },
  {
    name: 'context',
    // Pinned to a seeded session rather than the one a fresh tab mints, because
    // this is the only screen that prints the session key — and a minted key is
    // a UUID, which would make this file differ from itself on every run.
    route: '/sessions/web:default',
    settle: async (page) => {
      await ask(page, PROMPTS.answer);
      // The strip's accessible name is the figure it shows — "1,024 of 65,536 ·
      // 2%" — which is also the only stable thing about it.
      await page.getByRole('button', { name: /\bof\b.*·/u }).click();
      await page.getByRole('dialog').waitFor();
      await page.getByText('Tool definitions').first().waitFor();
    },
  },
  {
    name: 'files',
    // Inside a workspace, not at the list of them. `/files` on its own is every
    // workspace as a row, and a picture of two folder names says less about the
    // file browser than the browser itself does.
    route: '/files?workspace=default',
    settle: async (page) => {
      await page.getByText('notes.md').first().waitFor();
    },
  },
  {
    name: 'settings-providers',
    route: '/settings?panel=providers',
  },
  {
    name: 'agent-editor',
    route: '/agents/default',
  },
  {
    name: 'workspaces',
    route: '/workspaces',
  },
  {
    name: 'notifications',
    route: '/notifications',
    settle: async (page) => {
      await page.getByText('Scheduled job finished').first().waitFor();
    },
  },
];

/** A `darkwire_session` cookie, and nothing else. */
type Cookies = Awaited<ReturnType<BrowserContext['cookies']>>;

/**
 * Signs in once, and hands back the cookie every screen will reuse.
 *
 * One login for the whole run, not one per screen, because the login route is
 * rate-limited at **ten attempts a minute per address** — and that bucket
 * counts successes. Twenty screens logging in individually would get ten
 * captures and ten pictures of the sign-in overlay. `fixtures.ts` can log in
 * per test because Playwright gives each test its own worker and its own
 * harness; this script has one of each.
 */
async function signIn(browser: Browser, harness: Harness): Promise<Cookies> {
  const context = await browser.newContext();
  try {
    const response = await context.request.post(
      `${harness.url}/api/auth/login`,
      { data: { username: USERNAME, password: PASSWORD } },
    );
    if (!response.ok()) {
      throw new Error(
        `The harness login failed with ${String(response.status())}: ` +
          (await response.text()),
      );
    }
    return await context.cookies();
  } finally {
    await context.close();
  }
}

/**
 * A page on the harness, signed in, with the clock frozen and motion off.
 *
 * The cookie is injected rather than earned, per `signIn` above. Only cookies
 * cross between screens — never `localStorage` — so each screen still starts
 * with its own chat session rather than appending to the last one's transcript.
 */
async function open(
  browser: Browser,
  harness: Harness,
  theme: Theme,
  route: string,
  cookies: Cookies,
): Promise<{ page: Page; close: () => Promise<void> }> {
  const context = await browser.newContext({
    colorScheme: theme,
    viewport: VIEWPORT,
    // Pinned for the same reason `playwright.config.ts` pins it: the pre-paint
    // script resolves a locale from `Accept-Language`, and the config's `en`
    // only outranks it after sign-in.
    locale: 'en-US',
    reducedMotion: 'reduce',
  });
  await context.addCookies(cookies);

  const page = await context.newPage();

  // Before any script on the page runs. The components take `now` as an
  // argument and read it from the page's clock, so freezing it here is what
  // makes "3 minutes ago" say the same thing on every run.
  await page.addInitScript(`{
    const frozen = ${String(frozenNow())};
    const RealDate = Date;
    const FrozenDate = class extends RealDate {
      constructor(...args) {
        super(...(args.length === 0 ? [frozen] : args));
      }
      static now() { return frozen; }
    };
    globalThis.Date = FrozenDate;
  }`);

  await page.goto(`${harness.url}${route}`);
  // The shell rather than the route: waiting for it separates "the bundle
  // loaded" from "React mounted".
  await page.getByRole('complementary', { name: 'Sidebar' }).waitFor();
  await page.evaluate(async () => {
    await document.fonts.ready;
  });

  return {
    page,
    close: async () => {
      await context.close();
    },
  };
}

async function shoot(page: Page, name: string, theme: Theme): Promise<void> {
  await page.screenshot({
    path: join(OUT, `${name}.${theme}.png`),
    // `reducedMotion` stops animations that opted into respecting it; this
    // stops the rest, and `caret: 'hide'` stops the one thing neither covers.
    animations: 'disabled',
    caret: 'hide',
  });
}

/**
 * The first-run wizard, which needs a server nobody has claimed.
 *
 * Its own harness for that reason, and the language step is skipped because the
 * frame worth showing is the one asking for the code the terminal printed.
 */
async function captureSetup(browser: Browser): Promise<void> {
  const harness = await startHarness({ password: null });
  try {
    for (const theme of THEMES) {
      const context = await browser.newContext({
        colorScheme: theme,
        viewport: VIEWPORT,
        locale: 'en-US',
        reducedMotion: 'reduce',
      });
      try {
        const page = await context.newPage();
        await page.goto(harness.url);
        await page.getByText('Choose a language').waitFor();
        await page.evaluate(async () => {
          await document.fonts.ready;
        });
        await shoot(page, 'setup', theme);
      } finally {
        await context.close();
      }
    }
  } finally {
    await harness.close();
  }
}

async function main(): Promise<void> {
  mkdirSync(OUT, { recursive: true });

  const browser = await chromium.launch();

  try {
    for (const screen of SCREENS) {
      for (const theme of THEMES) {
        // A server per picture, not one for the run. `settle` sends messages,
        // and a message starts a session the sidebar then lists — so a shared
        // harness photographs every earlier screen's driving sentence stacked
        // up under "Latest sessions", including the other theme's. Boot is an
        // in-memory database and a port; it is cheaper than it looks, and it
        // also gives each picture its own login bucket.
        const harness = await startHarness(SEED);
        try {
          await seedNotifications(harness);
          const cookies = await signIn(browser, harness);
          const { page, close } = await open(
            browser,
            harness,
            theme,
            screen.route,
            cookies,
          );
          try {
            await screen.settle?.(page);
            await shoot(page, screen.name, theme);
            process.stdout.write(`${screen.name}.${theme}.png\n`);
          } finally {
            await close();
          }
        } finally {
          await harness.close();
        }
      }
    }

    await captureSetup(browser);
    process.stdout.write('setup.dark.png\nsetup.light.png\n');
  } finally {
    await browser.close();
  }

  process.stdout.write(`\nWrote ${OUT}\n`);
}

await main();
