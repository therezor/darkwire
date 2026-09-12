/**
 * The conformance suite, run against this extension for real.
 *
 * This file is the point of the example as much as `index.mjs` is: it is one
 * assertion, and it is what an extension living in its own repository copies.
 * The suite itself is Rust — `ghostai-extension-host`'s testkit — because the
 * thing being checked is a *process boundary*, and only the host knows how to
 * spawn a child, complete the handshake, probe every kind the manifest
 * declares and take the child down again. Nothing about that is expressible
 * from inside the extension, which is exactly why the v1 suite could not catch
 * a manifest that disagreed with the code.
 *
 * It shells out rather than reimplementing any of it, and skips when `cargo` is
 * not installed: a JavaScript author editing `index.mjs` should not be required
 * to have a Rust toolchain, and CI has one.
 */

import { execFileSync, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

const REPO_ROOT = fileURLToPath(new URL('../../../', import.meta.url));
const EXTENSION_DIR = fileURLToPath(new URL('../', import.meta.url));

/** Whether a Rust toolchain is on this machine at all. */
function hasCargo(): boolean {
  const probe = spawnSync('cargo', ['--version'], { stdio: 'ignore' });
  return probe.status === 0;
}

describe('the hello extension', () => {
  it.skipIf(!hasCargo())(
    'passes the host conformance suite',
    () => {
      // Every declared kind has to answer its list method, every id has to be
      // namespaced, and the counts have to match what the manifest discloses.
      const output = execFileSync(
        'cargo',
        [
          'run',
          '--quiet',
          '-p',
          'ghostai-extension-host',
          '--example',
          'check',
          '--features',
          'testkit',
          '--',
          EXTENSION_DIR,
          '--tools',
          '1',
          '--commands',
          '1',
          '--context',
          '1',
        ],
        { cwd: REPO_ROOT, encoding: 'utf8' },
      );

      expect(output).toContain('hello: ready');
    },
    // A cold Cargo cache builds the workspace before it can run anything.
    600_000,
  );
});
