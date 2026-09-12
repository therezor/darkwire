/**
 * @vitest-environment jsdom
 */

/**
 * The theme, and the flash.
 *
 * The interesting half of this file is the pre-paint script: it lives as inline
 * text in `index.html`, so nothing typechecks it, nothing bundles it, and
 * nothing would notice it going stale. So the test extracts it from the real
 * `index.html`, runs it against a stubbed DOM, and asserts it agrees with
 * `theme.ts` on all six combinations of stored preference and OS setting. Two
 * implementations of one rule are fine; two implementations that can disagree
 * without a test failing are how a page flashes white on every load.
 */

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  applyTheme,
  isThemePreference,
  readStoredPreference,
  resolveTheme,
  storePreference,
  systemPrefersLight,
  watchSystemTheme,
  THEME_STORAGE_KEY,
  type ResolvedTheme,
  type ThemePreference,
} from '@/theme/theme.js';
import { PACKAGE_ROOT } from '@testkit/paths.js';

const INDEX_HTML = join(PACKAGE_ROOT, 'index.html');
const html = readFileSync(INDEX_HTML, 'utf8');

/** Every listener the current `matchMedia` stub handed out, so a test can fire one. */
let listeners: Array<(event: MediaQueryListEvent) => void> = [];

/**
 * A real `Storage`, stubbed in rather than taken from the environment: jsdom's
 * `localStorage` is shadowed by node 26's own experimental global, which is
 * inert without `--localstorage-file`. Both the module and the inline script
 * read the global, so stubbing it is what tests them on the same footing.
 */
function memoryStorage(): Storage {
  const entries = new Map<string, string>();
  return {
    get length() {
      return entries.size;
    },
    key: (index: number) => [...entries.keys()][index] ?? null,
    getItem: (key: string) => entries.get(key) ?? null,
    setItem: (key: string, value: string) => {
      entries.set(key, value);
    },
    removeItem: (key: string) => {
      entries.delete(key);
    },
    clear: () => {
      entries.clear();
    },
  };
}

function stubMatchMedia(prefersLight: boolean): void {
  listeners = [];
  vi.stubGlobal('matchMedia', (query: string) => ({
    matches: query.includes('prefers-color-scheme: light') && prefersLight,
    media: query,
    addEventListener: (
      type: string,
      listener: (event: MediaQueryListEvent) => void,
    ) => {
      listeners.push(listener);
    },
    removeEventListener: (
      type: string,
      listener: (event: MediaQueryListEvent) => void,
    ) => {
      listeners = listeners.filter((existing) => existing !== listener);
    },
  }));
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage());
  delete document.documentElement.dataset.theme;
  stubMatchMedia(false);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

/**
 * Runs the extracted script the way the browser does. The rule this trips is
 * aimed at evaluating untrusted input; the input here is a file in this
 * repository, and running it is the only way to test the script that ships.
 */
function runScript(source: string): void {
  // eslint-disable-next-line @typescript-eslint/no-implied-eval, @typescript-eslint/no-unsafe-call
  new Function(source)();
}

/** The inline script, taken from the file that ships rather than a copy of it. */
function extractInlineScript(source: string): string {
  const match = /<script>([\s\S]*?)<\/script>/.exec(source);
  if (match?.[1] === undefined) {
    throw new Error('No inline script in index.html');
  }
  return match[1];
}

describe('resolveTheme', () => {
  it('lets an explicit choice win over the OS in both directions', () => {
    expect(resolveTheme('dark', true)).toBe('dark');
    expect(resolveTheme('light', false)).toBe('light');
  });

  it('follows the OS under `system`', () => {
    expect(resolveTheme('system', true)).toBe('light');
    expect(resolveTheme('system', false)).toBe('dark');
  });
});

