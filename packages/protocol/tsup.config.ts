import { defineConfig } from 'tsup';

export default defineConfig({
  // One file out per file in, with `"sideEffects": false` in package.json, so
  // the browser's bundler can leave out a module nothing it imports reaches.
  // One bundled file kept every schema in the first download.
  entry: ['src/*.ts'],
  bundle: false,
  format: ['esm'],
  target: 'node22',
  // tsc -b writes its declarations and .tsbuildinfo into this same dist, so
  // cleaning here would delete them and leave tsc believing they still exist.
  // Removing dist by hand is what forces a full rebuild of both tools.
  clean: false,
  dts: false, // tsc -b emits declarations; rollup-plugin-dts is the slow path
  sourcemap: true,
  // tsup rewrites `node:sqlite` to `sqlite` otherwise — a compatibility shim for
  // node versions older than 14.18 that turns a builtin into a missing package.
  // `node:sqlite` has no unprefixed form at all, so the rewrite is unloadable.
  // The default flips to false in tsup 9; this is only saying so early.
  removeNodeProtocol: false,
});
