/**
 * The favicon, and the lucide icon it is a copy of.
 *
 * The mark in the app is `Skull` from `lucide-react`, imported like every other
 * icon. A browser tab cannot import anything — it has no cascade to inherit a
 * colour from and no React to get lucide's stroke defaults from — so the same
 * drawing has to exist a second time as a static file with everything written
 * out. That is the one duplication this package carries, and it is the kind that
 * drifts silently: nobody looks at a favicon twice, and a lucide upgrade that
 * redraws the icon would leave the tab showing the old one indefinitely.
 *
 * So the shapes are read back off a rendered `<Skull />` rather than pasted into
 * an assertion. A version bump that changes the drawing fails here, which is the
 * moment to copy the new one across.
 */

import { render } from '@testing-library/react';
import { Skull } from 'lucide-react';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { createElement } from 'react';
import { describe, expect, it } from 'vitest';

import { PACKAGE_ROOT } from '@testkit/paths.js';

const favicon = readFileSync(
  join(PACKAGE_ROOT, 'public', 'favicon.svg'),
  'utf8',
);

/** Every shape lucide draws for `skull`, as `[tagName, attributes]`. */
function shapesOf(): ReadonlyArray<readonly [string, Record<string, string>]> {
  const { container } = render(createElement(Skull));
  const svg = container.querySelector('svg');
  if (svg === null) throw new Error('Skull rendered nothing');

  return [...svg.children].map((node) => [
    node.tagName,
    Object.fromEntries([...node.attributes].map((a) => [a.name, a.value])),
  ]);
}

describe('the favicon', () => {
  it('draws the same shapes lucide does', () => {
    const shapes = shapesOf();

    // Guards the loop below: an icon that rendered nothing would pass it.
    expect(shapes.length).toBeGreaterThan(0);

    for (const [tag, attributes] of shapes) {
      if (tag === 'path') {
        expect(favicon).toContain(`d="${attributes.d ?? ''}"`);
      } else {
        // A circle is three numbers rather than one string, and the file writes
        // them on separate lines, so each is checked on its own.
        for (const name of ['cx', 'cy', 'r']) {
          expect(favicon, `${tag} ${name}`).toContain(
            `${name}="${attributes[name] ?? ''}"`,
          );
        }
      }
    }
  });

  it('carries the stroke attributes the component gets from lucide', () => {
    // Without these the tab shows two dots and nothing else: every shape in the
    // drawing is stroked, and an SVG's default fill is black with no stroke.
    for (const attribute of [
      'fill="none"',
      'stroke-width="2"',
      'stroke-linecap="round"',
      'stroke-linejoin="round"',
    ]) {
      expect(favicon).toContain(attribute);
    }
  });

  it('writes its colour out, because a tab has no cascade to inherit from', () => {
    expect(favicon).not.toContain('currentColor');
    // The brand *hue* at mid-luminance — `oklch(0.6 0.13 132)`, not the accent
    // token's own lightness. A tab strip is white in one OS theme and near-black
    // in the other, and the light green the app uses holds against only one of
    // them. The favicon's own comment carries the measurements.
    expect(favicon).toContain('#629036');
  });

  it('is referenced by index.html, from this origin', () => {
    const index = readFileSync(join(PACKAGE_ROOT, 'index.html'), 'utf8');

    expect(index).toContain('rel="icon" href="/favicon.svg"');
  });
});
