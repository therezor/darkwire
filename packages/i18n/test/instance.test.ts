import { describe, expect, it } from 'vitest';

import { createI18n } from '#src/instance.js';
import { createWebI18n } from '#src/web.js';

const RESOURCES = {
  en: {
    web: {
      greeting: 'Hello {{name}}',
      files_one: '{{count}} file',
      files_other: '{{count}} files',
      blank: '',
      settings: { title: 'Settings' },
    },
    cli: { boom: 'It broke' },
  },
  de: {
    web: { greeting: 'Hallo {{name}}' },
    cli: {},
  },
};

/**
 * The fixture bundle above is deliberately *not* the product's, so its keys are
 * not in the `CustomTypeOptions` union and `t` has to be addressed untyped here.
 * That is the augmentation working rather than a hole in it — the tests further
 * down use the real bundles and stay fully checked.
 */
type LooseT = (key: string, options?: Record<string, unknown>) => string;

function build(locale = 'en', strict = false): LooseT {
  const instance = createI18n({
    locale,
    resources: RESOURCES,
    defaultNS: 'web',
    strict,
  });
  return instance.t as unknown as LooseT;
}

describe('the instance', () => {
  it('translates on the line after init, without awaiting anything', () => {
    // `initAsync: false` is what buys this. Left on, i18next loads inside a
    // setTimeout and this returns the key — and `ghostai --help` has no await to
    // hang on before printing.
    const t = build();

    expect(t('settings.title')).toBe('Settings');
  });

  it('interpolates without HTML-escaping the value', () => {
    // The default escapes, which would render a workspace called `Tom & Jerry`
    // as `Tom &amp; Jerry` — wrong in React, which escapes again, and simply
    // wrong in a terminal.
    const t = build();

    expect(t('greeting', { name: 'Tom & Jerry' })).toBe('Hello Tom & Jerry');
  });

  it('pluralises through Intl.PluralRules', () => {
    const t = build();

    expect(t('files', { count: 1 })).toBe('1 file');
    expect(t('files', { count: 4 })).toBe('4 files');
    expect(t('files', { count: 0 })).toBe('0 files');
  });

  it('addresses another namespace by prefix', () => {
    const t = build();

    expect(t('cli:boom')).toBe('It broke');
  });

  it('falls back to English for a key the locale has not translated', () => {
    const t = build('de');

    expect(t('greeting', { name: 'Ada' })).toBe('Hallo Ada');
    // Only in the English bundle — this is the half-translated case.
    expect(t('settings.title')).toBe('Settings');
  });

  it('falls back for a key that is present but empty', () => {
    // A blank translation is one nobody has finished, and an empty label reads
    // as a broken UI rather than an untranslated one.
    const t = build('en');

    expect(t('blank')).not.toBe('');
  });
});

describe('strict mode', () => {
  it('throws on a missing key, so a typo fails the suite', () => {
    const t = build('en', true);

    expect(() => t('nope.not.a.key')).toThrow(
      /Missing translation: web:nope\.not\.a\.key/u,
    );
  });

  it('returns the key instead of throwing when it is off', () => {
    // Production behaviour: a missing string is not worth a white screen.
    const t = build('en', false);

    expect(t('nope.not.a.key')).toBe('nope.not.a.key');
  });
});

describe('the browser instance', () => {
  it('gets its own bundle', () => {
    const i18n = createWebI18n('en', false);

    expect(i18n.t('settings.title')).toBe('Settings');
  });

  it('never ships the terminal’s strings', () => {
    // The one surviving per-surface instance, and the split still earns the
    // assertion: `cli` is the Rust binary's bundle, embedded there with
    // `include_str!`, and a browser that loaded it would be shipping copy no
    // screen can render.
    const web = createWebI18n('en', false);

    expect(web.hasResourceBundle('en', 'cli')).toBe(false);
  });
});
