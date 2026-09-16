/**
 * The context inspector.
 *
 * This is the panel that makes the token budget legible instead of a mystery.
 * "Why did the model forget what I said ten turns ago" has one honest answer —
 * the window filled and the oldest turns fell out of it — and without this the
 * only way to see that coming is to notice it after it happens.
 *
 * The bar is of the *window*, not of what is used, so the empty space to the
 * right is the headroom left. The one case where that breaks down is a budget
 * already over the window: the segments would run off the end and the last one
 * would be silently clipped, so past 100% they are scaled to the bar and the
 * overflow is stated in words instead. A chart that quietly truncates the
 * segment that caused the problem is worse than no chart.
 *
 * `GET /api/sessions/:key/context` is not cheap — it rebuilds the system prompt
 * and re-estimates the whole window — so it is fetched when the dialog opens
 * rather than kept warm behind a button nobody has pressed.
 */

import { useQuery } from '@tanstack/react-query';
import type { JSX, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';

import type { StoredMessage, ToolDefinition } from '@darkwire/protocol';

import { api, ApiError } from '@/lib/api.js';
import { useFormat } from '@/lib/use-format.js';
import { queryKeys } from '@/lib/query.js';
import { cn } from '@/lib/cn.js';
import { Badge } from '@/components/ui/badge.js';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogHeading,
  DialogSubheading,
} from '@/components/ui/dialog.js';
import { summariseContext, type ContextSegment } from './breakdown.js';

/** Section → fill. Anything the server adds later lands on the neutral fill. */
const SEGMENT_FILLS: Readonly<Record<string, string>> = {
  systemPrompt: 'context-fill--system-prompt',
  tools: 'context-fill--tools',
  messages: 'context-fill--messages',
  runtimeBlock: 'context-fill--runtime-block',
  other: 'context-fill--other',
};

const FALLBACK_FILL = 'context-fill--fallback';

/**
 * The full breakdown, opened from the strip under the composer.
 *
 * The trigger is not here: see `context-strip.tsx`, which is both the trigger
 * and the one-line version of this. It sits under the composer rather than in
 * the header, which would put the measurement about as far from the box you
 * type into as the layout allows.
 */
export function ContextDialog({
  sessionKey,
  open,
  onOpenChange,
}: {
  readonly sessionKey: string | undefined;
  readonly open: boolean;
  readonly onOpenChange: (open: boolean) => void;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="dialog--context">
        <DialogHeader>
          <DialogHeading>{t('context.title')}</DialogHeading>
          <DialogSubheading>{t('context.intro')}</DialogSubheading>
        </DialogHeader>

        {open && sessionKey !== undefined && (
          <ContextBody sessionKey={sessionKey} />
        )}
      </DialogContent>
    </Dialog>
  );
}

