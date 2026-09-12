/**
 * One zone, shared, for everything this browser renders a time in.
 *
 * This is the *display* half of `ui.timezone`; the scheduler reads the same
 * setting to decide when a cron expression fires. Having exactly one copy is
 * the point — two would let the automation list and the run history disagree
 * about what `09:00` meant, which is the failure the setting exists to end.
 *
 * Follows the locale rather than the theme, because it is the same kind of
 * thing: an install-wide answer that arrives as a query, not a per-browser
 * preference kept in `localStorage`. There is deliberately no pre-paint script
 * for it — an unstyled flash is visible and a timestamp that resolves a beat
 * late is not, so the cost the theme pays to avoid a flicker buys nothing here.
 *
 * A component rendered outside the provider gets the browser's own zone rather
 * than a throw, matching `useAppLocale` and `useAppTheme`: a component under
 * test that renders one timestamp should not have to mount an app first.
 */

import { useQuery } from '@tanstack/react-query';
import { createContext, useContext, type JSX, type ReactNode } from 'react';

import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';

const TimezoneContext = createContext<string | undefined>(undefined);

/**
 * The zone this browser is in, or UTC if it will not say.
 *
 * The fallback matters more than it looks: `resolvedOptions().timeZone` is
 * specified to return an IANA name, but a stripped runtime can return an empty
 * string, and an empty string handed to `Intl.DateTimeFormat` throws rather
 * than defaulting — which would take out every screen that renders a date.
 */
export function browserTimezone(): string {
  try {
    const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
    return zone === '' ? 'UTC' : zone;
  } catch {
    return 'UTC';
  }
}

export function TimezoneProvider({
  children,
}: {
  readonly children: ReactNode;
}): JSX.Element {
  // The same query key the Settings screen uses, so a session that visits it
  // makes no second request — and an unclaimed install pays exactly one 401,
  // which `query.ts` does not retry. Identical reasoning to `useConfigLocale`.
  const settings = useQuery({
    queryKey: queryKeys.settings,
    queryFn: ({ signal }) => api.settings(signal),
  });

  // The browser's zone until the install answers. Not UTC: before sign-in there
  // is no install opinion to honour, and a timestamp in the reader's own zone is
  // the better guess — the same rule the language follows before `ui.locale`
  // can be read.
  const zone = settings.data?.config.ui.timezone ?? browserTimezone();

  return <TimezoneContext value={zone}>{children}</TimezoneContext>;
}

/** The install's display zone, or this browser's when there is no provider. */
export function useAppTimezone(): string {
  return useContext(TimezoneContext) ?? browserTimezone();
}
