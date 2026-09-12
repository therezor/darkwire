/**
 * The install's locale, adopted once it can be read.
 *
 * The preference has two sources and they do not arrive together. Before
 * sign-in there is no config to read, so the language is whatever the browser
 * asked for; after sign-in `config.ui.locale` is the install's own answer and
 * wins. This is the seam between the two, and it runs everywhere rather than on
 * the Settings screen, because a locale that only applied once you visited
 * Settings would not be the install's locale at all.
 *
 * Fetching unconditionally is deliberate. On an unclaimed or signed-out install
 * this 401s, which is the correct answer rather than an error worth handling —
 * `query.ts` never retries a 401, so it costs exactly one request and the page
 * stays in the language the browser asked for. Sharing the query key with the
 * Settings screen means a session that visits it makes no second request.
 */

import { useQuery } from '@tanstack/react-query';
import { useEffect } from 'react';

import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';

import type { LocaleState } from './use-locale.js';

export function useConfigLocale(locale: LocaleState): void {
  const settings = useQuery({
    queryKey: queryKeys.settings,
    queryFn: ({ signal }) => api.settings(signal),
  });

  const configured = settings.data?.config.ui.locale;

  useEffect(() => {
    // `adopt` is a no-op when it already agrees, so this does not re-render the
    // tree every time the settings query is invalidated by an unrelated save.
    if (configured !== undefined) locale.adopt(configured);
  }, [configured, locale]);
}
