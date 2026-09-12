/**
 * Structural properties of the token sheet — the things the contrast assertion
 * cannot see because they are about how the sheet is *written* rather than what
 * it resolves to.
 *
 * The first is the important one. `tokens.css` declares the light seeds twice,
 * once under `prefers-color-scheme` and once under `[data-theme='light']`,
 * because a media query and an attribute selector cannot share a selector list.
 * Everything else in this file is downstream of that duplication being safe.
 */

import { describe, expect, it } from 'vitest';

import {
  parseDeclarations,
  readTokensCss,
  resolveTokens,
} from '@/tokens/sheet.js';

const css = readTokensCss();
const declarations = parseDeclarations(css);

describe('the two light seed blocks', () => {
  const inMediaQuery = declarations.filter(
    (declaration) => declaration.media !== undefined,
  );
  const inAttribute = declarations.filter((declaration) =>
    declaration.selector.includes("[data-theme='light']"),
  );

  it('both exist', () => {
    expect(inMediaQuery.length).toBeGreaterThan(0);
    expect(inAttribute.length).toBe(inMediaQuery.length);
  });

  it('declare exactly the same properties with the same values', () => {
    const pairs = (source: typeof declarations): readonly string[] =>
      source.map(({ property, value }) => `${property}: ${value}`);

    expect(pairs(inAttribute)).toEqual(pairs(inMediaQuery));
  });

  it('override every seed the dark block declares', () => {
    // A seed missing from the light blocks silently keeps its dark value —
    // which is how a light theme ends up with one component still inverted.
    const dark = new Set(
      declarations
        .filter((declaration) => declaration.media === undefined)
        .filter((declaration) => declaration.selector.trim() === ':root')
        .filter((declaration) => declaration.property.startsWith('--seed-'))
        .map((declaration) => declaration.property),
    );
    const light = new Set(
      inAttribute.map((declaration) => declaration.property),
    );

    // Shared on purpose. The gold is the gold — only its lightness and chroma
    // move between themes — and `--seed-on-fill-l` is text on a fill, which is
    // light in both themes, so it is near-black in both.
    const shared = [
      '--seed-accent-l',
      '--seed-accent-c',
      '--seed-accent-h',
      '--seed-neutral-h',
      '--seed-on-fill-l',
    ];
    const missing = [...dark].filter(
      (seed) => !light.has(seed) && !shared.includes(seed),
    );

    expect(missing).toEqual([]);
  });
});

describe('the derived layer', () => {
  const declared = new Set(
    declarations.map((declaration) => declaration.property),
  );

  it('exposes every colour the stylesheets use', () => {
    for (const token of [
      '--surface-0',
      '--surface-3',
      '--fg-1',
      '--fg-3',
      '--hover',
      '--line',
      '--accent',
      '--accent-fg',
      '--on-fill',
      '--danger-soft',
      '--danger-edge',
      '--scrim',
    ]) {
      expect(declared).toContain(token);
    }
  });

  it('resolves every token to a literal in both themes', () => {
    for (const theme of ['dark', 'light'] as const) {
      for (const [property, value] of resolveTokens(css, theme)) {
        expect(value, `${property} in ${theme}`).not.toContain('var(');
      }
    }
  });

  /**
   * There is no framework underneath this sheet any more, so a colour that only
   * exists as a seed is a colour no stylesheet can name. Every seed has to be
   * consumed by something in the derived block — an orphan is a value someone
   * added and then wired up nowhere.
   */
  it('consumes every seed it declares', () => {
    const seeds = [...declared].filter((property) =>
      property.startsWith('--seed-'),
    );
    const derived = declarations
      .filter((declaration) => !declaration.property.startsWith('--seed-'))
      .map((declaration) => declaration.value)
      .join(' ');

    const orphans = seeds.filter((seed) => !derived.includes(seed));
    expect(orphans).toEqual([]);
  });
});

describe('sizing', () => {
  /**
   * Every scale in the sheet, not only the type scale. With no utility
   * framework there is nowhere else a length can come from, so a `px` that
   * slipped into any of these is a piece of the UI that stopped honouring the
   * browser's font size.
   */
  const PREFIXES = [
    '--text-',
    '--leading-',
    '--radius-',
    '--space-',
    '--size-',
    '--layout-',
  ];

  const sizes = declarations.filter((declaration) =>
    PREFIXES.some((prefix) => declaration.property.startsWith(prefix)),
  );

  it('covers every scale', () => {
    // Guards the list above: a scale renamed out from under these prefixes
    // would otherwise make the assertion below vacuously pass.
    //
    // Per prefix rather than a floor on the total, which is what this used to
    // be. A total is the wrong shape for the claim: it conflates "a scale went
    // missing" — the thing worth catching — with "a scale got shorter", which
    // is a design decision and has since been made deliberately in every one of
    // these ramps. A count of 40 would now fail on a sheet that is *better*
    // than the one it was written against.
    const missing = PREFIXES.filter(
      (prefix) =>
        !sizes.some((declaration) => declaration.property.startsWith(prefix)),
    );

    expect(missing).toEqual([]);
  });

  it('is expressed in rem, so the UI honours the browser font size', () => {
    for (const { property, value } of sizes) {
      expect(value, property).toMatch(/rem$/);
    }
  });

  // "never overrides the root font size" lives in `stylesheets.test.ts` now.
  // It used to be a regex over this file's raw text, which cannot tell a rule
  // from a comment *about* a rule — and the comment explaining why the root
  // font size is never set had to spell one out. The replacement parses rules
  // and covers every stylesheet plus `index.html`, which is the scope the claim
  // was always making anyway.
});
