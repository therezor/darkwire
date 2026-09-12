/**
 * One theme, shared.
 *
 * `useTheme` is per-component state, which was fine while the toggle was its
 * only caller. It stopped being fine here: a code block picks its syntax theme
 * from the resolution, and two independent `useState`s mean pressing Light
 * repaints the toggle and leaves every code block on the dark palette until
 * something else remounts it.
 *
 * So the state is hoisted into a context mounted once in `providers.tsx`, and
 * `useTheme` stays exactly what it was — the rule, the persistence and the OS
 * subscription — with this file deciding only *how many copies of it exist*.
 *
 * A component rendered outside the provider gets a detached, non-reactive
 * reading rather than a throw. That is not defensive padding: `applyTheme`
 * stamps `<html>`, so the DOM already carries the answer, and a component under
 * test that only wants to know which palette it is in should not have to mount
 * an app to find out.
 */

import { createContext, useContext, type JSX, type ReactNode } from 'react';

import {
  readStoredPreference,
  resolveTheme,
  systemPrefersLight,
  type ResolvedTheme,
} from './theme.js';
import { useTheme, type ThemeState } from './use-theme.js';

const ThemeContext = createContext<ThemeState | undefined>(undefined);

export function ThemeProvider({
  children,
}: {
  readonly children: ReactNode;
}): JSX.Element {
  const theme = useTheme();
  return <ThemeContext value={theme}>{children}</ThemeContext>;
}

/** The shared theme, or a read of what is on `<html>` when there is no provider. */
export function useAppTheme(): ThemeState {
  return useContext(ThemeContext) ?? detached();
}

function detached(): ThemeState {
  const preference = readStoredPreference();
  const stamped = globalThis.document.documentElement.dataset.theme;
  const resolved: ResolvedTheme =
    stamped === 'dark' || stamped === 'light'
      ? stamped
      : resolveTheme(preference, systemPrefersLight());

  return {
    preference,
    resolved,
    setPreference: () => {
      // Nothing is listening. A toggle outside the provider would change the
      // attribute and no component's state, which is worse than doing nothing.
    },
  };
}
