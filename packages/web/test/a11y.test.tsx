/**
 * The rules that hold across every component, rather than inside one.
 *
 * Two of them are source sweeps, for the same reason the token gates are: a
 * component test can only assert about a component someone remembered to write
 * a test for, and the failure mode here is a *new* component that quietly opts
 * out. The third mounts the style guide — the one page that instantiates every
 * primitive — and walks it with a keyboard.
 */

import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { TokensRoute } from '@/routes/tokens.js';
import { collectSources } from '@/tokens/run-gates.js';
import { renderWithProviders } from '@testkit/render.js';

const sources = collectSources();

describe('the focus ring', () => {
  it('is never removed', () => {
    // `base.css` gives every focusable element a `:focus-visible` ring. The
    // only way to lose it is to remove it — and the usual reason is a designer
    // objecting to the ring on a *mouse* click, which `:focus-visible` already
    // solved. There is no legitimate use of these here.
    const offenders = sources
      .filter(({ file }) => file !== 'src/styles/base.css')
      .flatMap(({ file, source }) =>
        [
          ...source.matchAll(
            /(?:focus(?:-visible)?:)?outline-none|outline:\s*none/g,
          ),
        ].map((match) => `${file}: ${match[0]}`),
      );

    expect(offenders).toEqual([]);
  });

  it('is defined once, for everything, rather than per component', () => {
    const base = sources.find(({ file }) => file === 'src/styles/base.css');

    expect(base?.source).toMatch(/:focus-visible\s*\{[^}]*outline:/);
    // A stroke, so the accent *text* token — the fill is a 2:1 pairing on a
    // white card, which is the whole reason the third gate exists.
    expect(base?.source).toContain('var(--accent-fg)');
  });
});

describe('interactive elements', () => {
  it('carry an accessible name even when they are icon-only', () => {
    // An icon button with no label is announced as "button" and nothing else.
    const offenders = sources
      .filter(({ file }) => file.endsWith('.tsx'))
      .flatMap(({ file, source }) =>
        [...source.matchAll(/size="icon"[\s\S]{0,240}?<\/Button>/g)]
          .filter(
            (match) => !/aria-label|aria-labelledby|sr-only/.test(match[0]),
          )
          .map(() => file),
      );

    expect(offenders).toEqual([]);
  });

  it('are all reachable by keyboard on the page that instantiates every one', async () => {
    const user = userEvent.setup();
    renderWithProviders(<TokensRoute />);

    // Everything the page renders that a user is expected to operate.
    const interactive = [
      ...screen.getAllByRole('button'),
      ...screen.getAllByRole('switch'),
      ...screen.getAllByRole('tab'),
      ...screen.getAllByRole('combobox'),
      ...screen.getAllByRole('textbox'),
    ].filter((element) => !element.hasAttribute('disabled'));

    expect(interactive.length).toBeGreaterThan(20);

    const reached = new Set<Element>();
    // One tab per element plus slack for the roving tabindex groups, which
    // deliberately consume a single stop for a whole set.
    for (let i = 0; i < interactive.length + 10; i += 1) {
      await user.tab();
      if (document.activeElement !== null) reached.add(document.activeElement);
    }

    const unreachable = interactive
      .filter((element) => !reached.has(element))
      // A tab list is one tab stop by design: arrow keys move within it, which
      // `primitives.test.tsx` asserts separately.
      .filter(
        (element) =>
          element.getAttribute('role') !== 'tab' || element.tabIndex === 0,
      );

    expect(unreachable.map((element) => element.textContent)).toEqual([]);
  });
});
