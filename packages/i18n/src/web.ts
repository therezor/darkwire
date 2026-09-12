/**
 * The browser's instance: `web`, and nothing the terminal says.
 */

import type { i18n } from 'i18next';

import { createI18n } from './instance.js';
import { DEFAULT_LOCALE, type Locale } from './locale.js';
import { EN } from './resources.js';

/** Bundles a browser needs, in the shape i18next wants them. */
const WEB_RESOURCES = {
  en: { web: EN.web },
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
