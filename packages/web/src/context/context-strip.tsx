/**
 * The context budget, under the composer that is about to spend it.
 *
 * "Will this message fit" is a question asked *while composing*, and the answer
 * belongs where the question is. An icon in the corner of the header would be
 * the furthest point in the layout from the box you type into, so the number
 * sits under the input, and the full breakdown is one press away rather than
 * the only way to see anything.
 *
 * A plain `<button>` with visible text rather than an icon control. The text
 * is shortened for a glance (`41.2k of 65.5k`), so the accessible name is a
 * visually hidden sentence with the exact figures instead: a screen reader
 * reading "41.2k" says neither the number nor its unit. `title` would carry
 * the same sentence only to a mouse.
 *
 * The figure carries no estimate mark. While it is a character estimate the
 * screen reader sentence says "about"; once the model has replied, the server
 * scales it by the provider's own count.
 *
 * It renders nothing at all when there is nothing to measure. A fresh tab has a
 * session key the socket minted and no stored row behind it, so the request
 * 404s — and a red error under the composer of a conversation that has simply
 * not started yet is answering a question nobody asked.
 */

import { useQuery } from '@tanstack/react-query';
import { Suspense, lazy, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '@/lib/api.js';
import { cn } from '@/lib/cn.js';
import { useFormat } from '@/lib/use-format.js';
import { CONVERSATION_GC_MS, queryKeys } from '@/lib/query.js';
import { summariseContext, type ContextTone } from './breakdown.js';

// Loaded on the first click. The strip is on every chat screen, and the
// inspector behind it is opened rarely.
const ContextDialog = lazy(async () => ({
  default: (await import('./context-inspector.js')).ContextDialog,
}));

/** The same table the dialog uses, so the strip and the panel cannot disagree. */
const SEGMENT_FILLS: Readonly<Record<string, string>> = {
  systemPrompt: 'context-fill--system-prompt',
  tools: 'context-fill--tools',
  messages: 'context-fill--messages',
  other: 'context-fill--other',
};

const FALLBACK_FILL = 'context-fill--fallback';

const TONES: Readonly<Record<ContextTone, string | undefined>> = {
  calm: undefined,
  warning: 'context-strip--warning',
  danger: 'context-strip--danger',
};

export function ContextStrip({
  sessionKey,
}: {
  readonly sessionKey: string | undefined;
}): JSX.Element | null {
  const { t } = useTranslation();
  const fmt = useFormat();
  const [open, setOpen] = useState(false);
  // Stays true after a close, so the dialog can animate out.
  const [opened, setOpened] = useState(false);

  const context = useQuery({
    queryKey: queryKeys.contextSummary(sessionKey ?? ''),
    queryFn: ({ signal }) => api.context(sessionKey ?? '', signal, true),
    enabled: sessionKey !== undefined,
    // A conversation that has not started 404s, and that is not a condition
    // worth retrying three times under the composer.
    retry: false,
    gcTime: CONVERSATION_GC_MS,
  });

  if (sessionKey === undefined || !context.isSuccess) return null;

  const budget = summariseContext(context.data, t);
  // Past the window the segments are scaled to the bar so none is clipped, and
  // the overflow is said in words — the same rule the dialog follows.
  const scale =
    budget.over && budget.usedPercent > 0 ? 100 / budget.usedPercent : 1;
  const share = budget.over ? t('context.over') : budget.share;

  return (
    <>
      <button
        type="button"
        className={cn('context-strip', TONES[budget.tone])}
        onClick={() => {
          setOpened(true);
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

        <span className="context-strip__label" aria-hidden="true">
          {t('context.ofWindow', {
            used: fmt.tokensShort(budget.usedTokens),
            window: fmt.tokensShort(budget.windowTokens),
          })}
          {' · '}
          <span className="context-strip__share">{share}</span>
        </span>
        <span className="sr-only">
          {t(budget.measured ? 'context.summary' : 'context.summaryEstimated', {
            used: fmt.tokens(budget.usedTokens),
            window: fmt.tokens(budget.windowTokens),
            share,
          })}
        </span>
      </button>

      {opened && (
        <Suspense fallback={null}>
          <ContextDialog
            sessionKey={sessionKey}
            open={open}
            onOpenChange={setOpen}
          />
        </Suspense>
      )}
    </>
  );
}
