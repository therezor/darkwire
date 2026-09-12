/**
 * The gates as a command, so `pnpm --filter @ghostwire/web lint` fails the same
 * way the test suite does. Both entry points share `collectSources`, so there
 * is one answer to "which files are checked" rather than two that drift.
 */

import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { checkFiles, formatViolation, type SourceFile } from './gates.js';

const PACKAGE_ROOT: string = join(
  dirname(fileURLToPath(import.meta.url)),
  '..',
  '..',
);

const EXTENSIONS = ['.css', '.ts', '.tsx', '.html'];

/**
 * Everything that ends up in front of a user, and nothing else.
 *
 * `src/tokens/**` is excluded because it is the gate machinery: this file, the
 * patterns themselves and the colour maths all contain the literals they exist
 * to ban. Tests are excluded for the same reason — a test that asserts `13px`
 * is caught has to be able to write `13px`. Neither directory is reachable from
 * the bundle: nothing in `main.tsx`'s import graph leads here.
 */
export function collectSources(
  root: string = PACKAGE_ROOT,
): readonly SourceFile[] {
  const files = [join(root, 'index.html'), ...walk(join(root, 'src'))];

  return files
    .map((absolute) => relative(root, absolute).split(sep).join('/'))
    .filter((file) => !file.startsWith('src/tokens/'))
    .filter((file) => !/\.test\.[jt]sx?$/.test(file))
    .sort()
    .map((file) => ({ file, source: readFileSync(join(root, file), 'utf8') }));
}

function walk(directory: string): readonly string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return walk(path);
    return EXTENSIONS.some((extension) => entry.name.endsWith(extension))
      ? [path]
      : [];
  });
}

function main(): void {
  const violations = checkFiles(collectSources());
  if (violations.length === 0) {
    process.stdout.write('token gates: clean\n');
    return;
  }

  for (const violation of violations) {
    process.stderr.write(`${formatViolation(violation)}\n`);
  }
  process.stderr.write(
    `\n${violations.length.toString()} token gate violation(s)\n`,
  );
  process.exitCode = 1;
}

// `tsx` runs this as the entry module; importing it from the test does not.
if (
  process.argv[1] !== undefined &&
  pathToFileURL(process.argv[1]).href === import.meta.url
) {
  main();
}
