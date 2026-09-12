/**
 * Properties of the *whole* stylesheet layer, rather than of the token sheet.
 *
 * These three exist because each of them shipped as a bug first, and each is
 * invisible in review: a rule that reads correctly on its own but is wrong in
 * combination with the markup, or with the rest of the scale.
 *
 *  1. **A `--gap` nothing reads.** `.turn` set `--gap: var(--space-3)` and
 *     declared its own `display: flex`, so the `gap` property that consumes
 *     `--gap` — which lives on `.stack` — never applied. Every part of an
 *     assistant's turn rendered flush against the next with no space at all,
 *     and the stylesheet looked entirely correct.
 *  2. **A type scale whose base is unused.** `--text-base` is the default and
 *     is meant to be *inherited*. When every component restates a smaller step
 *     instead, the default stops being the default: this package once declared
 *     `--text-sm` forty-five times and `--text-base` three.
 *  3. **A root font size.** In any unit. It replaces the number the user chose
 *     in their browser with one the stylesheet chose, and `62.5%` is the same
 *     mistake wearing a disguise.
 */

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';

import { describe, expect, it } from 'vitest';

import { SRC, STYLES } from '@testkit/paths.js';

/** The three layout primitives, which are the only rules that consume `--gap`. */
const PRIMITIVES = ['stack', 'row', 'cluster'];

interface Rule {
  readonly file: string;
  /** The selector, with any comment in the prelude stripped. */
  readonly selector: string;
  readonly body: string;
}

function walk(dir: string): readonly string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry);
    return statSync(full).isDirectory() ? walk(full) : [full];
  });
}

const stylesheets = walk(STYLES).filter((file) => file.endsWith('.css'));
const components = walk(SRC).filter((file) => file.endsWith('.tsx'));

const read = (file: string): string => readFileSync(file, 'utf8');
const stripComments = (css: string): string =>
  css.replace(/\/\*[\s\S]*?\*\//g, '');

/** Every `selector { … }` in the styles directory, flattened. */
const rules: readonly Rule[] = stylesheets.flatMap((file) =>
  [...stripComments(read(file)).matchAll(/([^{}]+)\{([^{}]*)\}/g)].flatMap(
    (match) =>
      (match[1] ?? '')
        .split(',')
        .map((selector) => selector.trim().split('\n').at(-1)?.trim() ?? '')
        .filter((selector) => selector !== '')
        .map((selector) => ({
          file: relative(SRC, file),
          selector,
          body: match[2] ?? '',
        })),
  ),
);

/**
 * Every `className` value in the package, as a list of tokens.
 *
 * Both spellings: the attribute form, and the string literals inside a
 * `cn(...)` call. A class and the primitive it depends on are always written in
 * the same literal, which is what makes this checkable at all.
 */
function classLists(source: string): readonly string[] {
  const out: string[] = [];

  for (const match of source.matchAll(/className\s*=\s*(?:"([^"]*)"|\{)/g)) {
    if (match[1] !== undefined) {
      out.push(match[1]);
      continue;
    }

    // The brace form: scan to the matching close, then take every literal.
    let index = match.index + match[0].length;
    let depth = 1;
    while (index < source.length && depth > 0) {
      if (source[index] === '{') depth += 1;
      else if (source[index] === '}') depth -= 1;
      index += 1;
    }

    const expression = source.slice(match.index + match[0].length, index - 1);
    for (const literal of expression.matchAll(/'([^']*)'|"([^"]*)"/g)) {
      out.push(literal[1] ?? literal[2] ?? '');
    }
  }

  return out;
}

const classLiterals: ReadonlyArray<{
  readonly file: string;
  readonly value: string;
}> = components.flatMap((file) =>
  classLists(read(file)).map((value) => ({ file: relative(SRC, file), value })),
);

