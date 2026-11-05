/**
 * The favicon, and the lucide icon it is a copy of.
 *
 * The mark in the app is `Webhook` from `lucide-react`, imported like every other
 * icon. A browser tab cannot import anything — it has no cascade to inherit a
 * colour from and no React to get lucide's stroke defaults from — so the same
 * drawing has to exist a second time as a static file with everything written
 * out. That is the one duplication this package carries, and it is the kind that
 * drifts silently: nobody looks at a favicon twice, and a lucide upgrade that
 * redraws the icon would leave the tab showing the old one indefinitely.
 *
 * So the shapes are read back off a rendered `<Webhook />` rather than pasted into
 * an assertion. A version bump that changes the drawing fails here, which is the
 * moment to copy the new one across.
 */

import { render } from '@testing-library/react';
import { Webhook } from 'lucide-react';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { createElement } from 'react';
import { describe, expect, it } from 'vitest';

import { PACKAGE_ROOT } from '@testkit/paths.js';

function readIcon(name: string): string {
  return readFileSync(join(PACKAGE_ROOT, 'public', name), 'utf8');
}

const favicon = readIcon('favicon.svg');
/** The same mark with a dot, shown while an approval waits. */
const waiting = readIcon('favicon-alert.svg');

/** Every shape lucide draws for `webhook`, as `[tagName, attributes]`. */
function shapesOf(): ReadonlyArray<readonly [string, Record<string, string>]> {
  const { container } = render(createElement(Webhook));
  const svg = container.querySelector('svg');
  if (svg === null) throw new Error('Webhook rendered nothing');

  return [...svg.children].map((node) => [
    node.tagName,
    Object.fromEntries([...node.attributes].map((a) => [a.name, a.value])),
  ]);
}

describe.each([
  ['the favicon', favicon],
  ['the waiting favicon', waiting],
])('%s', (title, icon) => {
  it('draws the same shapes lucide does', () => {
    const shapes = shapesOf();

    // Guards the loop below: an icon that rendered nothing would pass it.
    expect(shapes.length).toBeGreaterThan(0);

    for (const [tag, attributes] of shapes) {
      if (tag === 'path') {
        expect(icon).toContain(`d="${attributes.d ?? ''}"`);
      } else {
        // `webhook` is three paths, so nothing reaches here today. It stays
        // because the branch is what makes a lucide upgrade that adds a circle
        // fail here rather than ship a half-drawn tab.
        for (const name of ['cx', 'cy', 'r']) {
          expect(icon, `${tag} ${name}`).toContain(
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
      expect(icon).toContain(attribute);
    }
  });

  it('writes its colour out, because a tab has no cascade to inherit from', () => {
    expect(icon).not.toContain('currentColor');
    // The brand hue a step under the accent's own lightness. A tab strip is
    // white in one OS theme and near-black in the other, and the terminal green
    // the app uses holds against only one of them. The favicon's own comment
    // carries the measurements.
    expect(icon).toContain('#009100');
  });
});

describe('the waiting favicon', () => {
  it('adds a red dot that holds on either tab strip', () => {
    // Measured like the mark: 4.4:1 on white, 4.2:1 on near-black.
    expect(waiting).toMatch(/<circle cx="19" cy="5" r="4.5" fill="#EB1C1C"/);
  });

  it('cuts the mark away around the dot, so it reads without the colour', () => {
    expect(waiting).toContain('<g mask="url(#dot-gap)">');
    expect(waiting).toMatch(/<mask id="dot-gap">/);
  });
});

describe('the tab', () => {
  it('is referenced by index.html, from this origin', () => {
    const index = readFileSync(join(PACKAGE_ROOT, 'index.html'), 'utf8');

    expect(index).toContain('rel="icon" href="/favicon.svg"');
  });
});
