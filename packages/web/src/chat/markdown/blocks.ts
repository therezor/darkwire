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
 * The fix is that the *parse* is cheap and the *render* is not. `marked`'s lexer
 * is a single linear pass, so re-lexing the buffer per frame is fine; what has
 * to stop is re-rendering blocks that cannot have changed. Markdown is
 * append-only while streaming — text lands at the end — so only the trailing
 * token is live and every earlier one is final. Keying each block on its own
 * `raw` text is what lets `React.memo` skip it: a paragraph that finished four
 * seconds ago is the same string this frame as last, so its subtree is never
 * touched again.
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
export function splitBlocks(markdown: string): readonly MarkdownBlock[] {
  if (markdown === '') return [];

  const blocks: MarkdownBlock[] = [];
  for (const token of marked.lexer(markdown, OPTIONS)) {
    if (token.type === 'space') continue;
    blocks.push({ key: String(blocks.length), token, raw: token.raw });
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
