import { defineConfig } from 'tsup';

export default defineConfig({
  entry: ['src/index.ts', 'src/web.ts'],
  format: ['esm'],
  target: 'node22',
  // tsc -b writes its declarations and .tsbuildinfo into this same dist, so
  // cleaning here would delete them and leave tsc believing they still exist.
  clean: false,
  dts: false, // tsc -b emits declarations; rollup-plugin-dts is the slow path
  sourcemap: true,
  removeNodeProtocol: false,
});
