import { describe, expect, it } from 'vitest';

import {
  binaryName,
  formatPattern,
  parsePattern,
  patternMatches,
  suggestPatterns,
} from '@/lib/exec-pattern.js';

describe('parsePattern', () => {
  it('splits on whitespace and honours quotes and escapes', () => {
    expect(parsePattern('  cargo   test *  ')).toEqual({
      ok: true,
      argv: ['cargo', 'test', '*'],
    });
    expect(parsePattern(`git commit -m "a b" 'c "d"' e\\ f ""`)).toEqual({
      ok: true,
      argv: ['git', 'commit', '-m', 'a b', 'c "d"', 'e f', ''],
    });
    expect(parsePattern('say "a \\"quoted\\" word"')).toEqual({
      ok: true,
      argv: ['say', 'a "quoted" word'],
    });
  });

  it('refuses what the server would', () => {
    expect(parsePattern('   ')).toEqual({ ok: false, error: 'empty' });
    expect(parsePattern('echo "open')).toEqual({
      ok: false,
      error: 'unclosed',
    });
    expect(parsePattern('cargo * test')).toEqual({ ok: false, error: 'star' });
  });

  it('round-trips through formatPattern', () => {
    for (const argv of [
      ['cargo', 'test', '*'],
      ['git', 'commit', '-m', 'a b'],
      ['echo', '', 'say "hi"', 'back\\slash'],
    ]) {
      expect(parsePattern(formatPattern(argv))).toEqual({ ok: true, argv });
    }
  });
});

describe('patternMatches', () => {
  it('matches the program by basename and a final star as the rest', () => {
    expect(patternMatches(['git', 'status'], ['/usr/bin/git', 'status'])).toBe(
      true,
    );
    expect(patternMatches(['git'], ['git.exe'])).toBe(true);
    expect(patternMatches(['cargo', 'test', '*'], ['cargo', 'test'])).toBe(
      true,
    );
    expect(
      patternMatches(['cargo', 'test', '*'], ['cargo', 'test', '-p']),
    ).toBe(true);
    expect(patternMatches(['cargo', 'test', '*'], ['cargo'])).toBe(false);
    expect(patternMatches(['cargo', 'test'], ['cargo', 'test', '-p'])).toBe(
      false,
    );
    expect(patternMatches(['cargo', 'test'], ['cargo', 'build'])).toBe(false);
    expect(patternMatches(['*'], ['anything'])).toBe(true);
  });
});

describe('suggestPatterns', () => {
  it('offers the exact command, then shorter prefixes with a star', () => {
    expect(
      suggestPatterns(['/usr/bin/cargo', 'test', '--release', '-p', 'x']).map(
        formatPattern,
      ),
    ).toEqual([
      'cargo test --release -p x',
      'cargo test --release *',
      'cargo test *',
      'cargo *',
    ]);
    expect(suggestPatterns(['ls']).map(formatPattern)).toEqual(['ls', 'ls *']);
    expect(suggestPatterns([])).toEqual([]);
  });

  it('names the program as the server does', () => {
    expect(binaryName('C:\\Tools\\git.EXE')).toBe('git');
    expect(binaryName('.exe')).toBe('.exe');
  });
});
