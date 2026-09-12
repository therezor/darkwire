/**
 * The contrast assertion.
 *
 * Every text-on-surface pairing the token layer permits, measured in both
 * themes against WCAG AA. This is the test that makes the seed block safe to
 * edit: a designer moving `--seed-text-3` two points darker gets a failing
 * suite naming the pairing and the ratio, rather than a settings panel that
 * turns out to be unreadable on a white card six steps later.
 *
 * It is deliberately exhaustive rather than representative. `--fg-3` on
 * `--surface-3` is the tightest pairing in the sheet and the one nobody thinks
 * to check by eye, because tertiary text on an elevated surface is a
 * combination that only shows up inside a popover.
 */

import { describe, expect, it } from 'vitest';

import { composite, contrastRatio, toHex, type Rgba } from '@/tokens/color.js';
import {
  readTokensCss,
  resolveTokens,
  toRgba,
  type ThemeName,
} from '@/tokens/sheet.js';

/** WCAG 2.1 AA for body text. Everything in the sheet is held to it. */
const AA_NORMAL = 4.5;

const THEMES: readonly ThemeName[] = ['dark', 'light'];
const SURFACES = ['--surface-0', '--surface-1', '--surface-2', '--surface-3'];

/** Every token intended to carry text or an icon on a surface. */
const FOREGROUNDS = [
  '--fg-1',
  '--fg-2',
  '--fg-3',
  '--accent-fg',
  '--success-fg',
  '--warning-fg',
  '--danger-fg',
  '--info-fg',
];

/** Fills that carry text, and the one token that goes on top of them. */
const FILLS = ['--accent', '--success', '--warning', '--danger', '--info'];

const css = readTokensCss();

describe.each(THEMES)('%s theme', (theme) => {
  const tokens = resolveTokens(css, theme);
  const color = (name: string): Rgba => {
    const value = tokens.get(name);
    if (value === undefined) throw new Error(`No such token: ${name}`);
    return toRgba(value);
  };

  it.each(
    FOREGROUNDS.flatMap((fg) =>
      SURFACES.map((surface) => [fg, surface] as const),
    ),
  )('%s on %s meets AA', (fg, surface) => {
    const background = color(surface);
    const ratio = contrastRatio(composite(color(fg), background), background);

    expect(
      ratio,
      `${fg} (${toHex(color(fg))}) on ${surface} (${toHex(background)}) is ${ratio.toFixed(2)}:1`,
    ).toBeGreaterThanOrEqual(AA_NORMAL);
  });

  it.each(FILLS)('--on-fill meets AA on %s', (fill) => {
    const background = color(fill);
    const ratio = contrastRatio(
      composite(color('--on-fill'), background),
      background,
    );

    expect(
      ratio,
      `--on-fill on ${fill} is ${ratio.toFixed(2)}:1`,
    ).toBeGreaterThanOrEqual(AA_NORMAL);
  });

  /**
   * A soft fill is a badge background, so the text it carries is the matching
   * `-fg` token — over whatever surface the badge happens to sit on. Worst case
   * is the surface with the least contrast against the text, so all four are
   * checked rather than an assumed one.
   */
  it.each(['accent', 'success', 'warning', 'danger', 'info'])(
    '%s-fg on %s-soft meets AA over every surface',
    (role) => {
      for (const surface of SURFACES) {
        const background = composite(color(`--${role}-soft`), color(surface));
        const ratio = contrastRatio(
          composite(color(`--${role}-fg`), background),
          background,
        );

        expect(
          ratio,
          `--${role}-fg on --${role}-soft over ${surface} is ${ratio.toFixed(2)}:1`,
        ).toBeGreaterThanOrEqual(AA_NORMAL);
      }
    },
  );

  /**
   * A border is not text, but a border nobody can see is not a border. 1.5:1 is
   * the informal floor for a non-text boundary; WCAG's 3:1 applies to controls
   * whose *state* the border conveys, which the primitives own.
   */
  it('lines are visible against the surfaces they sit on', () => {
    for (const surface of SURFACES) {
      const background = color(surface);
      const ratio = contrastRatio(
        composite(color('--line-strong'), background),
        background,
      );

      expect(
        ratio,
        `--line-strong on ${surface} is ${ratio.toFixed(2)}:1`,
      ).toBeGreaterThan(1.5);
    }
  });

  /**
   * A colour outside sRGB is clipped by the display, so its measured contrast
   * is a number describing a colour the user never sees. Every assertion above
   * depends on this one.
   */
  it('every colour is inside the sRGB gamut', () => {
    // Every value that *is* a colour, rather than every token whose name looks
    // like one: the sheet no longer namespaces colours, so the notation is the
    // only honest filter. A shadow starts with a length, so it does not match.
    const outside = [...tokens]
      .filter(([, value]) => /^(?:oklch|rgb)\(/.test(value))
      .filter(([, value]) => toRgba(value).outOfGamut)
      .map(([name, value]) => `${name}: ${value}`);

    expect(outside).toEqual([]);
  });
});
