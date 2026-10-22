import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { brotliCompressSync, constants, gzipSync } from 'node:zlib';

import react from '@vitejs/plugin-react';
import { defineConfig, type Plugin } from 'vite';

/** Below this, the headers cost more than the bytes saved. */
const COMPRESS_MIN_BYTES = 1024;

/**
 * Writes `.br` and `.gz` beside every hashed text asset, once, at build time.
 *
 * The server picks one by `Accept-Encoding` instead of compressing per request,
 * which on a small board would spend more CPU than the transfer saves. Both,
 * because a browser offers Brotli only over HTTPS, and a LAN install is usually
 * plain HTTP. Only `assets/`: the server gives those an immutable cache header,
 * and the few files outside it are small and revalidated.
 */
function precompress(): Plugin {
  return {
    name: 'darkwire:precompress',
    apply: 'build',
    async writeBundle(options, bundle) {
      const dir = options.dir ?? 'dist';
      for (const name of Object.keys(bundle)) {
        if (!/^assets\/.+\.(?:js|css|svg|json)$/.test(name)) continue;
        const path = join(dir, name);
        const source = await readFile(path);
        if (source.length < COMPRESS_MIN_BYTES) continue;
        await writeFile(
          `${path}.br`,
          brotliCompressSync(source, {
            params: {
              [constants.BROTLI_PARAM_QUALITY]: constants.BROTLI_MAX_QUALITY,
              [constants.BROTLI_PARAM_SIZE_HINT]: source.length,
            },
          }),
        );
        await writeFile(`${path}.gz`, gzipSync(source, { level: 9 }));
      }
    },
  };
}

/**
 * No CSS framework and no PostCSS pipeline. `src/styles/app.css` is plain CSS
 * with `@import` and `@layer`, which Vite inlines and minifies on its own —
 * the styling layer needs no build step of its own, which is one fewer thing
 * between what is written and what ships.
 *
 * The build is entirely self-contained by requirement, not by accident. Fonts
 * are npm packages emitted into `dist/assets`, nothing fetches from a CDN, and
 * `darkwire serve` mounts this directory as static files behind the same origin as
 * the API — so the UI renders with the network otherwise blocked, which is what
 * a self-hosted air-gapped install actually needs.
 */
export default defineConfig({
  plugins: [react(), precompress()],
  // `@/` for anything outside the importing file's own directory — see
  // `tsconfig.json` for why the alias exists rather than a deeper relative path.
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    sourcemap: true,
    // A hashed filename is what makes the server's immutable cache header on
    // `assets/` safe.
    assetsInlineLimit: 4096,
  },
  server: {
    port: 5173,
    // `darkwire serve` in the other terminal, on the config's default port. Only the dev server
    // proxies: the built app is served from the API's own origin.
    proxy: {
      '/api': 'http://127.0.0.1:3000',
      '/ws': { target: 'ws://127.0.0.1:3000', ws: true },
    },
  },
});
