/**
 * The parity oracle's read/write half, for this package's fixture tests.
 *
 * `fixtures/` at the repository root is the language-neutral record of what
 * the TypeScript implementation does, and the Rust port asserts equality
 * against it. Each test here regenerates its file when
 * `GHOSTAI_UPDATE_FIXTURES=1` is set and otherwise asserts that the
 * implementation still produces the committed bytes — the *bytes*, not a
 * parsed equivalent, because byte-equality is what the Rust side checks.
 *
 * Reached through `import.meta.url` rather than an import: `no-restricted-
 * imports` bans `../../*`, and a `package.json` `imports` entry cannot point
 * outside the package. The same file exists in every package that owns a
 * fixture, deliberately — a shared copy would have to live in one of them and
 * be imported across the boundary the rule exists to keep.
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect } from 'vitest';

export const UPDATE_FIXTURES: boolean =
  process.env.GHOSTAI_UPDATE_FIXTURES === '1';

/** Absolute path of a file under the repository's `fixtures/` directory. */
export function fixturePath(relative: string): string {
  return fileURLToPath(
    new URL(`../../../fixtures/${relative}`, import.meta.url),
  );
}

/**
 * Regenerates the fixture under the environment variable, and otherwise
 * asserts the committed text is exactly what the implementation produced.
 */
export function checkFixture(relative: string, produced: string): void {
  const file = fixturePath(relative);
  if (UPDATE_FIXTURES) {
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, produced);
    return;
  }
  if (!existsSync(file)) {
    throw new Error(
      `Missing fixture ${relative}; run the suite once with GHOSTAI_UPDATE_FIXTURES=1 to generate it`,
    );
  }
  expect(readFileSync(file, 'utf8')).toBe(produced);
}

/** `checkFixture` over the canonical JSON form: two spaces, trailing newline. */
export function checkJsonFixture(relative: string, value: unknown): void {
  checkFixture(relative, `${JSON.stringify(value, null, 2)}\n`);
}

/** A fixture file's committed text, for tests that read one rather than write it. */
export function readFixture(relative: string): string {
  return readFileSync(fixturePath(relative), 'utf8');
}
