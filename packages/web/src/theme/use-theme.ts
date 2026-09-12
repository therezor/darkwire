/**
 * The theme, as React state.
 *
 * The initial value comes from storage rather than from the DOM: the stamped
 * attribute records the *resolution*, and `dark` on a light laptop could mean
 * either "chose dark" or "system, which is dark right now". Only the stored
 * preference distinguishes them, and only the preference can be round-tripped
 * back into the toggle.
 */

import { useCallback, useEffect, useState } from 'react';

import {
  applyTheme,
  readStoredPreference,
  resolveTheme,
  systemPrefersLight,
  watchSystemTheme,
  type ResolvedTheme,
  type ThemePreference,
} from './theme.js';

export interface ThemeState {
  readonly preference: ThemePreference;
  readonly resolved: ResolvedTheme;
  readonly setPreference: (preference: ThemePreference) => void;
}

export function useTheme(): ThemeState {
  const [preference, setPreferenceState] =
    useState<ThemePreference>(readStoredPreference);
  const [resolved, setResolved] = useState<ResolvedTheme>(() =>
    resolveTheme(readStoredPreference(), systemPrefersLight()),
  );

  const setPreference = useCallback((next: ThemePreference): void => {
    setPreferenceState(next);
    setResolved(applyTheme(next));
  }, []);

  // A `system` preference has to keep tracking after the first paint: the OS
  // can flip at sunset, and the pre-paint script only ran once.
  useEffect(() => {
    if (preference !== 'system') return undefined;
    return watchSystemTheme(() => {
      setResolved(applyTheme('system', { persist: false }));
    });
  }, [preference]);

  return { preference, resolved, setPreference };
}
