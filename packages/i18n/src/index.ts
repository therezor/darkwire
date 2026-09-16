/**
 * The shared half of the translation layer.
 *
 * Everything here is safe in a browser, in Node and in a package that has no
 * opinion about either: locale negotiation, the `Intl` primitives, the instance
 * factory, and the key type that lets a package name a string without depending
 * on i18next to resolve it.
 *
 * `@darkwire/i18n/web` is the browser's entry point, and it exists so the UI
 * never ships the terminal's strings. There is no matching `/cli` entry: the
 * terminal is Rust, and `crates/i18n` embeds `locales/en/cli.json` with
 * `include_str!` and generates a typed constant per key from the same file. The
 * `cli` bundle is still exported through `./locales/*`, which is what both sides
 * read, and still typed here — `CustomTypeOptions` declares both namespaces, so
 * a `WireError` naming a CLI key is still checked against the bundle.
 */

// Side-effect import: the `CustomTypeOptions` augmentation that types `t()`.
// Everything downstream inherits it by importing this package at all.
import './types.js';

export {
  DEFAULT_LOCALE,
  SUPPORTED_LOCALES,
  isRtl,
  matchLocale,
  normaliseLocale,
  resolveFirstLocale,
  resolveLocale,
  type Locale,
} from './locale.js';

export {
  durationParts,
  formatCompactNumber,
  formatDate,
  formatDateTime,
  formatNumber,
  formatRelativeSpan,
  pluralCategory,
  relativeSpan,
  type RelativeSpan,
} from './format.js';

export {
  instantFromZonedInput,
  isValidTimeZone,
  zonedInputValue,
} from './zoned-time.js';

export { createI18n, type Namespace } from './instance.js';

export { EN, type CliResources, type WebResources } from './resources.js';

export type { ResourceKeys } from './keys.js';
