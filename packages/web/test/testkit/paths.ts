/**
 * Where the package's own files are, resolved from *this* module rather than
 * from whichever test is asking.
 *
 * The tests that read files off disk — the stylesheet properties, the recipe
 * class assertions, the `index.html` pre-paint script — used to count `../`
 * hops from their own directory, which made every one of those paths a
 * function of where the test file happened to sit. That is a bad thing to
 * depend on, because a wrong count does not fail: `walk()` over a directory
 * that does not exist throws, but a directory that exists and holds nothing
 * relevant yields an empty file list, and a suite that asserts a property of
 * every rule in an empty set passes while checking nothing.
 */

import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));

/** `packages/web` — where `index.html` and `package.json` sit. */
export const PACKAGE_ROOT: string = join(HERE, '..', '..');

/** `packages/web/src` — the source tree under test. */
export const SRC: string = join(PACKAGE_ROOT, 'src');

/** `packages/web/src/styles` — the stylesheet layer. */
export const STYLES: string = join(SRC, 'styles');
