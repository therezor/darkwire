/**
 * The locale, as React state.
 *
 * The initial value comes from storage rather than from the DOM, for the reason
 * `use-theme.ts` gives about the theme: the stamped attribute records the
 * *resolution*, and `en` on a German laptop could mean either "chose English"
 * or "system, which happens to resolve to English". Only the stored preference
 * distinguishes them, and only the preference can be round-tripped back into
 * the selector.
 *
 * `adopt` is the half the theme does not need. The install's locale arrives
 * later than the first paint — it comes from `GET /api/settings`, which needs a
 * session — so the config calls this once it knows, and it writes through to
 * storage so the next pre-auth paint is already right.
 */

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import type { Locale } from '@ghostwire/i18n';

import {
  applyLocale,
  browserLocales,
  readStoredPreference,
  resolvePreference,
  type LocalePreference,
} from './locale-preference.js';

export interface LocaleState {
  readonly preference: LocalePreference;
  /** What the page is actually rendered in. */
  readonly resolved: Locale;
  readonly setPreference: (preference: LocalePreference) => void;
  /**
   * Take the install's configured locale as the new preference.
   *
   * Separate from `setPreference` because it is not a *choice* — it is the
   * config catching up with a page that had to paint before it could be read.
   */
  readonly adopt: (locale: Locale) => void;
}

export function useLocale(): LocaleState {
  const { i18n } = useTranslation();
  const [preference, setPreferenceState] =
    useState<LocalePreference>(readStoredPreference);
  const [resolved, setResolved] = useState<Locale>(() =>
    resolvePreference(readStoredPreference(), browserLocales()),
  );

  const apply = useCallback((next: LocalePreference): void => {
    setPreferenceState(next);
    setResolved(applyLocale(next));
  }, []);

  const adopt = useCallback((locale: Locale): void => {
    // A no-op when the config agrees with what is already showing, so a
    // settings refetch on window focus does not re-render the whole tree.
    setPreferenceState((current) => (current === locale ? current : locale));
    setResolved((current) =>
      current === locale ? current : applyLocale(locale),
    );
  }, []);

  // i18next owns the strings; this owns the preference. Keeping the two in step
  // here rather than at every call site is the whole job of this hook.
  useEffect(() => {
    if (i18n.language !== resolved) void i18n.changeLanguage(resolved);
  }, [i18n, resolved]);

  return { preference, resolved, setPreference: apply, adopt };
}
