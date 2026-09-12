/**
 * Syntax highlighting, against the real engine.
 *
 * Mocking Shiki here would test the mock. What is actually worth knowing is
 * whether the fine-grained bundle is wired correctly — that the JavaScript
 * regex engine loads without WASM, that a grammar arrives through its own
 * dynamic import, and that an alias resolves to the same grammar rather than a
 * second copy of it. All three are the kind of thing that works in the test and
 * fails in the browser if it is faked.
 *
 * The theme is asserted only through the token colours differing between dark
 * and light, because pinning a hex would be pinning a decision that belongs to
 * whoever picked the theme.
 */

import { afterEach, describe, expect, it } from 'vitest';

import {
  LANGUAGES,
  highlight,
  resetHighlighter,
  resolveLanguage,
} from '@/chat/markdown/highlight.js';

afterEach(() => {
  resetHighlighter();
});

describe('the grammar table', () => {
  it('resolves every entry, so a typo is a failing test and not a dead fence', async () => {
    // A mistyped module path is neither a type error nor a build error: it is a
    // rejected `import()` the first time someone writes that fence. This is the
    // only place it can be caught early.
    const loaded = await Promise.all(
      Object.entries(LANGUAGES).map(
        async ([name, load]) => [name, await load()] as const,
      ),
    );

    for (const [name, grammar] of loaded) {
      const registration = (grammar as { default?: unknown }).default;
      expect(
        Array.isArray(registration),
        `${name} did not export a grammar`,
      ).toBe(true);
      expect((registration as unknown[]).length).toBeGreaterThan(0);
    }
  });

  it('points every alias at a grammar that is actually carried', () => {
    const aliases = [
      'ts',
      'js',
      'jsx',
      'sh',
      'zsh',
      'py',
      'rb',
      'rs',
      'yml',
      'md',
      'tf',
    ];

    for (const alias of aliases) {
      const resolved = resolveLanguage(alias);
      expect(resolved, `${alias} resolved to nothing`).toBeDefined();
      expect(Object.keys(LANGUAGES)).toContain(resolved);
    }
  });
});

describe('resolveLanguage', () => {
  it('maps the names a fence actually carries onto the grammars we ship', () => {
    expect(resolveLanguage('ts')).toBe('typescript');
    expect(resolveLanguage('sh')).toBe('bash');
    expect(resolveLanguage('zsh')).toBe('bash');
    expect(resolveLanguage('c++')).toBe('cpp');
    expect(resolveLanguage('dockerfile')).toBe('docker');
  });

  it('is undefined for a fence with no language, or one we do not carry', () => {
    expect(resolveLanguage('')).toBeUndefined();
    expect(resolveLanguage('brainfuck')).toBeUndefined();
  });
});

describe('highlight', () => {
  it('tokenises, and reassembles into exactly the source it was given', async () => {
    const code = 'const answer = 42;\nexport { answer };';
    const lines = await highlight(code, 'ts', 'dark');

    expect(lines).toBeDefined();
    expect(lines).toHaveLength(2);

    // A highlighter that drops or duplicates a character is a highlighter that
    // silently corrupts the code the user is about to copy.
    const rebuilt = lines
      ?.map((line) => line.map((token) => token.content).join(''))
      .join('\n');
    expect(rebuilt).toBe(code);
  });

  it('gives the keyword a colour, and a different one per theme', async () => {
    const dark = await highlight('const a = 1;', 'ts', 'dark');
    const light = await highlight('const a = 1;', 'ts', 'light');

    const first = (lines: typeof dark): string | undefined =>
      lines?.[0]?.[0]?.color;
    expect(first(dark)).toBeDefined();
    expect(first(light)).toBeDefined();
    expect(first(dark)).not.toBe(first(light));
  });

  it('returns undefined for a language it does not carry', async () => {
    // An unhighlighted code block is a perfectly good code block, and a fence
    // saying `brainfuck` is not an error condition.
    await expect(
      highlight('+++', 'brainfuck', 'dark'),
    ).resolves.toBeUndefined();
    await expect(highlight('x', '', 'dark')).resolves.toBeUndefined();
  });

  it('answers the same question from the cache', async () => {
    const first = await highlight('const a = 1;', 'typescript', 'dark');
    const second = await highlight('const a = 1;', 'typescript', 'dark');

    // Same array, not an equal one: highlighting is pure over (code, language,
    // theme), so a second pass over an unchanged block is pure waste.
    expect(second).toBe(first);
  });

  it('shares one grammar between a name and its aliases', async () => {
    const viaAlias = await highlight('const a = 1;', 'ts', 'dark');
    const viaName = await highlight('const a = 1;', 'typescript', 'dark');

    expect(viaAlias).toBe(viaName);
  });
}, 30_000);
