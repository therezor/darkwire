/**
 * The gates, both halves: that they catch what they exist to catch, and that
 * the package passes them.
 *
 * The second half is the one that matters day to day — it is what makes "no
 * `px`, no raw colour, no accent-as-text" a property of the codebase rather
 * than a paragraph in a document. The first half is what keeps the second
 * honest: a regex that stopped matching would otherwise report a clean sweep.
 */

import { describe, expect, it } from 'vitest';

import {
  checkFile,
  checkFiles,
  formatViolation,
  TOKEN_SHEET,
} from '@/tokens/gates.js';
import { collectSources } from '@/tokens/run-gates.js';

const rules = (file: string, source: string): readonly string[] =>
  checkFile({ file, source }).map((violation) => violation.rule);

describe('no-px', () => {
  it('catches a px literal in CSS', () => {
    expect(rules('src/x.css', '.a { padding: 12px; }')).toEqual(['no-px']);
  });

  it('catches a px literal in an inline style, which is the one place TSX can carry one', () => {
    expect(rules('src/x.tsx', "<div style={{ top: '1.5px' }} />")).toEqual([
      'no-px',
    ]);
  });

  it('allows rem, and allows px in the token sheet', () => {
    expect(rules('src/x.css', '.a { padding: 0.75rem; }')).toEqual([]);
    expect(rules(TOKEN_SHEET, ':root { --hairline: 1px; }')).toEqual([]);
  });

  it('does not fire on a word that merely ends in px', () => {
    expect(rules('src/x.ts', "const dropbox = '2px-free';")).toEqual([]);
  });
});

describe('no-raw-color', () => {
  it('catches hex in every length', () => {
    expect(rules('src/x.css', '.a { color: #fff; }')).toEqual(['no-raw-color']);
    expect(rules('src/x.css', '.a { color: #1a1a1a; }')).toEqual([
      'no-raw-color',
    ]);
    expect(
      rules('src/x.tsx', "<div style={{ color: '#0f0f0f80' }} />"),
    ).toEqual(['no-raw-color']);
  });

  it('catches every colour function', () => {
    for (const value of [
      'rgb(0 0 0)',
      'rgba(0,0,0,.5)',
      'hsl(0 0% 0%)',
      'oklch(0.2 0 0)',
    ]) {
      expect(rules('src/x.css', `.a { color: ${value}; }`)).toEqual([
        'no-raw-color',
      ]);
    }
  });

  it('allows a token reference, and allows raw colour in the token sheet', () => {
    expect(rules('src/x.css', '.a { color: var(--fg-1); }')).toEqual([]);
    expect(
      rules(TOKEN_SHEET, ':root { --shadow-xs: 0 0 0 rgb(0 0 0 / 0.18); }'),
    ).toEqual([]);
  });

  it('does not fire on a fragment identifier or an issue number', () => {
    expect(
      rules('src/x.tsx', '<a href="#top">top</a> {/* see #12 */}'),
    ).toEqual([]);
  });
});

describe('accent-position', () => {
  it('catches the fill token in a text or stroke declaration', () => {
    for (const property of [
      'color',
      'border-color',
      'outline',
      'fill',
      'stroke',
    ]) {
      expect(rules('src/x.css', `.a { ${property}: var(--accent); }`)).toEqual([
        'accent-position',
      ]);
    }
  });

  it('catches it inside a shorthand, where it is easiest to miss', () => {
    expect(
      rules('src/x.css', '.a { border: var(--hairline) solid var(--accent); }'),
    ).toEqual(['accent-position']);
  });

  it('allows the fill token as a fill', () => {
    expect(
      rules('src/x.css', '.a { background-color: var(--accent); }'),
    ).toEqual([]);
    expect(
      rules('src/x.css', '.a { background-color: var(--accent-soft); }'),
    ).toEqual([]);
  });

  it('allows the -fg token everywhere, which is the whole point', () => {
    expect(rules('src/x.css', '.a { color: var(--accent-fg); }')).toEqual([]);
    expect(
      rules('src/x.css', '.a { border-color: var(--accent-fg); }'),
    ).toEqual([]);
  });

  it('applies inside the token sheet too — it is a usage rule, not a literal rule', () => {
    expect(rules(TOKEN_SHEET, '.a { color: var(--accent); }')).toEqual([
      'accent-position',
    ]);
  });
});

describe('the package itself', () => {
  const sources = collectSources();

  it('checks the stylesheets, the components and index.html', () => {
    const files = sources.map((source) => source.file);

    expect(files).toContain('index.html');
    expect(files).toContain('src/styles/tokens.css');
    expect(files).toContain('src/styles/base.css');
    expect(files).toContain('src/routes/tokens.tsx');
    expect(files).toContain('src/components/ui/button.tsx');
    // The gate machinery is excluded: it contains every literal it bans.
    expect(files.filter((file) => file.startsWith('src/tokens/'))).toEqual([]);
  });

  it('passes all three gates', () => {
    const violations = checkFiles(sources).map(formatViolation);
    expect(violations).toEqual([]);
  });
});
