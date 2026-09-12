/**
 * Block splitting, and the property the streaming renderer depends on.
 *
 * The property: while text is appended, every block but the last has the same
 * `raw` as it did on the previous frame. That is the whole basis of the memo
 * comparator in `markdown.tsx` — if it stopped holding, a paragraph that
 * finished ten seconds ago would re-render on every token, taking the reader's
 * selection with it.
 */

import { describe, expect, it } from 'vitest';
import type { Tokens } from 'marked';

import {
  fenceLanguage,
  inlineTokens,
  splitBlocks,
} from '@/chat/markdown/blocks.js';

describe('splitBlocks', () => {
  it('is empty for an empty answer', () => {
    expect(splitBlocks('')).toEqual([]);
  });

  it('splits top-level blocks and drops the blank lines between them', () => {
    const blocks = splitBlocks('# Title\n\nA paragraph.\n\n- one\n- two\n');

    expect(blocks.map((block) => block.token.type)).toEqual([
      'heading',
      'paragraph',
      'list',
    ]);
    expect(blocks.map((block) => block.key)).toEqual(['0', '1', '2']);
  });

  it('leaves earlier blocks byte-identical as more text arrives', () => {
    const partial = 'First paragraph.\n\nSecond para';
    const complete = `${partial}graph.\n\nAnd a third.`;

    const before = splitBlocks(partial);
    const after = splitBlocks(complete);

    // This is the memo comparator's entire contract.
    expect(after[0]?.raw).toBe(before[0]?.raw);
    expect(after[1]?.raw).not.toBe(before[1]?.raw);
  });

  it('lexes a fence that has not been closed yet', () => {
    const blocks = splitBlocks('```ts\nconst a = 1;\n');

    // Mid-stream this is exactly what should be on screen: a code block with
    // what has arrived, rather than a paragraph of backticks.
    expect(blocks).toHaveLength(1);
    expect(blocks[0]?.token.type).toBe('code');
  });

  it('renders a single newline as a break, because a model means it', () => {
    const blocks = splitBlocks('one\ntwo');
    const tokens = inlineTokens(blocks[0]?.token ?? { type: 'space', raw: '' });

    expect(tokens.map((token) => token.type)).toContain('br');
  });

  it('reads GFM tables and task lists', () => {
    const table = splitBlocks('| a | b |\n| - | - |\n| 1 | 2 |\n');
    expect(table[0]?.token.type).toBe('table');

    const tasks = splitBlocks('- [x] done\n- [ ] not\n');
    const list = tasks[0]?.token as Tokens.List;
    expect(list.items.map((item) => item.checked)).toEqual([true, false]);
  });
});

describe('inlineTokens', () => {
  it('is empty for a leaf block', () => {
    const code = splitBlocks('```\nx\n```')[0]?.token;

    expect(inlineTokens(code ?? { type: 'space', raw: '' })).toEqual([]);
  });
});

describe('fenceLanguage', () => {
  const fence = (source: string): Tokens.Code =>
    splitBlocks(source)[0]?.token as Tokens.Code;

  it('takes the first word and lower-cases it', () => {
    expect(fenceLanguage(fence('```TypeScript\nx\n```'))).toBe('typescript');
    // Fences carry highlighter directives after the language; they are not ours.
    expect(fenceLanguage(fence('```ts twoslash\nx\n```'))).toBe('ts');
  });

  it('is empty when the fence declared nothing, or nothing usable', () => {
    expect(fenceLanguage(fence('```\nx\n```'))).toBe('');
    expect(fenceLanguage(fence('```{r, echo=FALSE}\nx\n```'))).toBe('');
  });

  it('keeps the punctuation a language name legitimately contains', () => {
    expect(fenceLanguage(fence('```c++\nx\n```'))).toBe('c++');
    expect(fenceLanguage(fence('```objective-c\nx\n```'))).toBe('objective-c');
  });
});
