/**
 * The context budget, under the composer that is about to spend it.
 *
 * "Will this message fit" is a question asked *while composing*, and the answer
 * belongs where the question is. An icon in the corner of the header would be
 * the furthest point in the layout from the box you type into — so the number
 * sits under the input, and the full breakdown is one press away rather than
 * the only way to see anything.
 *
 * A plain `<button>` with visible text rather than an icon control: it needs no
 * `aria-label`, and its accessible name is the figure it is showing.
 *
 * It renders nothing at all when there is nothing to measure. A fresh tab has a
 * session key the socket minted and no stored row behind it, so the request
 * 404s — and a red error under the composer of a conversation that has simply
 * not started yet is answering a question nobody asked.
 */

import { useQuery } from '@tanstack/react-query';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '@/lib/api.js';
import { cn } from '@/lib/cn.js';
import { useFormat } from '@/lib/use-format.js';
import { queryKeys } from '@/lib/query.js';
import { summariseContext } from './breakdown.js';
import { ContextDialog } from './context-inspector.js';

/** The same table the dialog uses, so the strip and the panel cannot disagree. */
const SEGMENT_FILLS: Readonly<Record<string, string>> = {
  systemPrompt: 'context-fill--system-prompt',
  tools: 'context-fill--tools',
  messages: 'context-fill--messages',
  other: 'context-fill--other',
};

const FALLBACK_FILL = 'context-fill--fallback';

export function ContextStrip({
  sessionKey,
}: {
  readonly sessionKey: string | undefined;
}): JSX.Element | null {
  const { t } = useTranslation();
  const fmt = useFormat();
  const [open, setOpen] = useState(false);

  const context = useQuery({
    queryKey: queryKeys.context(sessionKey ?? ''),
    queryFn: ({ signal }) => api.context(sessionKey ?? '', signal),
    enabled: sessionKey !== undefined,
    // A conversation that has not started 404s, and that is not a condition
    // worth retrying three times under the composer.
    retry: false,
  });

  if (sessionKey === undefined || !context.isSuccess) return null;

  const budget = summariseContext(context.data, t);
  // Past the window the segments are scaled to the bar so none is clipped, and
  // the overflow is said in words — the same rule the dialog follows.
  const scale =
    budget.over && budget.usedPercent > 0 ? 100 / budget.usedPercent : 1;

  return (
    <>
      <button
        type="button"
        className="context-strip"
        onClick={() => {
          setOpen(true);
        }}
      >
        <span className="context-strip__bar" aria-hidden="true">
          {budget.segments.map((segment) => (
            <span
              key={segment.key}
              className={cn(
                'context-strip__fill',
                SEGMENT_FILLS[segment.key] ?? FALLBACK_FILL,
              )}
              style={{ width: `${String(segment.percent * scale)}%` }}
            />
          ))}
        </span>

        <span className="context-strip__label">
          {fmt.tokens(budget.usedTokens)} of {fmt.tokens(budget.windowTokens)} ·{' '}
          {budget.over ? (
            <span className="context-strip__over">over the window</span>
          ) : (
            `${String(Math.round(budget.usedPercent))}%`
          )}
        </span>
      </button>

      <ContextDialog
        sessionKey={sessionKey}
        open={open}
        onOpenChange={setOpen}
      />
    </>
  );
}
