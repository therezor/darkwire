/**
 * The product being replaced, served from disk.
 *
 * It is a static document — one `index.html` with every panel already in it,
 * a folder of hand-written CSS, and scripts that talk to a backend that is not
 * running here. That is what makes it usable as a reference at all: the markup
 * and the stylesheet are the whole of the appearance, and neither needs the
 * server the scripts are looking for.
 *
 * Two things are done to it, and both are subtractions:
 *
 *  - **The scripts are not served.** They would run against a socket that will
 *    never connect, and their failure paths rewrite the DOM — an error banner
 *    here, an empty-state there. The reference has to be the same pixels every
 *    time it is measured, and the authored markup is; whatever a disconnected
 *    client happens to render is not.
 *  - **The webfont links are dropped**, unless a caller asks for them. The
 *    document opens with three stylesheets from a font CDN — a text family, a
 *    monospace one and an icon set. Leaving them in makes the reference depend
 *    on a network the rest of this suite is at pains to prove it does not need,
 *    and on whatever that CDN serves today.
 *
 * That last one is an option rather than a rule because the two consumers want
 * opposite answers. The gate measures lengths and colours, every one of which
 * is a fixed value in the reference's own stylesheet and none of which moves
 * when a font falls back — so it takes the hermetic form, and a machine with no
 * route to the internet measures what this one does. The capture script draws
 * a picture for a person to look at, and a picture of the reference rendered in
 * a system font with its icons spelled out as words is a worse likeness of it
 * than one with the fonts; it asks for them, and says so.
 *
 * Nothing is added, and no rule is rewritten.
 */

import { createReadStream, existsSync, readFileSync, statSync } from 'node:fs';
import { createServer, type Server } from 'node:http';
import { extname, join, normalize, resolve, sep } from 'node:path';
import type { AddressInfo } from 'node:net';

/**
 * Where the reference lives.
 *
 * `GHOSTAI_FIDELITY_ORIGINAL`, or a directory beside the repository root that
 * is not in the tree and not in the manifest. The reference is another
 * product's source: it is not vendored here, it is not named here, and a
 * checkout without it is the normal case rather than a broken one — which is
 * why the gate skips rather than fails when this path is empty.
 */
export const DEFAULT_ORIGINAL_ROOT: string = resolve(
  process.env.GHOSTAI_FIDELITY_ORIGINAL ??
    join(process.cwd(), '..', '..', '.fidelity-reference'),
);

const HTML = 'text/html; charset=utf-8';
const CSS = 'text/css; charset=utf-8';

const TYPES: Readonly<Record<string, string>> = {
  '.html': HTML,
  '.css': CSS,
  '.js': 'text/javascript; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.webp': 'image/webp',
  '.png': 'image/png',
  '.woff2': 'font/woff2',
};

interface Reference {
  readonly url: string;
  readonly root: string;
  close(): Promise<void>;
}

interface ReferenceOptions {
  readonly root?: string;
  /**
   * Let the document reach the font CDN.
   *
   * `false` — the default, and what the gate uses — makes the reference render
   * with no network at all. See the note at the top of this file for why the
   * capture script asks for the opposite.
   */
  readonly webfonts?: boolean;
}

/**
 * Removes every `<link>` pointing off this origin.
 *
 * Both the stylesheets and the `preconnect` hints: a preconnect on its own
 * still opens a TCP connection to a third party, which is the thing being
 * removed rather than an optimisation for it.
 */
function stripRemoteLinks(html: string): string {
  return html.replace(/<link\b[^>]*href=["']https?:\/\/[^"']*["'][^>]*>/gi, '');
}

/** True when the reference is where it is expected to be. */
export function referenceAvailable(
  root: string = DEFAULT_ORIGINAL_ROOT,
): boolean {
  return existsSync(join(root, 'index.html'));
}

export async function serveReference(
  options: ReferenceOptions = {},
): Promise<Reference> {
  const root = options.root ?? DEFAULT_ORIGINAL_ROOT;
  const webfonts = options.webfonts ?? false;

  if (!referenceAvailable(root)) {
    throw new Error(
      `No index.html under ${root}. Set GHOSTAI_FIDELITY_ORIGINAL to the reference build.`,
    );
  }

  const server: Server = createServer((request, response) => {
    const path = new URL(request.url ?? '/', 'http://localhost').pathname;
    // `normalize` then a prefix check: a request for `/../../etc/passwd` is a
    // 403 rather than a file read, even in a test-only server.
    const target = join(root, normalize(path === '/' ? '/index.html' : path));
    if (!target.startsWith(root + sep) && target !== join(root, 'index.html')) {
      response.writeHead(403).end();
      return;
    }

    const extension = extname(target);

    // Scripts answer 204: the browser gets a document, not a 404 in the console
    // that would itself be a difference between one run and the next.
    if (extension === '.js') {
      response.writeHead(204).end();
      return;
    }

    if (!existsSync(target) || !statSync(target).isFile()) {
      response.writeHead(404).end();
      return;
    }

    if (extension === '.html') {
      const html = readFileSync(target, 'utf8');
      response
        .writeHead(200, { 'content-type': HTML })
        .end(webfonts ? html : stripRemoteLinks(html));
      return;
    }

    if (extension === '.css') {
      // Only the *remote* imports. The reference's entry stylesheet is nothing
      // but `@import url('css/…')`, so a blanket strip removes the design
      // rather than the network dependency — and leaves a reference that
      // measures as an unstyled document, which is a baseline that would pass
      // nothing and fail everything for the same reason.
      const css = readFileSync(target, 'utf8').replace(
        /@import\s+url\(\s*['"]?(?:https?:)?\/\/[^)]*\)\s*;/g,
        '',
      );
      response.writeHead(200, { 'content-type': CSS }).end(css);
      return;
    }

    response.writeHead(200, {
      'content-type': TYPES[extension] ?? 'application/octet-stream',
    });
    createReadStream(target).pipe(response);
  });

  await new Promise<void>((ready) => {
    server.listen(0, '127.0.0.1', ready);
  });
  const address = server.address() as AddressInfo;

  return {
    url: `http://127.0.0.1:${String(address.port)}`,
    root,
    close: async () => {
      await new Promise<void>((done, fail) => {
        server.close((error) => {
          if (error) fail(error);
          else done();
        });
      });
    },
  };
}
