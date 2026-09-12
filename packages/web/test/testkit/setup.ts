/**
 * Shared setup for the component tests.
 *
 * Everything here exists because jsdom is not a browser in one specific way
 * each primitive depends on. Radix asks the layout engine questions jsdom has
 * no answer for — element sizes for collision-aware placement, pointer capture
 * for the select, `ResizeObserver` for anchored positioning — and without these
 * stubs the components throw rather than fail an assertion, which makes every
 * test read as a bug in the component.
 */

import '@testing-library/jest-dom/vitest';

import { createWebI18n } from '@ghostwire/i18n/web';
import { setI18n } from 'react-i18next';

import { cleanup, configure } from '@testing-library/react';
import { afterEach, beforeEach, vi } from 'vitest';

import { resetToasts } from '@/components/ui/toast.js';
import { resetConnection } from '@/lib/connection.js';
import { useTurnStore } from '@/state/turn.js';

/**
 * A default instance for components rendered without the provider.
 *
 * `renderWithProviders` mounts the real `Providers` stack, so most tests get
 * their `t` from `I18nProvider`. The primitives in `components/ui` are
 * deliberately tested *bare* — that is the point of `primitives.test.tsx` — and
 * `useTranslation` outside a provider falls back to react-i18next's global,
 * which is unset by default and makes `t` return the key. Setting it here means
 * a bare `<Dialog>` still renders `Close` rather than `common.close`, without
 * the primitive tests having to mount an app to prove a close button exists.
 *
 * Production never relies on this: `I18nProvider` sits above the whole tree.
 */
setI18n(createWebI18n('en'));

/**
 * `findBy*` and `waitFor` give up after one second by default, and that budget
 * is testing-library's own — the 15s `testTimeout` in `vitest.config.ts` does
 * not reach it, which is why raising that one did not stop the coverage job
 * from failing. A page like the agent editor mounts the whole shell and settles
 * half a dozen queries before the form exists: ~0.4s bare, ~1.8s under v8
 * instrumentation, and a shared CI runner is slower again. So the query
 * expired, the test read it as "the button is not there", and the failure
 * pointed at the component rather than the clock.
 *
 * Five seconds is a stall, not a slow render. Nothing here waits on a real
 * network — every route is stubbed — so a query that genuinely cannot resolve
 * still fails well inside the test timeout, with the same message.
 */
configure({ asyncUtilTimeout: 5_000 });

/** A no-op observer. Nothing is being laid out, so there is nothing to report. */
class NoopResizeObserver {
  observe(): void {
    // Nothing to observe: jsdom has no layout.
  }
  unobserve(): void {
    // See above.
  }
  disconnect(): void {
    // See above.
  }
}

/**
 * A socket that never connects.
 *
 * jsdom implements `WebSocket` for real, so the shell mounting the transport
 * would have every component test open a TCP connection to a server that is not
 * running — and then reconnect to it on a backoff schedule for the rest of the
 * file. A suite that wants to drive the transport constructs
 * `ReconnectingSocket` with its own `create`; nothing else should be dialling
 * anything.
 */
class InertWebSocket {
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;

  send(): void {
    // Nowhere to send it.
  }
  close(): void {
    // Never opened.
  }
}

/**
 * Storage in a `Map`, replaced per test.
 *
 * Two reasons, and only the second is about isolation. The first is that Node
 * 26 ships its own experimental `sessionStorage` global which shadows jsdom's
 * and is inert without `--localstorage-file`, so a `setItem` succeeds and a
 * `getItem` returns null — which reads as a bug in the code under test. The
 * second is that the reconnect cursor is deliberately durable, and durable
 * across a page reload should not mean durable across a test file.
 */
class MemoryStorage implements Storage {
  private readonly entries = new Map<string, string>();

  get length(): number {
    return this.entries.size;
  }
  clear(): void {
    this.entries.clear();
  }
  getItem(key: string): string | null {
    return this.entries.get(key) ?? null;
  }
  key(index: number): string | null {
    return [...this.entries.keys()][index] ?? null;
  }
  removeItem(key: string): void {
    this.entries.delete(key);
  }
  setItem(key: string, value: string): void {
    this.entries.set(key, value);
  }
}

beforeEach(() => {
  // The DOM lib types all of these as always present, which is exactly why
  // their absence is a surprise: a menu that refuses to open rather than a
  // type error. `Object.assign` because they live on the prototype, where
  // `vi.stubGlobal` cannot reach.
  Object.assign(Element.prototype, {
    hasPointerCapture: () => false,
    setPointerCapture: () => undefined,
    releasePointerCapture: () => undefined,
    // Called when a menu opens on its highlighted item.
    scrollIntoView: () => undefined,
    // The transcript pins itself to the bottom through this one.
    scrollTo: () => undefined,
  });

  vi.stubGlobal('ResizeObserver', NoopResizeObserver);
  vi.stubGlobal('WebSocket', InertWebSocket);
  vi.stubGlobal('sessionStorage', new MemoryStorage());
  // Pinned, not inherited. Every assertion in this suite matches on English,
  // and `navigator.languages` is jsdom's reading of the machine — so without
  // this the suite passes here and fails on a laptop set to German, which is
  // the least useful moment to find out. The same reason `localStorage` above
  // is a fresh `MemoryStorage`: a preference must not survive into the next
  // test and silently pick the language for it.
  vi.stubGlobal('localStorage', new MemoryStorage());
  // Defined onto the real `navigator` rather than stubbed over it: replacing the
  // whole object would drop `userAgent` and everything else jsdom puts there,
  // and these two are the only properties that decide a language.
  for (const [property, value] of [
    ['languages', ['en']],
    ['language', 'en'],
  ] as const) {
    Object.defineProperty(globalThis.navigator, property, {
      value,
      configurable: true,
    });
  }
  // The router scrolls to the top on every navigation; jsdom logs a
  // "Not implemented" line for each one.
  vi.stubGlobal('scrollTo', () => undefined);

  vi.stubGlobal('matchMedia', (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    addListener: () => undefined,
    removeListener: () => undefined,
    dispatchEvent: () => false,
  }));
});

afterEach(() => {
  cleanup();
  // All three are module state: without this, one test's toast is another
  // test's unexpected element, a connection status leaks across files, and a
  // socket opened by a shell that was never unmounted keeps its listeners.
  resetToasts();
  resetConnection();
  useTurnStore.getState().reset();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});
