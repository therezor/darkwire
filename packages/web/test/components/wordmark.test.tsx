/**
 * The lockup, and the two things about it that are easy to get wrong and
 * invisible once wrong.
 *
 * A decorated name is one thing, not two: if the mark reaches the accessibility
 * tree, a screen reader announces the brand twice — once as an image and once as
 * text — and nobody reviewing the markup notices, because it looks right.
 *
 * The second is the reason this is a component at all. It carries no type of its
 * own, so a caller's class decides the size, the weight and the colour; a font
 * size that crept in here would silently win over the login card's micro-label
 * and be correct in the header, which is the theme most of the work happens in.
 */

import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { Wordmark } from '@/components/wordmark.js';

describe('the wordmark', () => {
  it('reads as one name, with the mark hidden from the accessibility tree', () => {
    const { container, getByText } = render(<Wordmark />);

    // Exact string, not a substring: the mark is a sibling element rather than
    // a character in the text, which is what keeps this matching.
    expect(getByText('GhostAI')).toBeInTheDocument();
    expect(container.querySelector('svg')).toHaveAttribute(
      'aria-hidden',
      'true',
    );
    expect(container.textContent).toBe('GhostAI');
  });

  it("keeps the caller's class alongside its own", () => {
    const { container } = render(<Wordmark className="app-header__brand" />);
    const root = container.firstElementChild;

    // Both, in that order: `.wordmark` is the structure and the caller's class
    // is the type, and dropping either is a lockup that is half-styled.
    expect(root).toHaveClass('wordmark', 'app-header__brand');
  });
});
