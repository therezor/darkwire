/**
 * The bundles, imported so that the JSON is also the type.
 *
 * JSON rather than a TypeScript object literal for one reason that outranks the
 * convenience of comments: `i18next-parser` writes these files and a translation
 * service reads them. A `.ts` catalogue would mean hand-maintaining the output
 * of every extraction run, which is the chore the library was chosen to avoid.
 *
 * **Imported by package self-reference, not by relative path, and that detail is
 * load-bearing.** `tsc` copies the specifier into `dist/resources.d.ts`
 * verbatim, and it does not copy JSON — it compiles TypeScript. A relative
 * `./locales/en/web.json` therefore points, from inside `dist`, at a file
 * that was never emitted there. The failure is silent in the worst way:
 * `skipLibCheck` swallows the unresolved import, `WebResources` degrades to
 * `any`, every key type widens to `string`, and every consumer goes on
 * compiling with nothing checked — which is the entire guarantee this package
 * exists to provide.
 *
 * `@ghostwire/i18n/locales/…` resolves through this package's own `exports` map
 * instead, which reads the same from `src`, from `dist`, and from a consumer
 * three packages away. A copy step would have fixed the built output and *not*
 * `pnpm typecheck`, which emits declarations without ever running the bundler.
 *
 * `with { type: 'json' }` is required rather than decorative — Node refuses a
 * JSON import in an ES module without it.
 */

import cli from '@ghostwire/i18n/locales/en/cli.json' with { type: 'json' };
import web from '@ghostwire/i18n/locales/en/web.json' with { type: 'json' };

export const EN = { web, cli } as const;

export type WebResources = typeof web;
export type CliResources = typeof cli;
