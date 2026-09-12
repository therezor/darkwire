/**
 * Locale resolution, in the browser.
 *
 * Three states, not two, for the same reason `theme.ts` has three: `system` is a
 * real preference and not the absence of one. A user who has never opened
 * Settings follows the browser, and a user who chose English on a German laptop
 * stays in English. Storing only the *resolution* would silently convert
 * "follow my browser" into "English, forever, because that is what the browser
 * was set to the first time the page loaded".
 *
 * The order matters more here than it does for the theme, because a locale has
 * two sources and they become available at different times:
 *
 *  1. **Before sign-in** — the login and setup overlays render with no config to
 *     read, so the answer is the stored preference, then the browser. That is
 *     the honest default: nobody has *chosen* a language yet, so the one their
 *     browser asks for is the best guess available.
 *  2. **After sign-in** — `config.ui.locale` is the install's answer and wins,
 *     and it is cached back into storage so the *next* pre-auth paint is already
 *     right. The cache is a cache; the config is the truth.
 *
 * Storage access is wrapped because it throws rather than returning null in a
 * cross-origin iframe and under Safari's private mode — and a language lookup is
 * not worth a blank page.
 */

import {
  DEFAULT_LOCALE,
  isRtl,
  resolveFirstLocale,
  type Locale,
} from '@ghostwire/i18n';

/**
 * What the user chose: a BCP-47 tag, or `SYSTEM` meaning "ask the browser".
 *
 * A plain string rather than the `Locale | 'system'` this reads as, because
 * `Locale` is itself a string and that union collapses to `string` — the
 * compiler says so, and a type that lies about being narrower is worse than one
 * that admits it is not. `SYSTEM` carries the meaning instead.
 */
export type LocalePreference = string;

/** The preference that defers to the browser rather than naming a language. */
export const SYSTEM: LocalePreference = 'system';

/** Shared with `index.html`'s pre-paint script, which cannot import it. */
export const LOCALE_STORAGE_KEY = 'ghostai.locale';

export function isLocalePreference(value: unknown): value is LocalePreference {
  // Deliberately not checked against `SUPPORTED_LOCALES`: a stored tag for a
  // language that has since been removed should resolve back to the default
  // rather than be treated as corrupt storage.
  return typeof value === 'string' && value.length > 0;
}

/** What the browser asks for, most-preferred first. */
export function browserLocales(
  view: Window | undefined = defaultView(),
): readonly string[] {
  return view?.navigator.languages ?? [];
}

/**
 * The rule, in one place.
 *
 * `preference` first when it is an explicit choice, then the browser's list,
 * then the default. Passing `SYSTEM` is what skips straight to the browser.
 */
export function resolvePreference(
  preference: LocalePreference,
  fromBrowser: readonly string[],
): Locale {
  const requested =
    preference === SYSTEM ? fromBrowser : [preference, ...fromBrowser];
  return resolveFirstLocale(requested);
}

/** Reads the stored preference, defaulting to `SYSTEM`. */
export function readStoredPreference(
  storage: Storage | undefined = safeStorage(),
): LocalePreference {
  try {
    const stored = storage?.getItem(LOCALE_STORAGE_KEY);
    return isLocalePreference(stored) ? stored : SYSTEM;
  } catch {
    return SYSTEM;
  }
}

export function storePreference(
  preference: LocalePreference,
  storage: Storage | undefined = safeStorage(),
): void {
  try {
    storage?.setItem(LOCALE_STORAGE_KEY, preference);
  } catch {
    // A language that does not survive a reload beats a selector that throws.
  }
}

/**
 * Stamps the resolution onto `<html>` and returns it.
 *
 * Both attributes, because they answer different questions: `lang` is what a
 * screen reader switches voice on and what a spellchecker uses, and `dir` is
 * what the layout flows by. Returning the resolution lets a caller drive state
 * from it without asking the DOM back what it just wrote.
 */
export function applyLocale(
  preference: LocalePreference,
  options: {
    readonly document?: Document;
    readonly view?: Window;
    readonly persist?: boolean;
  } = {},
): Locale {
  const doc = options.document ?? globalThis.document;
  const resolved = resolvePreference(preference, browserLocales(options.view));

  doc.documentElement.lang = resolved;
  doc.documentElement.dir = directionOf(resolved);
  if (options.persist !== false) storePreference(preference);

  return resolved;
}

/** The `dir` attribute a locale wants. */
export function directionOf(locale: Locale): 'ltr' | 'rtl' {
  return isRtl(locale) ? 'rtl' : 'ltr';
}

/**
 * `window`, when there is one. Typed as optional rather than read straight from
 * `globalThis`, which types it as always present: these functions also run
 * under `renderToStaticMarkup`, where it is not.
 */
function defaultView(): Window | undefined {
  return typeof window === 'undefined' ? undefined : window;
}

function safeStorage(): Storage | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

export { DEFAULT_LOCALE };
