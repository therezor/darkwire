#!/usr/bin/env node
// Per-crate coverage gates for the Rust workspace.
//
// `cargo llvm-cov` has one `--fail-under-lines` for the whole workspace, and the
// bars here are per crate for the same reason `vitest.config.ts` sets them per
// package: an untested branch in `security` is a bypass, an untested branch in
// the CLI is a cosmetic bug, and one number cannot say both. Reads the JSON that
// `cargo llvm-cov ... --json --output-path <file>` writes and exits non-zero with
// a sentence per failing crate.
//
// Usage: node scripts/coverage-gate.mjs coverage.json
//
// The table mirrors the TypeScript one in vitest.config.ts. "Branches" here is
// llvm-cov's region coverage, the closest thing a stable toolchain measures.

import { readFileSync } from 'node:fs';

const DEFAULT = { lines: 70, branches: 65 };

/** @type {Record<string, {lines: number, branches: number}>} */
const THRESHOLDS = {
  // An untested branch in a guard is a vulnerability, not a bug.
  security: { lines: 95, branches: 95 },
  // Pure decision logic; a wrong branch is a silently wrong bundle, not a crash.
  i18n: { lines: 90, branches: 90 },
  // The spine everything else stands on, and the two toolkits with no deps.
  core: { lines: 90, branches: 85 },
  channels: { lines: 90, branches: 85 },
  tui: { lines: 90, branches: 85 },
  // The turn, the composition and the transport.
  agent: { lines: 85, branches: 80 },
  runtime: { lines: 85, branches: 80 },
  'extension-host': { lines: 85, branches: 80 },
  server: { lines: 85, branches: 80 },
  // The one crate held at the default on purpose. Its policy decisions are
  // covered like any other guard — the socket boundary, the egress refusals,
  // the proxy's header rules — but roughly a third of it only runs with a
  // container daemon behind it, and the test that supplies one is `#[ignore]`d
  // because CI has none. Raising this bar would mean either deleting that
  // third or asserting it against a mock of the daemon, which tests the mock.
  environment: { lines: 70, branches: 65 },
  // Wire adapters and process runners: much of the surface is I/O plumbing.
  providers: { lines: 80, branches: 75 },
  tools: { lines: 80, branches: 75 },
  mcp: { lines: 80, branches: 75 },
};

const file = process.argv[2];
if (!file) {
  console.error('usage: node scripts/coverage-gate.mjs <llvm-cov json>');
  process.exit(2);
}

const report = JSON.parse(readFileSync(file, 'utf8'));
const files = report.data.flatMap((d) => d.files ?? []);

/** crate name from a source path like `crates/security/src/jail.rs` */
function crateOf(path) {
  const m = /(?:^|\/)crates\/([^/]+)\/src\//.exec(path);
  return m ? m[1] : undefined;
}

const byCrate = new Map();
for (const f of files) {
  const crate = crateOf(f.filename);
  if (!crate) continue;
  // Testkits run on every test and would pad the crate that holds them.
  if (/\/src\/testkit(\.rs|\/)/.test(f.filename)) continue;
  // A binary's `main.rs` only maps a run to an exit code and is exercised by
  // the e2e suite, not by unit tests; the same reason vitest excludes `index.ts`.
  if (/\/src\/main\.rs$/.test(f.filename)) continue;
  const agg = byCrate.get(crate) ?? {
    lines: { covered: 0, count: 0 },
    regions: { covered: 0, count: 0 },
  };
  agg.lines.covered += f.summary.lines.covered;
  agg.lines.count += f.summary.lines.count;
  agg.regions.covered += f.summary.regions.covered;
  agg.regions.count += f.summary.regions.count;
  byCrate.set(crate, agg);
}

let failed = false;
const pct = (s) => (s.count === 0 ? 100 : (100 * s.covered) / s.count);
for (const [crate, agg] of [...byCrate.entries()].sort()) {
  const bar = THRESHOLDS[crate] ?? DEFAULT;
  const lines = pct(agg.lines);
  const branches = pct(agg.regions);
  const ok = lines >= bar.lines && branches >= bar.branches;
  const mark = ok ? 'ok  ' : 'FAIL';
  console.log(
    `${mark} ${crate.padEnd(16)} lines ${lines.toFixed(1).padStart(5)}% (>= ${bar.lines})  ` +
      `regions ${branches.toFixed(1).padStart(5)}% (>= ${bar.branches})`,
  );
  if (!ok) failed = true;
}

if (byCrate.size === 0) {
  console.log('no crate sources in the report; nothing to gate yet');
}
process.exit(failed ? 1 : 0);