describe('--gap', () => {
  const setters = new Set(
    rules
      .filter((rule) => rule.body.includes('--gap:'))
      .map((rule) => rule.selector)
      .filter((selector) => /^\.[\w-]+$/.test(selector))
      .map((selector) => selector.slice(1)),
  );

  /** True when the rule consumes `--gap` itself, so it needs no primitive. */
  const consumesGap = (name: string): boolean =>
    rules.some(
      (rule) =>
        rule.selector === `.${name}` && /(?<![\w-])gap:/.test(rule.body),
    );

  it('is set by a meaningful number of rules', () => {
    // Guards the two assertions below: a rename that emptied this set would
    // otherwise make them vacuously pass.
    expect(setters.size).toBeGreaterThan(20);
  });

  it('is only ever set on an element that also carries a layout primitive', () => {
    const unwired: string[] = [];

    for (const name of setters) {
      if (consumesGap(name)) continue;

      const uses = classLiterals.filter(({ value }) =>
        value.split(/\s+/).includes(name),
      );
      if (uses.length === 0) {
        unwired.push(`.${name} — sets --gap and is never used in markup`);
        continue;
      }

      for (const { file, value } of uses) {
        const tokens = value.split(/\s+/);
        if (!PRIMITIVES.some((primitive) => tokens.includes(primitive))) {
          unwired.push(
            `.${name} — ${file}: "${value}" has no stack/row/cluster`,
          );
        }
      }
    }

    expect(unwired).toEqual([]);
  });
});

describe('the type scale', () => {
  const fontSizes = rules.flatMap((rule) =>
    [...rule.body.matchAll(/font-size:\s*var\((--text-[\w-]+)\)/g)].map(
      (match) => ({
        ...rule,
        token: match[1] ?? '',
      }),
    ),
  );

  it('declares --text-base exactly once, on body, so everything inherits it', () => {
    const declared = fontSizes.filter((rule) => rule.token === '--text-base');

    // `.markdown h5` and `h6` are the exception that proves it: they sit inside
    // an answer, which is `--text-md`, so returning to the base is a step
    // *down* rather than a restatement of the default.
    const outsideMarkdown = declared.filter(
      (rule) => !rule.selector.startsWith('.markdown'),
    );

    expect(
      outsideMarkdown.map((rule) => `${rule.file}  ${rule.selector}`),
    ).toEqual(['styles/base.css  body']);
  });

  it('reserves the two smallest steps for things that are not sentences', () => {
    // A paragraph at 0.6875rem is a paragraph nobody reads. `2xs` is for
    // uppercase micro-labels and badge text; anything with prose in it belongs
    // at `sm` or above.
    //
    // The budget was 12 and is 14. The two additions are the turn-details
    // definition list and the context strip's figure — an uppercase micro-label
    // ramp and an ambient mono readout, which is precisely what this step is
    // for. Raising it is not free and should not become routine: the assertion
    // below, not this number, is what actually keeps prose out of 2xs, but the
    // cap is what keeps the step from spreading by default.
    const tiny = fontSizes.filter((rule) => rule.token === '--text-2xs');

    expect(tiny.length).toBeLessThanOrEqual(14);
    for (const rule of tiny) {
      expect(
        /--tracking-caps|text-transform: uppercase|font-variant-numeric|^\.badge$/.test(
          rule.body,
        ) ||
          /badge|label|lang|count|time|footer|summary|timing|hint|state|pending/.test(
            rule.selector,
          ),
        `${rule.file} ${rule.selector} is 2xs but reads like prose`,
      ).toBe(true);
    }
  });

  it('uses every step it declares', () => {
    const scale = [
      ...readFileSync(join(STYLES, 'tokens.css'), 'utf8').matchAll(
        /(--text-[\w-]+):/g,
      ),
    ]
      .map((match) => match[1] ?? '')
      .filter((token) => token !== '--text-base'); // set once, on body, and inherited

    const used = new Set(fontSizes.map((rule) => rule.token));
    // A step nothing uses is a step nobody reviews, and the style guide will
    // not show it either.
    expect(scale.filter((token) => !used.has(token))).toEqual([]);
  });
});

describe('the root font size', () => {
  it('is never set, in any unit, by any stylesheet', () => {
    // Not px, and not rem either: `rem` on `:root` resolves against the
    // browser's own default, so setting it there is how `62.5%` — and every
    // variant of that trick — takes the user's choice away.
    const offenders = rules
      .filter((rule) => /^(?:html|:root)\b/.test(rule.selector))
      .filter((rule) => /(?<![\w-])font-size:/.test(rule.body))
      .map((rule) => `${rule.file}  ${rule.selector}`);

    expect(offenders).toEqual([]);
  });

  it('is not set by index.html either', () => {
    const html = read(join(SRC, '..', 'index.html'));
    expect(html).not.toMatch(/(?:html|:root)[^{}]*\{[^}]*font-size/);
  });
});
