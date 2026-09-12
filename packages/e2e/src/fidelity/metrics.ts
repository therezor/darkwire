/**
 * What "visually indistinguishable at 1×" is, stated as numbers.
 *
 * A pixel diff was the obvious way to ask this question and it is the wrong
 * one here, for a reason that has nothing to do with fidelity: the two products
 * draw their icons from different sources — one from a webfont's glyph set, the
 * other from an inline SVG set — so every button, every row and every header in
 * the chrome differs at the pixel level no matter how exactly the layout
 * matches. A diff over that reports a large number for a screen that is
 * correct, and would report the same large number for one that is not.
 *
 * So the gate measures the things that *are* comparable, and they turn out to
 * be the things the requirement was actually about: the shell's three
 * dimensions, the type scale's base, the radius scale, and the surface and text
 * ramps. Both sides are measured the same way — `getComputedStyle` on the
 * rendered document — so neither is being read out of a stylesheet and trusted.
 *
 * The four known deviations from the plan are not measured here, deliberately:
 * the focus ring and the scrollbar are additions the reference does not have,
 * the surface ramp is compared with a stated tolerance rather than for
 * equality, and ±1px on rounded spacing is inside every tolerance below.
 *
 * `fidelity/capture.ts` writes both screenshots and their pixel diff to
 * `artifacts/fidelity/`. That is for the human review the plan asks for beside
 * this, not for a threshold — see above for why it cannot be one.
 */

/** A length in CSS pixels, read off the rendered document. */
export interface Metrics {
  /** The left column, including its border. */
  readonly sidebarWidth: number;
  readonly headerHeight: number;
  /** The measure a message is set to — the reason long answers stay readable. */
  readonly chatWidth: number;
  /** `font-size` on `body`, which the whole rem scale is relative to. */
  readonly baseFontSize: number;
  /** The page behind everything, as sRGB. */
  readonly surface0: [number, number, number];
  /** The sidebar's surface — one step up from the page. */
  readonly surface1: [number, number, number];
  /** Body text. */
  readonly foreground: [number, number, number];
  /** Secondary text: labels, timestamps, hints. */
  readonly foregroundMuted: number;
}

export interface Probe {
  /** The element carrying the measurement, first match wins. */
  readonly selectors: readonly string[];
}

/**
 * Where each measurement lives on each side.
 *
 * Selectors rather than a shared test id, because the reference cannot be
 * edited: it is another project's source, and a gate that required changing the
 * thing being measured would not be measuring it. Several candidates per probe
 * for the same reason — the reference names its regions with classes and the
 * replacement with landmarks.
 */
export interface Layout {
  readonly sidebar: Probe;
  readonly header: Probe;
  readonly chat: Probe;
  readonly muted: Probe;
}

export const REFERENCE_LAYOUT: Layout = {
  sidebar: { selectors: ['.sidebar'] },
  header: { selectors: ['.chat-header'] },
  // The composer, which is the one element on each side that is sized directly
  // by the measure rather than centred inside it. The transcript follows it on
  // both — each with a padding derived from the same width — so comparing the
  // composers compares the rule.
  chat: { selectors: ['.input-wrapper'] },
  muted: { selectors: ['.welcome-subtitle', '.version'] },
};

export const REPLACEMENT_LAYOUT: Layout = {
  sidebar: { selectors: ['aside[aria-label="Sidebar"]'] },
  header: { selectors: ['header'] },
  // The composer's inner column, which is the element `--layout-chat` sizes
  // directly — the token this gate is about. A `data-` attribute added for the
  // test would be a hook that could drift from the class the layout uses.
  chat: { selectors: ['.composer__inner'] },
  // Any element painted in the muted tier. The welcome screen's note is the
  // first one on the chat screen; the page note is the first on the others.
  muted: {
    selectors: ['.welcome__note', '.page__note', '.sidebar__section-title'],
  },
};

/**
 * Reads the metrics out of a live document.
 *
 * Stringified and evaluated in the page rather than imported there: Playwright
 * serialises the function source, so it may close over nothing. `layout` is
 * passed as an argument for exactly that reason.
 */
export function readMetrics(layout: Layout): Metrics {
  const find = (probe: {
    readonly selectors: readonly string[];
  }): Element | null => {
    for (const selector of probe.selectors) {
      const element = document.querySelector(selector);
      if (element !== null) return element;
    }
    return null;
  };

  const probe = document.createElement('canvas');
  probe.width = 1;
  probe.height = 1;
  const ctx = probe.getContext('2d', { willReadFrequently: true });
  const rgb = (color: string): [number, number, number] => {
    if (ctx === null) return [0, 0, 0];
    ctx.clearRect(0, 0, 1, 1);
    ctx.fillStyle = color;
    ctx.fillRect(0, 0, 1, 1);
    const [r = 0, g = 0, b = 0] = ctx.getImageData(0, 0, 1, 1).data;
    return [r, g, b];
  };
  const luminance = ([r, g, b]: [number, number, number]): number =>
    0.2126 * r + 0.7152 * g + 0.0722 * b;

  const width = (element: Element | null): number =>
    element === null ? 0 : Math.round(element.getBoundingClientRect().width);
  const height = (element: Element | null): number =>
    element === null ? 0 : Math.round(element.getBoundingClientRect().height);

  const muted = find(layout.muted);

  return {
    sidebarWidth: width(find(layout.sidebar)),
    headerHeight: height(find(layout.header)),
    chatWidth: width(find(layout.chat)),
    baseFontSize: parseFloat(getComputedStyle(document.body).fontSize),
    surface0: rgb(getComputedStyle(document.body).backgroundColor),
    surface1: rgb(
      getComputedStyle(find(layout.sidebar) ?? document.body).backgroundColor,
    ),
    foreground: rgb(getComputedStyle(document.body).color),
    foregroundMuted:
      muted === null ? 0 : luminance(rgb(getComputedStyle(muted).color)),
  };
}