export function ContextBody({
  sessionKey,
}: {
  readonly sessionKey: string;
}): JSX.Element {
  const { t } = useTranslation();
  const fmt = useFormat();
  const context = useQuery({
    queryKey: queryKeys.context(sessionKey),
    queryFn: ({ signal }) => api.context(sessionKey, signal),
    // Re-measures on open, and the reason is the strip rather than the timer it
    // used to be. `use-connection.ts` patches the *totals* into this cache from
    // every `context.usage` frame, so mid-turn the bar and the figure here are
    // current while `messages`, `tools` and `systemPrompt` are the last fetch's
    // — a panel whose header says 41k over a list that adds up to 23k. Opening
    // it is the one moment anyone reads those lists, so it is the right moment
    // to pay for them.
    //
    // Still no `staleTime: 0`. The strip under the composer shares this key and
    // is always mounted; zeroing it would make every background revalidation
    // rebuild the whole system prompt, which is what that setting was removed
    // for in the first place.
    refetchOnMount: 'always',
  });

  if (context.isPending) {
    return <p className="page__note">{t('context.measuring')}</p>;
  }
  if (context.isError) {
    // A 404 here is not a failure. The socket mints a session key the moment a
    // tab connects, and the store does not hold a row for it until the first
    // message lands — so a fresh tab asks about a conversation that does not
    // exist yet, and answering that with a red error is answering the wrong
    // question.
    if (context.error instanceof ApiError && context.error.status === 404) {
      return <p className="page__note">{t('context.nothingYet')}</p>;
    }

    return (
      <p role="alert" className="page__error">
        {t('context.loadError', { message: context.error.message })}
      </p>
    );
  }

  const budget = summariseContext(context.data, t);
  // Past the window the segments are scaled to the bar so none is clipped; the
  // overflow is then said in words rather than drawn.
  const scale =
    budget.over && budget.usedPercent > 0 ? 100 / budget.usedPercent : 1;

  return (
    <div className="stack context">
      <div className="cluster context__headline">
        <span className="context__used">{fmt.tokens(budget.usedTokens)}</span>
        <span className="context__of">
          of {fmt.tokens(budget.windowTokens)} tokens ·{' '}
          {budget.usedPercent.toFixed(0)}%
        </span>
        <span className="spacer" />
        {budget.over ? (
          <Badge tone="danger">over the window</Badge>
        ) : (
          <Badge tone="neutral">{fmt.tokens(budget.freeTokens)} free</Badge>
        )}
        {/* The figure the two halves exist to move. Everything else here is
            paid once for the conversation; this is paid again on every request
            of every turn, which is what makes it the number worth acting on. */}
        <Badge tone="neutral">
          {t('context.perIteration', {
            tokens: fmt.tokens(budget.uncachedTokens),
          })}
        </Badge>
      </div>

      {/* The bar is decoration for the table below it, which carries the same
          numbers as text — so it is hidden from the accessibility tree rather
          than announced as a row of empty divs. */}
      <div aria-hidden="true" className="context__bar">
        {budget.segments.map((segment) => (
          <div
            key={segment.key}
            className={cn(SEGMENT_FILLS[segment.key] ?? FALLBACK_FILL)}
            style={{ width: `${String(segment.percent * scale)}%` }}
          />
        ))}
      </div>

      <table className="context__table">
        <caption className="sr-only">{t('context.usageBySection')}</caption>
        <thead>
          <tr>
            <th scope="col">{t('context.section')}</th>
            <th scope="col">{t('context.tokens')}</th>
            <th scope="col">{t('context.share')}</th>
          </tr>
        </thead>
        <tbody>
          {budget.segments.map((segment) => (
            <SegmentRow key={segment.key} segment={segment} />
          ))}
        </tbody>
      </table>

      <div className="cluster context__meta">
        <span>
          {t('context.messagesInWindow', {
            count: context.data.messages.length,
          })}
        </span>
        <span>{sessionKey}</span>
      </div>

      {/* One disclosure per section, in the order they reach the model. Each is
          the *contents* of the row above with the same name, which is what turns
          the table from a set of numbers into something answerable: "tools:
          1,240" and "which tools" are one click apart rather than a question for
          whoever wrote the agent. */}
      <div className="stack context__sections">
        <Section
          fill={SEGMENT_FILLS.systemPrompt}
          label={t('context.systemPrompt')}
        >
          <pre className="context__text">{context.data.systemPrompt}</pre>
        </Section>

        <Section
          fill={SEGMENT_FILLS.tools}
          label={t('context.toolDefinitions', {
            count: context.data.tools.length,
          })}
        >
          {context.data.tools.length === 0 ? (
            <p className="page__note">{t('context.noTools')}</p>
          ) : (
            <ul className="stack context__tools">
              {context.data.tools.map((tool) => (
                <ToolEntry key={tool.name} tool={tool} />
              ))}
            </ul>
          )}
        </Section>

        <Section
          fill={SEGMENT_FILLS.messages}
          label={t('context.session', { count: context.data.messages.length })}
        >
          {context.data.messages.length === 0 ? (
            <p className="page__note">{t('context.nothingYet')}</p>
          ) : (
            <ol className="stack context__messages">
              {context.data.messages.map((stored) => (
                <MessageEntry key={stored.id} stored={stored} />
              ))}
            </ol>
          )}
        </Section>

        {/* Last, because it is last in the request — the trailing turn the loop
            appends after the conversation so the conversation stays cacheable.
            Absent in `raw` mode, where the operator's one template is the whole
            system message and there is no second half to show. */}
        {context.data.runtimeBlock === '' ? null : (
          <Section
            fill={SEGMENT_FILLS.runtimeBlock}
            label={t('context.runtimeBlock')}
          >
            <pre className="context__text">{context.data.runtimeBlock}</pre>
          </Section>
        )}
      </div>
    </div>
  );
}

