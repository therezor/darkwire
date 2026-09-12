import { describe, expect, it } from 'vitest';

import {
  DEFAULT_LOCALE,
  SUPPORTED_LOCALES,
  isRtl,
  matchLocale,
  normaliseLocale,
  resolveFirstLocale,
  resolveLocale,
} from '#src/locale.js';

// A second and third language so the negotiation can be proved without the
// product having to ship one. `de-AT` is here because the interesting case is a
// regional bundle sitting beside its base.
const AVAILABLE = ['en', 'de', 'de-AT', 'ar'];

describe('normaliseLocale', () => {
  it('accepts the shapes a locale actually arrives in', () => {
    expect(normaliseLocale('de-DE')).toBe('de-de');
    // POSIX: an underscore, and a codeset that is about bytes not language.
    expect(normaliseLocale('de_DE.UTF-8')).toBe('de-de');
    expect(normaliseLocale('de_DE@euro')).toBe('de-de');
    expect(normaliseLocale('  DE-de  ')).toBe('de-de');
  });

  it('reads the POSIX no-localisation locales as English', () => {
    // `LANG=C` does not mean "the C language"; it means "do not localise",
    // which is this product's default rather than a lookup failure.
    expect(normaliseLocale('C')).toBe(DEFAULT_LOCALE);
    expect(normaliseLocale('POSIX')).toBe(DEFAULT_LOCALE);
    expect(normaliseLocale('C.UTF-8')).toBe(DEFAULT_LOCALE);
  });

  it('is empty for nothing, rather than throwing', () => {
    expect(normaliseLocale(undefined)).toBe('');
    expect(normaliseLocale('')).toBe('');
  });
});

describe('matchLocale', () => {
  it('prefers the most specific bundle that exists', () => {
    expect(matchLocale('de-AT', AVAILABLE)).toBe('de-AT');
  });

  it('narrows to the base language when the region has no bundle', () => {
    expect(matchLocale('de-CH', AVAILABLE)).toBe('de');
  });

  it('reports no match rather than quietly answering English', () => {
    // The distinction this function exists for: a source that said nothing
    // must be distinguishable from one that asked for English.
    expect(matchLocale('ja', AVAILABLE)).toBeUndefined();
    expect(matchLocale(undefined, AVAILABLE)).toBeUndefined();
    expect(matchLocale('en', AVAILABLE)).toBe('en');
  });

  it('matches case-insensitively without changing the bundle name', () => {
    expect(matchLocale('DE-at', AVAILABLE)).toBe('de-AT');
  });
});

describe('resolveLocale', () => {
  it('falls back rather than failing, so a bad tag is never a blank page', () => {
    expect(resolveLocale('ja', AVAILABLE)).toBe(DEFAULT_LOCALE);
    expect(resolveLocale(undefined, AVAILABLE)).toBe(DEFAULT_LOCALE);
    expect(resolveLocale('not a locale at all', AVAILABLE)).toBe(
      DEFAULT_LOCALE,
    );
  });

  it('defaults to what the product actually ships', () => {
    expect(resolveLocale('en')).toBe(DEFAULT_LOCALE);
    expect(SUPPORTED_LOCALES).toContain(DEFAULT_LOCALE);
  });
});

describe('resolveFirstLocale', () => {
  it('takes the first source that names a language we have', () => {
    expect(resolveFirstLocale([undefined, 'de-AT', 'ar'], AVAILABLE)).toBe(
      'de-AT',
    );
  });

  it('skips a source naming a language nobody has translated', () => {
    // The bug this prevents: `LANG=ja_JP.UTF-8` resolving to `en` and shadowing
    // a perfectly good config value that comes after it.
    expect(resolveFirstLocale(['ja', 'de'], AVAILABLE)).toBe('de');
  });

  it('falls back when no source says anything usable', () => {
    expect(resolveFirstLocale([undefined, '', 'ja'], AVAILABLE)).toBe(
      DEFAULT_LOCALE,
    );
    expect(resolveFirstLocale([], AVAILABLE)).toBe(DEFAULT_LOCALE);
  });
});

describe('isRtl', () => {
  it('knows the right-to-left languages, region and case regardless', () => {
    expect(isRtl('ar')).toBe(true);
    expect(isRtl('ar-EG')).toBe(true);
    expect(isRtl('HE')).toBe(true);
    expect(isRtl('en')).toBe(false);
    expect(isRtl('de-AT')).toBe(false);
  });
});
