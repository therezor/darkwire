/**
 * What makes `t()` a checked call rather than a string lookup.
 *
 * i18next reads its key types out of one global interface, so this is declared
 * once here and inherited by every package that imports the layer. A misspelled
 * `t('settings.titel')` is a compile error in the web app, in the CLI and in a
 * `GhostError` thrown three packages away — which is the whole reason the
 * resources are imported rather than merely shipped.
 *
 * `defaultNS` is `web` because that is the surface with the most call sites.
 * The CLI works through a namespace-scoped `t` (`getFixedT(null, 'cli')`), so
 * its keys are checked against `cli` rather than against this default — and a
 * CLI file that reaches for the unscoped `t` by mistake fails to compile, which
 * is the outcome worth having.
 *
 * A `.ts` file rather than a `.d.ts`: `tsc -b` does not re-emit declarations
 * that are already declarations, so a `.d.ts` here would type-check locally and
 * ship nothing. This one is in `index.ts`'s import graph, so it lands in `dist`.
 */

import type { CliResources, WebResources } from './resources.js';

declare module 'i18next' {
  interface CustomTypeOptions {
    defaultNS: 'web';
    resources: {
      web: WebResources;
      cli: CliResources;
    };
  }
}

// A module augmentation needs the file to be a module, and an empty export is
// the honest way to say so — there is no value here to export.
export {};
