/**
 * A resource bundle's keys, as a union of dotted paths.
 *
 * This is what makes `t('settings.panels.tools.label')` a compile error when it
 * is misspelled, and it is why the resources are imported rather than merely
 * shipped: the JSON *is* the type. It is also what `GhostError.messageKey` is
 * checked against, so a package with no runtime dependency on i18next still
 * cannot name a string that does not exist.
 *
 * Plural keys are a deliberate wrinkle. i18next resolves `files_one` /
 * `files_other` from a base name of `files` that appears nowhere in the JSON,
 * so the suffixes are stripped here — otherwise the one call site that pluralises
 * would be the one that fails to type-check.
 */

/** Strips i18next's CLDR plural suffixes back to the base key. */
type WithoutPluralSuffix<K extends string> =
  K extends `${infer Base}_${infer Suffix}`
    ? Suffix extends 'zero' | 'one' | 'two' | 'few' | 'many' | 'other'
      ? Base
      : K
    : K;

/**
 * Every leaf path in a nested resource object.
 *
 * Recursion is bounded by the object's own depth, which is three or four here —
 * far short of anything TypeScript would complain about.
 */
export type ResourceKeys<T> = T extends string
  ? never
  : {
      [K in keyof T & string]: T[K] extends string
        ? WithoutPluralSuffix<K>
        : `${K}.${ResourceKeys<T[K]>}`;
    }[keyof T & string];
