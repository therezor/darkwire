/**
 * The style guide page.
 *
 * It is a smoke test with a purpose beyond "does it render": this page is the
 * one surface that instantiates every token and every recipe variant, so if it
 * mounts and its markup contains no literal colour, then every token in the
 * sheet is reachable from a component without one. The gate sweep in
 * `tokens/gates.test.ts` checks the source; this checks the output.
 */

import { screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { TokensRoute } from '@/routes/tokens.js';
import { readTokensCss } from '@/tokens/sheet.js';
import { renderWithProviders } from '@testkit/render.js';

describe('the style guide', () => {
  it('renders every surface, text tier and role', () => {
    renderWithProviders(<TokensRoute />);

    for (const token of [
      'surface-0',
      'surface-3',
      'fg-1',
      'fg-3',
      'accent',
      'danger',
      'info',
    ]) {
      expect(screen.getAllByText(new RegExp(token)).length).toBeGreaterThan(0);
    }
  });

  it('exercises every button and badge variant, which is what makes it a review surface', () => {
    renderWithProviders(<TokensRoute />);

    for (const variant of ['primary', 'secondary', 'ghost', 'danger', 'link']) {
      expect(
        screen.getByRole('button', { name: `${variant} md` }),
      ).toBeInTheDocument();
    }
    // soft, solid and outline × six roles.
    expect(screen.getAllByText('warning')).toHaveLength(4);
  });

  it('paints itself from tokens rather than from literals', () => {
    const { container } = renderWithProviders(<TokensRoute />);

    expect(container.innerHTML).toContain('var(--surface-0)');
    expect(container.innerHTML).not.toMatch(/#[0-9a-fA-F]{6}/);
  });

  /**
   * Every token this page names must exist.
   *
   * This page is the one place in the codebase that builds token names from
   * data — `var(--${role}-soft)` — and a template string is invisible to every
   * search for a token's callers. That is not a hypothetical: a pass that
   * deleted the tokens nothing referenced deleted `--active`, because grepping
   * for `var(--active)` found nothing while this page was rendering it. The
   * result was a swatch painted with an undefined property, which is not an
   * error anywhere — it is a silently transparent box on a page whose entire
   * job is to show what the colours are.
   */
  it('names only tokens the sheet actually declares', () => {
    const { container } = renderWithProviders(<TokensRoute />);
    const declared = new Set(
      [...readTokensCss().matchAll(/^\s+(--[a-z0-9-]+):/gm)].map(
        (match) => match[1],
      ),
    );

    const referenced = [
      ...new Set(
        [...container.innerHTML.matchAll(/var\((--[a-z0-9-]+)\)/g)].map(
          (m) => m[1],
        ),
      ),
    ];

    // Guards the assertion below against an innerHTML that stopped carrying
    // inline `var()` at all.
    expect(referenced.length).toBeGreaterThan(20);
    expect(referenced.filter((token) => !declared.has(token))).toEqual([]);
  });
});
