/**
 * Which language, out of the ones that exist.
 *
 * Locale negotiation is three lines of matching and one decision, and the
 * decision is that a request is never refused. A browser sending `de-AT`, a
 * shell exporting `LANG=de_DE.UTF-8` and a `config.json` written by hand all
 * name a language in a slightly different dialect of the same standard, and
 * every one of them has to land somewhere renderable — a thrown error here
 * would be a blank page over a spelling difference.
 *
 * So the chain narrows rather than fails: `de-AT` → `de` → `en`. The last step
 * is `DEFAULT_LOCALE` and it always matches, which is what makes the return
 * type `Locale` rather than `Locale | undefined` and spares every caller a
 * branch it would have written the same way.
 */

/** A BCP-47 tag the product ships strings for. */
export type Locale = string;

/** The one locale that is always present, and the end of every fallback chain. */
export const DEFAULT_LOCALE = 'en';

/**
 * Everything with a resource bundle.
 *
 * English only today. A second entry here plus a `locales/<tag>/` directory is
 * the whole of adding a language — the negotiation below needs no changes,
 * because it matches against this list rather than against a hardcoded set.
 */
export const SUPPORTED_LOCALES: readonly Locale[] = [DEFAULT_LOCALE];

/**
 * Normalises the shapes a locale arrives in.
 *
 * POSIX environments say `de_DE.UTF-8` — an underscore, and a codeset suffix
 * that is about bytes rather than language. `LANG=C` and `LANG=POSIX` are not
 * languages at all; they mean "no localisation", which is `en` here rather than
 * a lookup failure. Everything else is lowercased so `DE-de` and `de-DE` are
 * one key rather than two.
 */
export function normaliseLocale(raw: string | undefined): string {
  if (raw === undefined) return '';

  // Strip the codeset (`.UTF-8`) and the modifier (`@euro`); neither names a
  // language, and both are common in `LANG`.
  const bare = raw.split('.')[0]?.split('@')[0] ?? '';
  const tag = bare.replace(/_/gu, '-').trim().toLowerCase();

  return tag === 'c' || tag === 'posix' ? DEFAULT_LOCALE : tag;
}

/**
 * The best available match, or `undefined` when there is genuinely none.
 *
 * The distinction between "matched `en`" and "matched nothing, so `en`" is the
 * whole reason this returns `undefined` rather than the default: a preference
 * order needs to know that a source had nothing to say so it can ask the next
 * one. `resolveLocale` collapses that back down for the callers who only want
 * an answer.
 *
 * `available` is a parameter rather than a read of `SUPPORTED_LOCALES` so a
 * test can prove the negotiation without the product having to ship a second
 * language for it to be provable.
 */
export function matchLocale(
  requested: string | undefined,
  available: readonly Locale[] = SUPPORTED_LOCALES,
): Locale | undefined {
  const tag = normaliseLocale(requested);
  if (tag === '') return undefined;

  // `de-AT` before `de`: the most specific bundle that exists should win, and
  // walking the prefixes longest-first is what makes that true without ranking.
  const parts = tag.split('-');
  for (let length = parts.length; length > 0; length -= 1) {
    const prefix = parts.slice(0, length).join('-');
    const hit = available.find((locale) => locale.toLowerCase() === prefix);
    if (hit !== undefined) return hit;
  }

  return undefined;
}

/** The best available match for what was asked for, falling back rather than failing. */
export function resolveLocale(
  requested: string | undefined,
  available: readonly Locale[] = SUPPORTED_LOCALES,
): Locale {
  return matchLocale(requested, available) ?? DEFAULT_LOCALE;
}

/**
 * The first request that matches something, or the default when none do.
 *
 * This is the shape every consumer's preference order actually has — the CLI
 * asks `GHOSTAI_LANG`, then the config, then `LANG`; the web asks storage, then
 * the browser. Expressing it once keeps "first one that means anything wins"
 * from being re-implemented three times with three different opinions about
 * what "anything" is.
 *
 * A source naming a language nobody has translated is skipped rather than
 * treated as an answer, so `LANG=xx-YY` cannot shadow a perfectly good config
 * value by resolving to the default ahead of it.
 */
export function resolveFirstLocale(
  requested: ReadonlyArray<string | undefined>,
  available: readonly Locale[] = SUPPORTED_LOCALES,
): Locale {
  for (const candidate of requested) {
    const matched = matchLocale(candidate, available);
    if (matched !== undefined) return matched;
  }

  return DEFAULT_LOCALE;
}

/** Whether this locale is written right-to-left, for the `dir` attribute. */
export function isRtl(locale: Locale): boolean {
  const language = normaliseLocale(locale).split('-')[0] ?? '';
  return RTL_LANGUAGES.has(language);
}

/**
 * The right-to-left languages, by ISO 639-1 code.
 *
 * A list rather than `Intl.Locale.prototype.getTextInfo`, which is still
 * unevenly implemented and which returns `ltr` on the engines that do not have
 * it — a wrong answer being worse here than a maintained list of six.
 */
const RTL_LANGUAGES: ReadonlySet<string> = new Set([
  'ar',
  'fa',
  'he',
  'ps',
  'ur',
  'yi',
]);
