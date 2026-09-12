/**
 * Colour maths for the token gates.
 *
 * This exists because the contrast assertion has to answer a question the
 * browser answers at paint time — "what does `oklch(0.664 0.005 77.3)` on
 * `oklch(0.225 0.005 77.3)` actually measure?" — in a node test, before any of
 * it ships. It is a converter, not a colour library: it supports exactly the
 * notations `tokens.css` is allowed to use, and anything else is an error
 * rather than a silent fallback that would make the assertion meaningless.
 *
 * The chain is OKLCH → Oklab → linear sRGB → sRGB, then WCAG relative
 * luminance. Out-of-gamut colours are reported rather than clamped: a seed the
 * display cannot show is a seed whose measured contrast is fiction, so
 * `contrast.test.ts` fails on one instead of quietly testing a colour nobody
 * will ever see.
 */

/** A colour resolved all the way to sRGB, channels 0–1, alpha 0–1. */
export interface Rgba {
  readonly r: number;
  readonly g: number;
  readonly b: number;
  readonly a: number;
  /** True when any linear channel fell outside 0–1 before clamping. */
  readonly outOfGamut: boolean;
}

/** OKLCH → sRGB. `l` is 0–1, `c` is absolute, `h` is degrees. */
export function oklchToRgba(
  l: number,
  c: number,
  h: number,
  alpha: number = 1,
): Rgba {
  const hRad = (h * Math.PI) / 180;
  const a = c * Math.cos(hRad);
  const b = c * Math.sin(hRad);

  const lms1 = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const lms2 = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const lms3 = (l - 0.0894841775 * a - 1.291485548 * b) ** 3;

  const lr = 4.0767416621 * lms1 - 3.3077115913 * lms2 + 0.2309699292 * lms3;
  const lg = -1.2684380046 * lms1 + 2.6097574011 * lms2 - 0.3413193965 * lms3;
  const lb = -0.0041960863 * lms1 - 0.7034186147 * lms2 + 1.707614701 * lms3;

  // A hair of slack: the matrices round, and pure white lands just past 1.
  const EPSILON = 1e-6;
  const outOfGamut = [lr, lg, lb].some((v) => v < -EPSILON || v > 1 + EPSILON);

  return {
    r: encodeSrgb(clamp01(lr)),
    g: encodeSrgb(clamp01(lg)),
    b: encodeSrgb(clamp01(lb)),
    a: alpha,
    outOfGamut,
  };
}

/** sRGB channels 0–255 → the same structure, so `rgb()` tokens compare directly. */
export function rgb255ToRgba(
  r: number,
  g: number,
  b: number,
  alpha: number = 1,
): Rgba {
  return { r: r / 255, g: g / 255, b: b / 255, a: alpha, outOfGamut: false };
}

/**
 * Composites `fg` over `bg`, in sRGB space.
 *
 * Not colour-correct — a browser composites in the same non-linear space, so
 * matching what ships beats matching the physics.
 */
export function composite(fg: Rgba, bg: Rgba): Rgba {
  if (fg.a >= 1) return fg;
  const a = fg.a;
  return {
    r: fg.r * a + bg.r * (1 - a),
    g: fg.g * a + bg.g * (1 - a),
    b: fg.b * a + bg.b * (1 - a),
    a: 1,
    outOfGamut: fg.outOfGamut || bg.outOfGamut,
  };
}

/** WCAG 2.1 relative luminance. Expects an opaque colour. */
export function relativeLuminance({ r, g, b }: Rgba): number {
  return (
    0.2126 * decodeSrgb(r) + 0.7152 * decodeSrgb(g) + 0.0722 * decodeSrgb(b)
  );
}

/** WCAG 2.1 contrast ratio, 1–21. Both colours must be opaque. */
export function contrastRatio(a: Rgba, b: Rgba): number {
  const la = relativeLuminance(a);
  const lb = relativeLuminance(b);
  const [hi, lo] = la >= lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}

/** `#rrggbb`, for a failure message a human can paste into a colour picker. */
export function toHex({ r, g, b }: Rgba): string {
  const channel = (v: number): string =>
    Math.round(clamp01(v) * 255)
      .toString(16)
      .padStart(2, '0');
  return `#${channel(r)}${channel(g)}${channel(b)}`;
}

function clamp01(v: number): number {
  return v < 0 ? 0 : v > 1 ? 1 : v;
}

function encodeSrgb(v: number): number {
  return v <= 0.0031308 ? v * 12.92 : 1.055 * v ** (1 / 2.4) - 0.055;
}

function decodeSrgb(v: number): number {
  return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
}
