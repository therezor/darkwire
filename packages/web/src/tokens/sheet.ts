/**
 * Reads `tokens.css` the way a browser would, for one theme at a time.
 *
 * The contrast assertion could have been written against a TypeScript object
 * that the stylesheet was generated from — but then the thing under test would
 * be the generator, and the stylesheet that actually ships could drift from it
 * without a single test going red. So this parses the real file: the same bytes
 * Vite serves are the bytes the assertion measures.
 *
 * It is a deliberately small subset of CSS — top-level rules, one level of
 * `@media` nesting, custom properties only — because `tokens.css` is a
 * deliberately small subset of CSS. Anything outside it throws rather than
 * being skipped: a token the parser silently ignored is a token nothing checks.
 */

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { oklchToRgba, rgb255ToRgba, type Rgba } from './color.js';

export type ThemeName = 'dark' | 'light';

/** Absolute path to the one file the gates exempt. */
const TOKENS_CSS: string = join(
  dirname(fileURLToPath(import.meta.url)),
  '..',
  'styles',
  'tokens.css',
);

/** One `--custom-property: value` pair, with the selector that declared it. */
interface Declaration {
  readonly property: string;
  readonly value: string;
  readonly selector: string;
  /** Present when the rule sat inside an `@media`. */
  readonly media: string | undefined;
}

export function readTokensCss(): string {
  return readFileSync(TOKENS_CSS, 'utf8');
}

/**
 * Every custom-property declaration in source order.
 *
 * Exported because `tokens.test.ts` uses it to assert the two light seed blocks
 * are identical — the one duplication `tokens.css` deliberately carries.
 */
export function parseDeclarations(css: string): readonly Declaration[] {
  const source = stripComments(css);
  const out: Declaration[] = [];

  let index = 0;
  while (index < source.length) {
    const open = source.indexOf('{', index);
    if (open === -1) break;

    const prelude = source.slice(index, open).trim();
    const close = matchingBrace(source, open);
    const body = source.slice(open + 1, close);

    if (prelude.startsWith('@media')) {
      // One level of nesting is all the sheet uses, so recursion terminates
      // against a body that has no further at-rules.
      for (const nested of parseDeclarations(body)) {
        out.push({ ...nested, media: prelude.slice('@media'.length).trim() });
      }
    } else {
      for (const [property, value] of parseBody(body, prelude)) {
        out.push({ property, value, selector: prelude, media: undefined });
      }
    }

    index = close + 1;
  }

  return out;
}

/**
 * Resolves every `--*` custom property for one theme, following `var()` chains
 * to a literal.
 *
 * Selection mirrors the cascade the sheet is written for: `:root` always
 * applies, the light blocks apply only for `light`, and later wins. `@theme`
 * blocks are included — `--color-accent` is declared there, and it is the name
 * every component reads.
 */
export function resolveTokens(
  css: string,
  theme: ThemeName,
): ReadonlyMap<string, string> {
  const raw = new Map<string, string>();
  for (const decl of parseDeclarations(css)) {
    if (appliesTo(decl, theme)) raw.set(decl.property, decl.value);
  }

  const resolved = new Map<string, string>();
  for (const key of raw.keys()) resolved.set(key, expand(key, raw, new Set()));
  return resolved;
}

/** Resolves one token all the way to sRGB. Throws on a notation the sheet should not contain. */
export function toRgba(value: string): Rgba {
  const oklch =
    /^oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)\s*(?:\/\s*([\d.]+)\s*)?\)$/.exec(
      value,
    );
  if (oklch) {
    const [, l, c, h, a] = oklch;
    return oklchToRgba(
      Number(l),
      Number(c),
      Number(h),
      a === undefined ? 1 : Number(a),
    );
  }

  const rgb = /^rgb\(\s*(\d+)\s+(\d+)\s+(\d+)\s*(?:\/\s*([\d.]+)\s*)?\)$/.exec(
    value,
  );
  if (rgb) {
    const [, r, g, b, a] = rgb;
    return rgb255ToRgba(
      Number(r),
      Number(g),
      Number(b),
      a === undefined ? 1 : Number(a),
    );
  }

  throw new Error(`Not a colour this parser supports: ${value}`);
}

/**
 * True when the rule that carried this declaration is in effect for `theme`.
 *
 * The light seeds live in two blocks — a `prefers-color-scheme` query and a
 * `[data-theme='light']` attribute — and this resolves the attribute branch,
 * which is the one the app actually runs under: the inline script in
 * `index.html` stamps a resolved theme before first paint, so `data-theme` is
 * always set by the time anything renders.
 */
function appliesTo(decl: Declaration, theme: ThemeName): boolean {
  if (decl.media !== undefined) return false; // the attribute branch is the one under test
  if (decl.selector.includes("[data-theme='light']")) return theme === 'light';
  if (decl.selector.includes("[data-theme='dark']")) return theme === 'dark';
  return true;
}

/** Substitutes `var(--x)` references until the value is a literal. */
function expand(
  key: string,
  raw: ReadonlyMap<string, string>,
  seen: ReadonlySet<string>,
): string {
  if (seen.has(key)) throw new Error(`Cyclic custom property: ${key}`);

  const value = raw.get(key);
  if (value === undefined) throw new Error(`Undefined custom property: ${key}`);

  const next = new Set([...seen, key]);
  return value.replace(/var\(\s*(--[\w-]+)\s*\)/g, (match, ref: string) =>
    expand(ref, raw, next),
  );
}

function parseBody(
  body: string,
  prelude: string,
): ReadonlyArray<readonly [string, string]> {
  const out: Array<readonly [string, string]> = [];

  for (const statement of splitTopLevel(body)) {
    const trimmed = statement.trim();
    if (trimmed === '') continue;

    // A nested at-rule or plain rule inside a `:root` block. `tokens.css` has
    // none; the base layer's nested `@media` lives in `base.css`, not here.
    if (trimmed.includes('{')) {
      throw new Error(`Unexpected nested rule in ${prelude}`);
    }

    const colon = trimmed.indexOf(':');
    if (colon === -1) {
      throw new Error(`Unparsable declaration in ${prelude}: ${trimmed}`);
    }

    const property = trimmed.slice(0, colon).trim();
    const value = trimmed.slice(colon + 1).trim();
    if (property.startsWith('--')) {
      out.push([property, collapseWhitespace(value)]);
    }
  }

  return out;
}

/** Splits on `;` at paren depth zero, so `rgb(1 2 3 / 0.5)` survives intact. */
function splitTopLevel(body: string): readonly string[] {
  const out: string[] = [];
  let depth = 0;
  let start = 0;

  for (let i = 0; i < body.length; i += 1) {
    const char = body[i];
    if (char === '(') depth += 1;
    else if (char === ')') depth -= 1;
    else if (char === ';' && depth === 0) {
      out.push(body.slice(start, i));
      start = i + 1;
    }
  }

  out.push(body.slice(start));
  return out;
}

function matchingBrace(source: string, open: number): number {
  let depth = 0;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === '{') depth += 1;
    else if (source[i] === '}') {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  throw new Error('Unbalanced braces in tokens.css');
}

function stripComments(css: string): string {
  return css.replace(/\/\*[\s\S]*?\*\//g, '');
}

function collapseWhitespace(value: string): string {
  return value.replace(/\s+/g, ' ').trim();
}
