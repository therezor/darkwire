/**
 * Nothing in the UI reaches off-origin.
 *
 * GhostAI is self-hosted, and some installs are air-gapped. A font CDN, a
 * Google Fonts `<link>` or an analytics beacon would mean a first paint that
 * leaks every user's IP to a third party — and, on a machine with no route out,
 * a page that renders in Times New Roman while a request times out.
 *
 * The rule is easier to hold than to restore, which is why it is a test rather
 * than a note: the first `<link href="https://…">` is caught on the commit that
 * adds it.
 */

import { describe, expect, it } from 'vitest';

import { collectSources } from '@/tokens/run-gates.js';

const sources = collectSources();

/** Anything that would resolve to a host other than the one serving the page. */
const EXTERNAL_URL =
  /(?:https?:)?\/\/(?!localhost|127\.0\.0\.1)[a-z0-9-]+\.[a-z]/gi;

describe('the shipped UI', () => {
  it('references no external origin', () => {
    const offenders = sources.flatMap(({ file, source }) =>
      [...source.matchAll(EXTERNAL_URL)].map((match) => `${file}: ${match[0]}`),
    );

    expect(offenders).toEqual([]);
  });

  it('has no preconnect, dns-prefetch or cross-origin stylesheet', () => {
    const index = sources.find((source) => source.file === 'index.html');

    expect(index?.source).not.toMatch(/rel=["'](?:preconnect|dns-prefetch)/);
    expect(index?.source).not.toMatch(/<link[^>]+href=["']https?:/);
  });

  it('loads its fonts from npm rather than from a CDN', () => {
    const main = sources.find((source) => source.file === 'src/main.tsx');

    expect(main?.source).toContain("import '@fontsource-variable/inter'");
    expect(main?.source).toContain(
      "import '@fontsource-variable/jetbrains-mono'",
    );
  });
});
