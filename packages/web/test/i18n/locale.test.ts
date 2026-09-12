/**
 * @vitest-environment jsdom
 */

/**
 * The locale, and the two attributes it stamps.
 *
 * Same shape as `theme.test.ts`, and for the same reason: the pre-paint script
 * lives as inline text in `index.html`, so nothing typechecks it, nothing
 * bundles it, and nothing would notice it going stale. This extracts it from
 * the real file, runs it against a stubbed DOM, and asserts it agrees with
 * `locale-preference.ts`. Two implementations of one rule are fine; two that
 * can disagree without a test failing are how a page announces itself in the
 * wrong language for a frame — or reflows right-to-left after the first paint.
 */

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  LOCALE_STORAGE_KEY,
  applyLocale,
  browserLocales,
  directionOf,
  isLocalePreference,
  readStoredPreference,
  resolvePreference,
  storePreference,
} from '@/i18n/locale-preference.js';
import { PACKAGE_ROOT } from '@testkit/paths.js';

const INDEX_HTML = join(PACKAGE_ROOT, 'index.html');
const html = readFileSync(INDEX_HTML, 'utf8');

/** See the note in `theme.test.ts`: node's own inert global shadows jsdom's. */
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

function stubBrowserLanguages(languages: readonly string[]): void {
  Object.defineProperty(globalThis.navigator, 'languages', {
    value: languages,
    configurable: true,
  });
}

beforeEach(() => {
  vi.stubGlobal('localStorage', memoryStorage());
  stubBrowserLanguages(['en']);
  document.documentElement.removeAttribute('lang');
  document.documentElement.removeAttribute('dir');
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('resolvePreference', () => {
  it('lets an explicit choice win over the browser', () => {
    expect(resolvePreference('en', ['de', 'fr'])).toBe('en');
  });

  it('asks the browser under `system`', () => {
    expect(resolvePreference('system', ['en'])).toBe('en');
  });

  it('falls back rather than rendering nothing when nothing matches', () => {
    // Only English ships today, so every one of these lands on it. The point is
    // that it lands somewhere renderable rather than throwing.
    expect(resolvePreference('system', ['ja', 'ko'])).toBe('en');
    expect(resolvePreference('system', [])).toBe('en');
    expect(resolvePreference('zz', [])).toBe('en');
  });
});

describe('the stored preference', () => {
  it('defaults to `system` when absent', () => {
    expect(readStoredPreference()).toBe('system');
  });

  it('round-trips a choice', () => {
    storePreference('en');
    expect(readStoredPreference()).toBe('en');

    storePreference('system');
    expect(readStoredPreference()).toBe('system');
  });

  it('keeps a tag for a language that no longer ships', () => {
    // Storage is not the place to validate this. A config or a stored choice
    // naming a removed language should resolve back to the default, not read as
    // corrupt — `resolvePreference` is where that decision belongs.
    expect(isLocalePreference('de-AT')).toBe(true);
    expect(isLocalePreference('')).toBe(false);
    expect(isLocalePreference(null)).toBe(false);
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
      storePreference('en', hostile);
    }).not.toThrow();
  });
});

describe('browserLocales', () => {
  it('reports what the browser asks for, most-preferred first', () => {
    stubBrowserLanguages(['de-AT', 'de', 'en']);

    expect(browserLocales()).toEqual(['de-AT', 'de', 'en']);
  });

  it('answers empty when there is no window to ask', () => {
    // These functions also run under `renderToStaticMarkup`, where `window` is
    // not a thing. Stubbing the global is the only way to reach that branch —
    // passing `undefined` would just re-apply the default parameter.
    vi.stubGlobal('window', undefined);

    expect(browserLocales()).toEqual([]);
  });
});

describe('directionOf', () => {
  it('flows right-to-left only for the languages that do', () => {
    expect(directionOf('en')).toBe('ltr');
    expect(directionOf('de-AT')).toBe('ltr');
    expect(directionOf('ar-EG')).toBe('rtl');
    expect(directionOf('he')).toBe('rtl');
  });
});

describe('applyLocale', () => {
  it('stamps both attributes, because they answer different questions', () => {
    // `lang` is what a screen reader picks its voice from; `dir` is what the
    // layout flows by.
    applyLocale('en');

    expect(document.documentElement.lang).toBe('en');
    expect(document.documentElement.dir).toBe('ltr');
  });

  it('persists the preference, not the resolution', () => {
    // Storing the resolution would turn "follow my browser" into a fixed
    // language the first time the page loaded.
    applyLocale('system');

    expect(localStorage.getItem(LOCALE_STORAGE_KEY)).toBe('system');
  });

  it('can stamp without persisting', () => {
    applyLocale('en', { persist: false });

    expect(localStorage.getItem(LOCALE_STORAGE_KEY)).toBeNull();
  });
});

describe('the pre-paint script in index.html', () => {
  const script = extractInlineScript(html);

  it('reads the same storage key the module writes', () => {
    expect(script).toContain(LOCALE_STORAGE_KEY);
  });

  it('agrees with resolvePreference on every combination', () => {
    const combinations: ReadonlyArray<
      readonly [string | null, readonly string[]]
    > = [
      ['en', ['en']],
      ['en', ['de']],
      ['system', ['en']],
      ['system', []],
      [null, ['en']],
      [null, []],
    ];

    for (const [stored, languages] of combinations) {
      vi.stubGlobal('localStorage', memoryStorage());
      if (stored !== null) localStorage.setItem(LOCALE_STORAGE_KEY, stored);
      stubBrowserLanguages(languages);
      document.documentElement.removeAttribute('lang');

      runScript(script);

      const expected = resolvePreference(stored ?? 'system', languages);
      expect(
        document.documentElement.lang,
        `stored=${String(stored)} languages=${languages.join()}`,
      ).toBe(expected);
    }
  });

  it('stamps the direction of the locale it resolved, not the one that was asked for', () => {
    // Both attributes describe the same rendered page, so they are read off the
    // same answer. A stored `ar` on a build that ships no Arabic resolves to
    // English, and stamping `dir="rtl"` beside `lang="en"` would right-align
    // English prose — a worse outcome than the untranslated text itself.
    //
    // `applyLocale` rather than `directionOf` on the raw tag: the contract this
    // file exists to hold is that the script and the module agree, so the module
    // is what the script is measured against.
    for (const locale of ['en', 'de-AT', 'ar', 'ar-EG', 'he', 'fa']) {
      vi.stubGlobal('localStorage', memoryStorage());
      localStorage.setItem(LOCALE_STORAGE_KEY, locale);
      document.documentElement.removeAttribute('dir');
      document.documentElement.removeAttribute('lang');

      runScript(script);
      const fromScript = {
        lang: document.documentElement.lang,
        dir: document.documentElement.dir,
      };

      applyLocale(locale, { persist: false });

      expect(fromScript.lang, locale).toBe(document.documentElement.lang);
      expect(fromScript.dir, locale).toBe(document.documentElement.dir);
      expect(fromScript.dir, locale).toBe(directionOf(fromScript.lang));
    }
  });

  it('falls back to the browser when storage throws', () => {
    stubBrowserLanguages(['en']);
    vi.stubGlobal('localStorage', {
      getItem: () => {
        throw new Error('SecurityError');
      },
    });

    expect(() => {
      runScript(script);
    }).not.toThrow();
    expect(document.documentElement.lang).toBe('en');
  });
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

function extractInlineScript(source: string): string {
  const match = /<script>([\s\S]*?)<\/script>/.exec(source);
  if (match?.[1] === undefined) {
    throw new Error('No inline script in index.html');
  }
  return match[1];
}
