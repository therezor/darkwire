/**
 * What a link in model output is allowed to be.
 *
 * Everything rendered in the transcript came from somewhere else — a model, or a
 * tool result the model read — and a markdown link is the shortest path from
 * "the page fetched a web page" to "the page ran a script". Two rules:
 *
 *  - **Only `http`, `https` and `mailto` become links.** `javascript:` is the
 *    obvious one; `data:` is the one people forget, and `data:text/html` in an
 *    `<a>` is a same-origin document in every browser that still honours it. A
 *    refused scheme renders as text, so nothing is hidden — the user can still
 *    read the URL, they just cannot click it into existence.
 *  - **Images must be same-origin.** GhostAI is self-hosted and some installs
 *    have no route out; an `<img>` pointing at a third party would leak the
 *    reader's IP to whoever wrote the markdown, which in a tool result is not
 *    the user and not us. Off-origin images render as a link to click
 *    deliberately.
 *
 * Both functions take the base URL rather than reading `location`, so the rules
 * are testable without a jsdom navigation.
 */

const LINK_SCHEMES = new Set(['http:', 'https:', 'mailto:']);

/** The href to use, or `undefined` when this is not a scheme worth linking. */
export function safeHref(
  href: string,
  base: string = documentBase(),
): string | undefined {
  const url = parse(href, base);
  if (url === undefined) return undefined;
  return LINK_SCHEMES.has(url.protocol) ? url.href : undefined;
}

/** True when this URL is served by the same origin as the page. */
export function isSameOrigin(
  href: string,
  base: string = documentBase(),
): boolean {
  const url = parse(href, base);
  if (url === undefined) return false;
  return url.origin === new URL(base).origin;
}

function parse(href: string, base: string): URL | undefined {
  const trimmed = href.trim();
  if (trimmed === '') return undefined;
  try {
    return new URL(trimmed, base);
  } catch {
    // Not a URL at all. A relative fragment resolves fine, so this is genuinely
    // malformed input rather than a shorthand.
    return undefined;
  }
}

function documentBase(): string {
  return globalThis.location.href;
}
