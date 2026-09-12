/**
 * Theme resolution.
 *
 * Three states, not two: `system` is a real preference and not the absence of
 * one, so a user who has never touched the toggle follows their OS and a user
 * who chose dark on a light laptop stays dark. That is why the stored value is
 * the *preference* and the stamped value is the *resolution* — storing only the
 * resolution would silently convert "follow the system" into "dark, forever,
 * because it was dark when you first loaded the page".
 *
 * The stamp is an attribute on `<html>` rather than a class, because
 * `tokens.css` overrides `prefers-color-scheme` in both directions with
 * `[data-theme]` selectors — and because the pre-paint script in `index.html`
 * has to write the same thing before React exists. That script and this module
 * are two implementations of one rule; `theme.test.ts` runs both and asserts
 * they agree on every input.
 */

/** What the user chose. */
export type ThemePreference = 'dark' | 'light' | 'system';

/** What the page is actually painted in. */
export type ResolvedTheme = 'dark' | 'light';

/** Shared with the pre-paint script in `index.html`, which cannot import it. */
export const THEME_STORAGE_KEY = 'ghostai.theme';

/** The media query both implementations ask. */
const LIGHT_QUERY = '(prefers-color-scheme: light)';

const PREFERENCES: readonly ThemePreference[] = ['dark', 'light', 'system'];

export function isThemePreference(value: unknown): value is ThemePreference {
  return (
    typeof value === 'string' &&
    (PREFERENCES as readonly string[]).includes(value)
  );
}

/** The rule, in one place: an explicit choice wins, and `system` asks the OS. */
export function resolveTheme(
  preference: ThemePreference,
  systemPrefersLight: boolean,
): ResolvedTheme {
  if (preference === 'system') return systemPrefersLight ? 'light' : 'dark';
  return preference;
}

/**
 * Reads the stored preference, defaulting to `system`.
 *
 * Storage access is wrapped because it throws rather than returning null in a
 * cross-origin iframe and under Safari's private mode — and a theme lookup is
 * not worth a blank page.
 */
export function readStoredPreference(
  storage: Storage | undefined = safeStorage(),
): ThemePreference {
  try {
    const stored = storage?.getItem(THEME_STORAGE_KEY);
    return isThemePreference(stored) ? stored : 'system';
  } catch {
    return 'system';
  }
}

export function storePreference(
  preference: ThemePreference,
  storage: Storage | undefined = safeStorage(),
): void {
  try {
    storage?.setItem(THEME_STORAGE_KEY, preference);
  } catch {
    // A theme that does not survive a reload beats a toggle that throws.
  }
}

/** True when the OS asks for light. False whenever the question cannot be asked. */
export function systemPrefersLight(
  view: Window | undefined = defaultView(),
): boolean {
  return view?.matchMedia(LIGHT_QUERY).matches ?? false;
}

/**
 * Applies a preference: stamps the resolution and persists the choice.
 *
 * Returns the resolution so a caller can drive state from it without asking the
 * DOM back what it just wrote.
 */
export function applyTheme(
  preference: ThemePreference,
  options: {
    readonly document?: Document;
    readonly view?: Window;
    readonly persist?: boolean;
  } = {},
): ResolvedTheme {
  const doc = options.document ?? globalThis.document;
  const resolved = resolveTheme(preference, systemPrefersLight(options.view));

  doc.documentElement.dataset.theme = resolved;
  if (options.persist !== false) storePreference(preference);

  return resolved;
}

/**
 * Calls back when the OS theme changes, and returns the unsubscribe.
 *
 * Only meaningful under `system` — but subscribing unconditionally and
 * filtering at the callback is one fewer listener to get wrong on every toggle.
 */
export function watchSystemTheme(
  onChange: (systemPrefersLight: boolean) => void,
  view: Window | undefined = defaultView(),
): () => void {
  const query = view?.matchMedia(LIGHT_QUERY);
  if (query === undefined) return () => undefined;

  const listener = (event: MediaQueryListEvent): void => {
    onChange(event.matches);
  };

  query.addEventListener('change', listener);
  return () => {
    query.removeEventListener('change', listener);
  };
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
