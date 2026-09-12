/**
 * One configured i18next, built the same way everywhere.
 *
 * Four of i18next's defaults are wrong for this product, and each one is a
 * silent behaviour change rather than an error, so they are set here once
 * instead of being rediscovered per consumer:
 *
 *  - **`escapeValue: false`.** The default HTML-escapes every interpolated
 *    value. React already escapes what it renders, so leaving it on
 *    double-escapes a workspace called `Tom & Jerry` into `Tom &amp; Jerry` —
 *    and the CLI writes to a terminal, where the entity is simply wrong.
 *  - **`returnNull` / `returnEmptyString: false`.** A key present but empty is
 *    a translation nobody has finished; falling back to English reads better
 *    than a blank label.
 *  - **`fallbackLng: 'en'`.** A partially translated locale renders English for
 *    what is missing rather than the key itself.
 *  - **`missingKeyHandler` that throws under test.** A typo'd key is a bug that
 *    should stop the suite, not a `chat.send` rendered on a button. In
 *    production it is the opposite: the key is returned and the app keeps
 *    working, because a missing string is not worth a white screen.
 *
 * `initAsync: false` matters more than it looks: i18next defaults to loading
 * resources inside a `setTimeout`, which is right for a backend plugin and
 * wrong for bundles that are already in memory. Left on, `t` returns keys for
 * one tick after `init` — and the CLI has no await to hang that on before it
 * prints help.
 */

import { createInstance, type i18n, type Resource } from 'i18next';

import { DEFAULT_LOCALE, type Locale } from './locale.js';

/** The namespaces the product splits its strings across. */
export type Namespace = 'web' | 'cli';

interface CreateI18nOptions {
  /** The resolved locale. Not negotiated here — see `resolveLocale`. */
  readonly locale?: Locale;
  /** Bundles, keyed by locale then namespace. */
  readonly resources: Resource;
  readonly defaultNS: Namespace;
  /**
   * Throw on a missing key rather than returning it.
   *
   * Defaults to on under `NODE_ENV=test`, which is what turns a typo into a
   * failing assertion in the suite that would otherwise have rendered it.
   */
  readonly strict?: boolean;
}

export function createI18n(options: CreateI18nOptions): i18n {
  const instance = createInstance();
  const strict = options.strict ?? process.env.NODE_ENV === 'test';

  void instance.init({
    lng: options.locale ?? DEFAULT_LOCALE,
    fallbackLng: DEFAULT_LOCALE,
    resources: options.resources,
    defaultNS: options.defaultNS,
    ns: Object.keys(options.resources[DEFAULT_LOCALE] ?? {}),

    // Resources are bundled, so there is nothing to wait for and every caller
    // can use `t` on the next line.
    initAsync: false,

    returnNull: false,
    returnEmptyString: false,

    interpolation: {
      // React escapes; a terminal has no entities. See the note above.
      escapeValue: false,
    },

    // `missingKeyHandler` is only consulted when `saveMissing` is on, which is
    // why the two move together rather than the handler standing alone.
    saveMissing: strict,
    missingKeyHandler: strict
      ? (lngs: readonly string[], ns: string, key: string): void => {
          throw new Error(`Missing translation: ${ns}:${key}`);
        }
      : false,
  });

  return instance;
}
