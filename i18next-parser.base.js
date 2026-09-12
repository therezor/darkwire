/**
 * Extraction: every `t()` call in the source, back into the English bundles.
 *
 * This is one of two gates and it catches only one direction. The parser finds
 * keys that are *used*; it cannot find copy that was never wrapped in `t()` at
 * all — a hardcoded string is invisible to it, because there is nothing to
 * parse. The source sweep in `packages/web/test/i18n/untranslated.test.ts` is the
 * other direction, and neither substitutes for the other.
 *
 * What it is really for is translators. `pnpm i18n:extract` regenerates the
 * files a TMS ingests, and `pnpm i18n:check` fails when the committed bundles
 * are stale — the same shape as `format:check`, and the reason i18next was
 * chosen over a hand-rolled catalogue.
 *
 * **One config per surface, because the namespace cannot be inferred.** The
 * parser cannot see a runtime `getFixedT(null, ns)` binding, so pointed at two
 * trees at once it would file every key under one namespace. `defaultNS` is set
 * per invocation and each run is given only its own sources. Only `web` is
 * extracted today, and the shape is kept because a second surface would need it
 * back unchanged.
 *
 * **`locales/en/cli.json` is hand-maintained and is not extracted.** There used
 * to be an `i18next-parser.cli.js` beside this file pointed at `packages/cli`;
 * the terminal is Rust now, and `crates/i18n` embeds that bundle with
 * `include_str!`, generates its typed key constants from it in `build.rs` and
 * asserts in `crates/i18n/tests` that every key resolves. So the file stays
 * where it is and is edited by hand — `pnpm i18n:check` covers the web half
 * alone, and the Rust build is what fails on a key the bundle does not carry.
 *
 * `keepRemoved` is on. A key the parser cannot see is not necessarily dead: the
 * tables in `settings/panels.ts` and `chat/notice.tsx` hold keys as *data* and
 * resolve them through a variable, which no static pass can follow. Off, every
 * extraction run would delete them and the next would restore only what a
 * literal call site named.
 */

export function config(defaultNamespace) {
  return {
    locales: ['en'],
    defaultNamespace,
    namespaceSeparator: ':',
    keySeparator: '.',

    // Written where the runtime reads them from, so an extraction run and a
    // `git diff` are the same question.
    output: 'packages/i18n/locales/$LOCALE/$NAMESPACE.json',

    // An untranslated key is written empty rather than as its own name.
    // `returnEmptyString: false` in `createI18n` makes that fall back to
    // English, and a key written as its own text would defeat the
    // `t(key) !== key` assertion several tests use to prove an entry exists.
    defaultValue: '',
    keepRemoved: true,
    sort: true,

    // Matches `createI18n`. The parser writes plural suffixes from this, so a
    // disagreement here produces keys the runtime never looks up.
    pluralSeparator: '_',
    contextSeparator: '_',

    failOnWarnings: false,
    verbose: false,
  };
}
