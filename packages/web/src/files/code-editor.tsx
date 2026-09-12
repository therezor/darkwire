/**
 * The text surface: a textarea, a syntax-highlighted layer behind it, and a
 * line-number gutter.
 *
 * The technique is the usual one — a transparent `<textarea>` over a coloured
 * `<pre>` — and the two hard parts are both solved by *not* using JavaScript
 * for them:
 *
 *  - **Nothing synchronises scroll position**, because only one element
 *    scrolls. The textarea grows to its own content (`field-sizing: content`
 *    and `wrap="off"`), the highlight layer is pinned over it, and the box
 *    around both is the one thing with `overflow: auto`. A scroll handler
 *    copying `scrollTop` from one node to another is a frame behind by
 *    construction, and the tearing shows up exactly when a reader is scrolling
 *    fast enough to notice.
 *  - **Nothing measures character widths.** The gutter is a separate column
 *    rather than padding inside the text, so no code has to agree with CSS
 *    about how wide a digit is.
 *
 * **The colouring is only ever shown when it belongs to the text on screen.**
 * Highlighting is a tokenise pass over the whole buffer and cannot run per
 * keystroke, so while it is behind, the textarea's own text is made visible and
 * the stale layer is dropped. Typing therefore looks like plain monospace for a
 * moment and snaps into colour when it catches up — which is the honest
 * version. The alternative, leaving the last colouring up, paints old tokens
 * over new characters, and the smear is worst mid-word where it is least
 * excusable. `code-block.tsx` makes the same trade during a stream.
 */

import { useEffect, useMemo, useState, type JSX, type Ref } from 'react';

import { cn } from '@/lib/cn.js';
import { onIdle } from '@/lib/idle.js';
import { useAppTheme } from '@/theme/theme-context.js';
import type { HighlightedLines } from '@/chat/markdown/highlight.js';

interface CodeEditorProps {
  readonly value: string;
  readonly onChange: (value: string) => void;
  readonly readOnly: boolean;
  /** The fence token to highlight under — see `languageForFile`. */
  readonly language: string;
  /** Names the textarea for a screen reader; there is no visible label. */
  readonly label: string;
  readonly textareaRef?: Ref<HTMLTextAreaElement>;
  readonly onKeyDown?: (
    event: React.KeyboardEvent<HTMLTextAreaElement>,
  ) => void;
}

/** What was highlighted, kept with the text it was highlighted from. */
interface Highlighted {
  readonly code: string;
  readonly theme: string;
  readonly lines: HighlightedLines;
}

export function CodeEditor({
  value,
  onChange,
  readOnly,
  language,
  label,
  textareaRef,
  onKeyDown,
}: CodeEditorProps): JSX.Element {
  const { resolved } = useAppTheme();
  const [highlighted, setHighlighted] = useState<Highlighted | undefined>(
    undefined,
  );

  useEffect(() => {
    if (language === '') return undefined;

    let cancelled = false;
    // On idle rather than on change: tokenising half a megabyte is not work to
    // do between two keystrokes, and the text is already legible without it.
    const idle = onIdle(() => {
      void (async () => {
        try {
          const { highlight } = await import('@/chat/markdown/highlight.js');
          const lines = await highlight(value, language, resolved);
          if (!cancelled && lines !== undefined) {
            setHighlighted({ code: value, theme: resolved, lines });
          }
        } catch {
          // A grammar that failed to load, or an engine the browser refused.
          // The text is already on screen and readable; colour is what is lost.
        }
      })();
    });

    return () => {
      cancelled = true;
      idle.cancel();
    };
  }, [value, language, resolved]);

  // The colouring belongs to the text that produced it, and to the theme it was
  // produced under. Either mismatching means the plain text shows instead.
  const fresh = highlighted?.code === value && highlighted.theme === resolved;

  const lineCount = useMemo(() => value.split('\n').length, [value]);

  return (
    <div className="code-editor">
      <div aria-hidden="true" className="code-editor__gutter">
        {Array.from({ length: lineCount }, (value, index) =>
          String(index + 1),
        ).join('\n')}
      </div>

      <div className="code-editor__code">
        {fresh && (
          // `aria-hidden`: it is a second copy of the textarea's value, and a
          // screen reader that read both would read the file twice.
          <pre aria-hidden="true" className="code-editor__highlight">
            {highlighted.lines.map((line, index) => (
              <span key={index} className="code-editor__line">
                {line.map((token, tokenIndex) => (
                  // The colour is the syntax theme's, applied inline. It is one
                  // of the two places a raw colour is correct: a TextMate theme
                  // is not part of the design system and has no token for
                  // "keyword".
                  <span key={tokenIndex} style={{ color: token.color }}>
                    {token.content}
                  </span>
                ))}
                {'\n'}
              </span>
            ))}
          </pre>
        )}

        <textarea
          ref={textareaRef}
          // `off`, so a long line scrolls the box instead of wrapping. A wrapped
          // line in the textarea and an unwrapped one in the `<pre>` would drift
          // apart by a line for every wrap, and the gutter would be wrong too.
          wrap="off"
          spellCheck={false}
          aria-label={label}
          readOnly={readOnly}
          value={value}
          className={cn(
            'code-editor__input',
            fresh && 'code-editor__input--ghost',
          )}
          onChange={(event) => {
            onChange(event.target.value);
          }}
          {...(onKeyDown ? { onKeyDown } : {})}
        />
      </div>
    </div>
  );
}
