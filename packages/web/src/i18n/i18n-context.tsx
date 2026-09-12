/**
 * One locale, shared, and one i18next beneath it.
 *
 * The split mirrors `theme-context.tsx` and exists for the same reason: the
 * strings and the *preference* are two different things, and only one of them
 * belongs to the library. `I18nextProvider` gives the tree its `t`; this file
 * decides how many copies of the preference exist, which is one — two would let
 * the Settings selector repaint while the sidebar stayed in the old language.
 *
 * A component rendered outside the provider gets a detached, non-reactive
 * reading rather than a throw. Not defensive padding: `applyLocale` stamps
 * `<html lang>`, so the DOM already carries the answer, and a component under
 * test that only needs to know which language it is in should not have to mount
 * an app to find out.
 */

import { createWebI18n } from '@ghostwire/i18n/web';
import {
  createContext,
  useContext,
  useState,
  type JSX,
  type ReactNode,
} from 'react';
import { I18nextProvider } from 'react-i18next';

import {
  browserLocales,
  readStoredPreference,
  resolvePreference,
} from './locale-preference.js';
import { useConfigLocale } from './use-config-locale.js';
import { useLocale, type LocaleState } from './use-locale.js';

const LocaleContext = createContext<LocaleState | undefined>(undefined);

export function I18nProvider({
  children,
}: {
  readonly children: ReactNode;
}): JSX.Element {
  // `useState` rather than a module constant: an instance created at import
  // time is shared by every test in a file, so one test's `changeLanguage`
  // leaks into the next one's expectations.
  const [instance] = useState(() =>
    createWebI18n(resolvePreference(readStoredPreference(), browserLocales())),
  );

  return (
    <I18nextProvider i18n={instance}>
      <LocaleBridge>{children}</LocaleBridge>
    </I18nextProvider>
  );
}

/**
 * Inside `I18nextProvider` rather than beside it, because `useLocale` calls
 * `useTranslation` to drive `changeLanguage` — and that needs the instance the
 * provider above supplies.
 */
function LocaleBridge({
  children,
}: {
  readonly children: ReactNode;
}): JSX.Element {
  const locale = useLocale();
  // The install's own answer, once there is a session to read it with. Before
  // that the browser's language stands.
  useConfigLocale(locale);

  return <LocaleContext value={locale}>{children}</LocaleContext>;
}

/** The shared locale, or a read of what is on `<html>` when there is no provider. */
export function useAppLocale(): LocaleState {
  return useContext(LocaleContext) ?? detached();
}

function detached(): LocaleState {
  const preference = readStoredPreference();
  const stamped = globalThis.document.documentElement.lang;
  const resolved =
    stamped === '' ? resolvePreference(preference, browserLocales()) : stamped;

  return {
    preference,
    resolved,
    setPreference: () => {
      // Nothing is listening. A selector outside the provider would change the
      // attribute and no component's state, which is worse than doing nothing.
    },
    adopt: () => {
      // Same reasoning: there is no tree to tell.
    },
  };
}
