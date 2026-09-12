/**
 * What a link in model output is allowed to be.
 *
 * These are the two rules that keep untrusted markdown from being a delivery
 * mechanism: a scheme allowlist, and same-origin images. Both are one-liners
 * that are easy to relax by accident, and neither fails visibly when it is —
 * `javascript:` in an `<a>` looks like an ordinary link right up until it is
 * clicked.
 */

import { describe, expect, it } from 'vitest';

import { isSameOrigin, safeHref } from '@/lib/url.js';

const BASE = 'https://ghost.example/app/';

describe('safeHref', () => {
  it('allows the three schemes a document legitimately links with', () => {
    expect(safeHref('https://example.com/a', BASE)).toBe(
      'https://example.com/a',
    );
    expect(safeHref('http://example.com', BASE)).toBe('http://example.com/');
    expect(safeHref('mailto:a@b.c', BASE)).toBe('mailto:a@b.c');
  });

  it('resolves a relative link against the page', () => {
    expect(safeHref('./b', BASE)).toBe('https://ghost.example/app/b');
  });

  it('refuses the schemes that execute', () => {
    // The obvious one, and the one people forget: `data:text/html` in an `<a>`
    // is a same-origin document in every browser that still honours it.
    expect(safeHref('javascript:alert(1)', BASE)).toBeUndefined();
    expect(
      safeHref('data:text/html,<script>alert(1)</script>', BASE),
    ).toBeUndefined();
    expect(safeHref('vbscript:msgbox(1)', BASE)).toBeUndefined();
    expect(safeHref('file:///etc/passwd', BASE)).toBeUndefined();
  });

  it('is not fooled by whitespace or case', () => {
    expect(safeHref('  JavaScript:alert(1)  ', BASE)).toBeUndefined();
    expect(safeHref('\njavascript:alert(1)', BASE)).toBeUndefined();
  });

  it('refuses what is not a URL at all', () => {
    expect(safeHref('', BASE)).toBeUndefined();
    expect(safeHref('   ', BASE)).toBeUndefined();
  });
});

describe('isSameOrigin', () => {
  it('is true for the origin serving the page and false for anywhere else', () => {
    expect(isSameOrigin('/api/media/abc', BASE)).toBe(true);
    expect(isSameOrigin('https://ghost.example/x', BASE)).toBe(true);

    // An `<img>` at a third party tells whoever wrote the markdown — which, in
    // a tool result, is a web page — the reader's IP and when they read it.
    expect(isSameOrigin('https://tracker.example/pixel.gif', BASE)).toBe(false);
    expect(isSameOrigin('http://ghost.example/x', BASE)).toBe(false);
  });

  it('is false for something that is not a URL', () => {
    expect(isSameOrigin('', BASE)).toBe(false);
  });
});
