/**
 * Anchors for the colour maths.
 *
 * The contrast assertion is only as trustworthy as the converter underneath it,
 * and a converter with a transposed matrix row still returns plausible numbers
 * for every input. These are the values that pin it: sRGB primaries have known
 * OKLCH coordinates and known WCAG luminances, so a wrong sign shows up here
 * rather than as a palette that measures fine and looks wrong.
 */

import { describe, expect, it } from 'vitest';

import {
  composite,
  contrastRatio,
  oklchToRgba,
  relativeLuminance,
  rgb255ToRgba,
  toHex,
} from '@/tokens/color.js';

describe('oklchToRgba', () => {
  it('maps the achromatic ends to black and white', () => {
    expect(toHex(oklchToRgba(0, 0, 0))).toBe('#000000');
    expect(toHex(oklchToRgba(1, 0, 0))).toBe('#ffffff');
  });

  it('reproduces the sRGB primaries from their OKLCH coordinates', () => {
    expect(toHex(oklchToRgba(0.6279, 0.2577, 29.23))).toBe('#ff0000');
    expect(toHex(oklchToRgba(0.8664, 0.2948, 142.5))).toBe('#00ff00');
    expect(toHex(oklchToRgba(0.452, 0.3132, 264.05))).toBe('#0000ff');
  });

  it('reports a colour the display cannot show rather than silently clipping', () => {
    expect(oklchToRgba(0.75, 0.3, 250).outOfGamut).toBe(true);
    expect(oklchToRgba(0.75, 0.12, 250).outOfGamut).toBe(false);
  });

  it('treats a white that rounds past 1.0 as in gamut', () => {
    expect(oklchToRgba(1, 0, 77.3).outOfGamut).toBe(false);
  });
});

describe('relativeLuminance', () => {
  it('matches the WCAG definition at the primaries', () => {
    expect(relativeLuminance(rgb255ToRgba(255, 255, 255))).toBeCloseTo(1, 5);
    expect(relativeLuminance(rgb255ToRgba(0, 0, 0))).toBeCloseTo(0, 5);
    expect(relativeLuminance(rgb255ToRgba(0, 255, 0))).toBeCloseTo(0.7152, 4);
  });
});

describe('contrastRatio', () => {
  it('is 21:1 black on white, in either order', () => {
    const black = rgb255ToRgba(0, 0, 0);
    const white = rgb255ToRgba(255, 255, 255);

    expect(contrastRatio(black, white)).toBeCloseTo(21, 4);
    expect(contrastRatio(white, black)).toBeCloseTo(21, 4);
  });

  it('is 1:1 against itself', () => {
    expect(
      contrastRatio(rgb255ToRgba(18, 18, 18), rgb255ToRgba(18, 18, 18)),
    ).toBeCloseTo(1, 6);
  });
});

describe('composite', () => {
  it('leaves an opaque colour alone', () => {
    const opaque = rgb255ToRgba(10, 20, 30);
    expect(composite(opaque, rgb255ToRgba(255, 255, 255))).toBe(opaque);
  });

  it('blends an overlay towards its backdrop', () => {
    const white50 = rgb255ToRgba(255, 255, 255, 0.5);
    expect(toHex(composite(white50, rgb255ToRgba(0, 0, 0)))).toBe('#808080');
  });

  it('is what makes an alpha overlay measurable at all', () => {
    // The hover token is exactly this: white at 5% over the page surface.
    const surface = oklchToRgba(0.159, 0.005, 77.3);
    const hovered = composite(rgb255ToRgba(255, 255, 255, 0.05), surface);

    expect(relativeLuminance(hovered)).toBeGreaterThan(
      relativeLuminance(surface),
    );
  });
});