describe('the stored preference', () => {
  it('defaults to `system` when absent or unrecognised', () => {
    expect(readStoredPreference()).toBe('system');

    localStorage.setItem(THEME_STORAGE_KEY, 'sepia');
    expect(readStoredPreference()).toBe('system');
  });

  it('round-trips each preference', () => {
    for (const preference of ['dark', 'light', 'system'] as const) {
      storePreference(preference);
      expect(readStoredPreference()).toBe(preference);
    }
  });

  it('survives storage that throws, which is Safari private mode', () => {
    const hostile = {
      getItem: () => {
        throw new Error('SecurityError');
      },
      setItem: () => {
        throw new Error('SecurityError');
      },
    } as unknown as Storage;

    expect(readStoredPreference(hostile)).toBe('system');
    expect(() => {
      storePreference('dark', hostile);
    }).not.toThrow();
  });

  it('recognises exactly the three preferences', () => {
    expect(isThemePreference('system')).toBe(true);
    expect(isThemePreference('auto')).toBe(false);
    expect(isThemePreference(null)).toBe(false);
  });
});

describe('applyTheme', () => {
  it('stamps the resolution on <html> and persists the preference', () => {
    expect(applyTheme('light')).toBe('light');

    expect(document.documentElement.dataset.theme).toBe('light');
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe('light');
  });

  it('stamps a resolution under `system`, not the word `system`', () => {
    stubMatchMedia(true);
    expect(applyTheme('system')).toBe('light');

    expect(document.documentElement.dataset.theme).toBe('light');
    // The *preference* is what is stored: `light` would turn "follow the OS"
    // into "light forever, because it was light when you first loaded".
    expect(localStorage.getItem(THEME_STORAGE_KEY)).toBe('system');
  });

  it('toggles by rewriting one attribute — no reload, no stylesheet swap', () => {
    applyTheme('dark');
    expect(document.documentElement.dataset.theme).toBe('dark');

    applyTheme('light');
    expect(document.documentElement.dataset.theme).toBe('light');
  });
});

describe('watchSystemTheme', () => {
  it('reports a change and unsubscribes cleanly', () => {
    const seen: boolean[] = [];
    const stop = watchSystemTheme((prefersLight) => seen.push(prefersLight));

    listeners.forEach((listener) => {
      listener({ matches: true } as MediaQueryListEvent);
    });
    stop();
    listeners.forEach((listener) => {
      listener({ matches: false } as MediaQueryListEvent);
    });

    expect(seen).toEqual([true]);
  });
});

describe('the pre-paint script in index.html', () => {
  /** The inline script, extracted from the real file. */
  const script = extractInlineScript(html);

  it('is inline, blocking, and ahead of the module script', () => {
    // Anything `defer`, `async`, `type="module"` or external paints the default
    // theme first and corrects it a frame later. That frame is the flash.
    expect(script).not.toContain('src=');
    expect(html.indexOf('<script>')).toBeLessThan(
      html.indexOf('type="module"'),
    );
    expect(html.indexOf('<script>')).toBeLessThan(html.indexOf('</head>'));
  });

  it('reads the same storage key the module writes', () => {
    expect(script).toContain(THEME_STORAGE_KEY);
  });

  it('agrees with resolveTheme on every combination', () => {
    const combinations: ReadonlyArray<
      readonly [ThemePreference | null, boolean]
    > = [
      ['dark', false],
      ['dark', true],
      ['light', false],
      ['light', true],
      ['system', false],
      ['system', true],
      [null, false],
      [null, true],
    ];

    for (const [stored, prefersLight] of combinations) {
      vi.stubGlobal('localStorage', memoryStorage());
      if (stored !== null) localStorage.setItem(THEME_STORAGE_KEY, stored);
      delete document.documentElement.dataset.theme;
      stubMatchMedia(prefersLight);

      runScript(script);

      const expected: ResolvedTheme = resolveTheme(
        stored ?? 'system',
        prefersLight,
      );
      expect(
        document.documentElement.dataset.theme,
        `stored=${String(stored)} prefersLight=${String(prefersLight)}`,
      ).toBe(expected);
    }
  });

  it('falls back to the OS when storage throws', () => {
    stubMatchMedia(true);
    vi.stubGlobal('localStorage', {
      getItem: () => {
        throw new Error('SecurityError');
      },
    });

    expect(() => {
      runScript(script);
    }).not.toThrow();
    expect(document.documentElement.dataset.theme).toBe('light');
  });
});

describe('systemPrefersLight', () => {
  it('answers false when the question cannot be asked', () => {
    expect(systemPrefersLight(undefined)).toBe(false);
  });
});
