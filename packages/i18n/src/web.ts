/**
 * The browser's instance: `web`, and nothing the terminal says.
 */

import type { i18n } from 'i18next';

import web from '@darkwire/i18n/locales/en/web.json' with { type: 'json' };

import { createI18n } from './instance.js';
import { DEFAULT_LOCALE, type Locale } from './locale.js';

/**
 * Bundles a browser needs, in the shape i18next wants them.
 *
 * `web.json` by itself rather than through `EN`, which also holds the CLI's
 * strings. Reaching through it put the whole terminal vocabulary in the
 * browser's first download. See `resources.ts` for why the specifier is a
 * self-reference.
 */
const WEB_RESOURCES = {
  en: { web },
} as const;

export function createWebI18n(
  locale: Locale = DEFAULT_LOCALE,
  strict?: boolean,
): i18n {
  return createI18n({
    locale,
    resources: WEB_RESOURCES,
    defaultNS: 'web',
    ...(strict === undefined ? {} : { strict }),
  });
}
