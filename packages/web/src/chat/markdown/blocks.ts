/**
 * Splitting a streaming answer into blocks.
 *
 * The naive way to render streaming markdown is to parse the whole buffer on
 * every delta and render the result. That is O(n) parse × O(n) deltas — O(n²)
 * over a long answer — and, worse than slow, it throws away the DOM: a
 * re-render of the entire subtree collapses the user's text selection and, if
 * anything above the caret changed height, their scroll position. Watching an
 * answer arrive while being unable to select the paragraph that already
 * finished is the failure this exists to avoid.
 *
 * The render is the expensive half. Markdown is append-only while streaming
 * (text lands at the end), so only the trailing token is live and every earlier
 * one is final. Keying each block on its own `raw` text is what lets
 * `React.memo` skip it: a paragraph that finished four seconds ago is the same
 * string this frame as last, so its subtree is never touched again.
 *
 * The same fact bounds the lex. Given the previous frame's blocks, only the
 * text from the start of the trailing block is lexed again, so a long answer
 * costs the length of its last block per delta rather than its whole length.
 *
 * Two smaller decisions:
 *
 *  - **`breaks: true`.** CommonMark folds a single newline into a space, which
 *    is right for prose written for a document and wrong for a model that
 *    line-breaks a list of options and expects to see them on separate lines.
 *  - **`space` tokens are dropped.** They carry the blank lines *between*
 *    blocks, which the block spacing already expresses. Keeping them would also
 *    make the block index unstable, since a trailing newline appears and
 *    disappears as the next block starts.
 */

import { marked, type Token, type Tokens } from 'marked';

/** One top-level markdown block, with the key and the raw text memoisation needs. */
interface MarkdownBlock {
  /**
   * Stable across frames.
   *
   * The index, because blocks only ever append while an answer streams: block 3
   * is block 3 for the whole life of the message. Keying on the content instead
   * would remount a block the moment a duplicate paragraph appeared above it.
   */
  readonly key: string;
  readonly token: Token;
  /** The token's source text. The memo comparator's only input. */
  readonly raw: string;
  /**
   * Where the block starts in the source. Absent when the lexer rewrote the
   * input (it normalises line endings), which rules out resuming from it.
   */
  readonly offset: number | undefined;
}

/** One frame's lex, which the next frame of the same answer resumes from. */
export interface LexedMarkdown {
  readonly text: string;
  readonly blocks: readonly MarkdownBlock[];
}

const OPTIONS = { gfm: true, breaks: true } as const;

/**
 * Lex `markdown` into top-level blocks.
 *
 * Incomplete input is not an error: an unterminated fence lexes as a code block
 * containing what has arrived so far, and a half-written table lexes as a
 * paragraph until its delimiter row lands. Both are exactly what should be on
 * screen mid-stream.
 */
export function splitBlocks(
  markdown: string,
  previous?: LexedMarkdown,
): readonly MarkdownBlock[] {
  if (markdown === '') return [];

  // Every block but the last is final once more text is appended, so the lex
  // resumes where the last one started.
  const tail = previous?.blocks.at(-1);
  if (
    previous !== undefined &&
    tail?.offset !== undefined &&
    markdown.startsWith(previous.text)
  ) {
    const kept = previous.blocks.slice(0, -1);
    return [
      ...kept,
      ...lex(markdown.slice(tail.offset), tail.offset, kept.length),
    ];
  }
  return lex(markdown, 0, 0);
}

function lex(
  source: string,
  base: number,
  firstIndex: number,
): MarkdownBlock[] {
  const tokens = marked.lexer(source, OPTIONS);
  const exact =
    tokens.reduce((length, token) => length + token.raw.length, 0) ===
    source.length;

  const blocks: MarkdownBlock[] = [];
  let offset = base;
  for (const token of tokens) {
    if (token.type !== 'space') {
      blocks.push({
        key: String(firstIndex + blocks.length),
        token,
        raw: token.raw,
        offset: exact ? offset : undefined,
      });
    }
    offset += token.raw.length;
  }
  return blocks;
}

/** Inline tokens of a block, for the renderer. Absent on leaves like `code`. */
export function inlineTokens(token: Token): readonly Token[] {
  return 'tokens' in token && Array.isArray(token.tokens) ? token.tokens : [];
}

/**
 * The language a fence declared, normalised.
 *
 * Fences carry anything after the backticks — `ts twoslash`, `js {1,3}` — so the
 * first word is the language and the rest is a highlighter's business, not
 * ours. An empty result means "no language", which is a plain code block rather
 * than a failed lookup.
 */
export function fenceLanguage(token: Tokens.Code): string {
  const first = (token.lang ?? '').trim().split(/\s+/)[0] ?? '';
  return /^[\w+#-]+$/.test(first) ? first.toLowerCase() : '';
}
