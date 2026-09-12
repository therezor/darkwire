import { fileURLToPath } from 'node:url';

import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

/**
 * No CSS framework and no PostCSS pipeline. `src/styles/app.css` is plain CSS
 * with `@import` and `@layer`, which Vite inlines and minifies on its own —
 * the styling layer needs no build step of its own, which is one fewer thing
 * between what is written and what ships.
 *
 * The build is entirely self-contained by requirement, not by accident. Fonts
 * are npm packages emitted into `dist/assets`, nothing fetches from a CDN, and
 * `ghostai serve` mounts this directory as static files behind the same origin as
 * the API — so the UI renders with the network otherwise blocked, which is what
 * a self-hosted air-gapped install actually needs.
 */
export default defineConfig({
  plugins: [react()],
  // `@/` for anything outside the importing file's own directory — see
  // `tsconfig.json` for why the alias exists rather than a deeper relative path.
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    sourcemap: true,
    // The server sets its own cache headers; a hashed filename is what makes
    // an aggressive one safe.
    assetsInlineLimit: 4096,
  },
  server: {
    port: 5173,
    // `ghostai serve` in the other terminal, on the config's default port. Only the dev server
    // proxies: the built app is served from the API's own origin.
    proxy: {
      '/api': 'http://127.0.0.1:3000',
      '/ws': { target: 'ws://127.0.0.1:3000', ws: true },
    },
  },
});
