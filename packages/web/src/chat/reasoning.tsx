/**
 * The reasoning block.
 *
 * Collapsed by default and never in the answer's own typography. Reasoning is
 * the model thinking out loud — it contradicts itself, abandons approaches and
 * is explicitly not a commitment — and rendering it in the same voice as the
 * answer invites it to be read as one. Smaller, dimmer, behind a disclosure,
 * and available to anyone who wants to know how the answer was reached.
 *
 * It auto-expands while it is the only thing arriving, because a turn that
 * spends thirty seconds reasoning before its first token would otherwise show
 * an empty bubble and a spinner. The moment answer text starts, it collapses
 * again on its own — and if the answer never comes, it stays open, because at
 * that point the reasoning is not an aside about the answer, it is all there is.
 */

import { Brain, ChevronRight } from 'lucide-react';
import { useEffect, useId, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { cn } from '@/lib/cn.js';

interface ReasoningBlockProps {
  readonly text: string;
  /** True while this is the newest thing on a turn that has produced no answer yet. */
  readonly live?: boolean;
  /**
   * True when this reasoning is the whole of what the turn produced.
   *
   * A finished turn that reasoned and then said nothing would otherwise render
   * as a single collapsed strip and a footer — the user sees a message that
   * appears to be empty and has no way to tell that anything happened. When the
   * reasoning is all there is, it stops being an aside and becomes the content,
   * so it is shown rather than hidden behind a disclosure nobody knows to open.
   */
  readonly expanded?: boolean;
}

export function ReasoningBlock({
  text,
  live = false,
  expanded = false,
}: ReasoningBlockProps): JSX.Element {
  const { t } = useTranslation();
  // One condition, so a turn that ends without an answer stays open rather than
  // collapsing the instant `live` goes false — which is the same frame the
  // reasoning becomes the only thing there is to read.
  const shouldOpen = live || expanded;
  const [open, setOpen] = useState(shouldOpen);
  const [pinned, setPinned] = useState(false);
  const bodyId = useId();

  // Follows `shouldOpen` until the reader takes over. Without the pin, a click to
  // keep it open would be undone by the first answer token.
  useEffect(() => {
    if (!pinned) setOpen(shouldOpen);
  }, [shouldOpen, pinned]);

  return (
    <div className="reasoning">
      <button
        type="button"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => {
          setPinned(true);
          setOpen((value) => !value);
        }}
        className="reasoning__toggle"
      >
        <ChevronRight
          className={cn(
            'disclosure-chevron',
            open && 'disclosure-chevron--open',
          )}
        />
        <Brain />
        <span>{t('chat.reasoning')}</span>
        {live && <span className="reasoning__live">thinking…</span>}
      </button>

      <div id={bodyId} hidden={!open} className="reasoning__body">
        {text}
      </div>
    </div>
  );
}