/**
 * One collapsed section of the prompt.
 *
 * `<details>` rather than a button and state: it is a disclosure, the element
 * exists for exactly this, and it comes with the keyboard behaviour and the
 * `aria-expanded` semantics already correct. The swatch ties it to its row in the
 * table and to its band in the bar.
 */
function Section({
  fill,
  label,
  children,
}: {
  readonly fill: string | undefined;
  readonly label: string;
  readonly children: ReactNode;
}): JSX.Element {
  return (
    <details className="context__section">
      <summary>
        <span
          aria-hidden="true"
          className={cn('context__swatch', fill ?? FALLBACK_FILL)}
        />
        {label}
      </summary>
      <div className="context__section-body">{children}</div>
    </details>
  );
}

/**
 * One tool, as the provider is given it.
 *
 * The schema is shown as formatted JSON rather than summarised, because the
 * reason to open this is almost always that a schema is bigger than expected —
 * and a summary is exactly what hides that.
 */
function ToolEntry({ tool }: { readonly tool: ToolDefinition }): JSX.Element {
  const { t } = useTranslation();

  return (
    <li className="stack context__tool">
      <p className="cluster context__tool-head">
        <code>{tool.name}</code>
        <Badge tone={tool.risk === 'safe' ? 'neutral' : 'warning'}>
          {tool.risk}
        </Badge>
      </p>
      <p className="context__tool-desc">{tool.description}</p>
      <details>
        <summary>{t('context.toolSchema', { name: tool.name })}</summary>
        <pre className="context__text">
          {JSON.stringify(tool.parameters, null, 2)}
        </pre>
      </details>
    </li>
  );
}

/**
 * One message in the window, addressed by the seq the rest of the UI uses.
 *
 * Tool results are shown whole. They are the entries most likely to be the
 * reason a window filled up, and this is the one screen where the envelope and
 * the full untruncated output are the point rather than noise.
 *
 * An assistant's `reasoning` is not shown, and the route does not send it. This
 * screen is what the request contains, and the wire has never carried it — a
 * dimmed block of it here read as "this is in your window", which is the
 * question the screen exists to answer and the one answer it was getting wrong.
 * The transcript still shows it, in the collapsible block beside the answer,
 * where it is a record of what the model did rather than a claim about cost.
 */
function MessageEntry({
  stored,
}: {
  readonly stored: StoredMessage;
}): JSX.Element {
  const { message } = stored;
  // `content` is a bare string for `system` and `tool`, and a part array for the
  // two that can carry images. One branch on the *shape* covers all four roles;
  // branching on the role would need four.
  const text =
    typeof message.content === 'string'
      ? message.content
      : message.content
          .map((part) =>
            part.type === 'text'
              ? part.text
              : part.type === 'file'
                ? `[file: ${part.name ?? part.path}]`
                : `[${part.type}]`,
          )
          .join('');

  const calls = message.role === 'assistant' ? message.toolCalls : [];

  return (
    <li className="stack context__message">
      <p className="cluster context__message-head">
        <Badge tone="neutral">{message.role}</Badge>
        <span className="context__message-seq">#{stored.seq}</span>
      </p>
      {text !== '' && <pre className="context__text">{text}</pre>}
      {calls.map((call) => (
        <pre key={call.id} className="context__text">
          {call.name}({call.argumentsJson})
        </pre>
      ))}
    </li>
  );
}

function SegmentRow({
  segment,
}: {
  readonly segment: ContextSegment;
}): JSX.Element {
  const fmt = useFormat();

  return (
    <tr>
      <th scope="row">
        <span className="row">
          <span
            aria-hidden="true"
            className={cn(
              'context__swatch',
              SEGMENT_FILLS[segment.key] ?? FALLBACK_FILL,
            )}
          />
          {segment.label}
        </span>
      </th>
      <td className="context__tokens">{fmt.tokens(segment.tokens)}</td>
      <td className="context__share">{segment.percent.toFixed(1)}%</td>
    </tr>
  );
}
