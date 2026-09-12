/**
 * A tool call, as a card — and the list of parts a turn or a delegation renders.
 *
 * Both are here because they are mutually recursive: a card that delegated to a
 * subagent renders that subagent's parts inside itself, and one of those parts
 * can be another card. Splitting them across two files would buy a tidier name
 * and an import cycle.
 *
 * What the card has to answer, in the order a reader asks it: what is being
 * run, how dangerous the system considers it, whether it is still going, and
 * what it produced. Everything else — the full arguments, the whole output — is
 * behind a disclosure, because a turn that made six calls should not be six
 * screens of JSON before the answer.
 *
 * A delegation adds one more question ahead of "what it produced": *what did the
 * subagent do*. So the nested run sits above the output rather than below it,
 * and a card that is still delegating opens itself — the same rule the reasoning
 * block uses, for the same reason. A collapsed card over a running subagent is a
 * turn that has silently stopped.
 *
 * Three decisions:
 *
 *  - **The elapsed counter ticks locally.** `tool.progress` arrives every 15
 *    seconds, which is a fine heartbeat and a terrible clock: a card that
 *    updates four times a minute looks frozen for the fourteen seconds in
 *    between, which is exactly the impression the heartbeat exists to prevent.
 *    The server's figure is the floor; the local timer fills the gaps.
 *  - **`exec` output gets a terminal, everything else gets a code block.** The
 *    distinction is real — `exec` output is interleaved stdout and stderr with
 *    ANSI-era conventions and meaningful trailing whitespace, and rendering it
 *    the way a file read is rendered loses the shape of it.
 *  - **The nonce envelope is never shown.** `tool.result` carries the tool's own
 *    output, and stored history is unwrapped in `transcript.ts`. The delimiters
 *    are a defence aimed at the model; showing them would be showing the reader
 *    the machinery instead of the answer.
 */

import {
  AlertCircle,
  Bot,
  CheckCircle2,
  ChevronRight,
  Loader2,
  Terminal,
  Wrench,
} from 'lucide-react';
import { useEffect, useId, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { useQuery } from '@tanstack/react-query';

import type {
  ApprovalScope,
  StoredMessage,
  SubagentRunRef,
  ToolRisk,
} from '@ghostwire/protocol';

import { cn } from '@/lib/cn.js';
import { api } from '@/lib/api.js';
import { formatArgs, formatDuration, summariseArgs } from '@/lib/format.js';
import { queryKeys } from '@/lib/query.js';
import { Badge, type BadgeProps } from '@/components/ui/badge.js';
import {
  fromStoredMessages,
  type SubagentPart,
  type ToolPart,
  type TurnPart,
} from '@/state/transcript.js';
import { ApprovalPrompt } from './approval.js';
import { Markdown } from './markdown/markdown.js';
import { Notice } from './notice.js';
import { ReasoningBlock } from './reasoning.js';

/**
 * How loud each risk band is.
 *
 * `exec` and `network` are the two an operator of a self-hosted agent actually
 * wants to see before they happen — which is also why they are the two whose
 * default policy is `ask` — so they get the two loudest tones.
 */
const RISK: Record<
  ToolRisk,
  { readonly label: string; readonly tone: BadgeProps['tone'] }
> = {
  safe: { label: 'read', tone: 'neutral' },
  write: { label: 'write', tone: 'info' },
  exec: { label: 'exec', tone: 'danger' },
  network: { label: 'network', tone: 'warning' },
};

interface ToolCardProps {
  readonly tool: ToolPart;
  readonly onApprove: (
    callId: string,
    approved: boolean,
    scope: ApprovalScope,
  ) => void;
}

export function ToolCard({ tool, onApprove }: ToolCardProps): JSX.Element {
  const { t } = useTranslation();
  const subagent = tool.subagent;
  // Open by default while a decision is needed, and opened when a subagent
  // starts working: a running delegation inside a collapsed card is a card that
  // says nothing for as long as the delegation takes. The approval half stays
  // an *initial* state and deliberately does not gain an effect — the prompt
  // itself renders outside this disclosure, so opening the body for one would
  // reveal only the arguments the header already summarises.
  //
  // The subagent half is sticky rather than derived, and that is the whole
  // subtlety. A derived `open` would *close* the card the moment the run ended
  // — swallowing both the run and the answer the reader was waiting for. This
  // opens on the edge and never closes on its own; a click always wins.
  const live = subagent !== undefined && !subagent.done;
  const [open, setOpen] = useState(tool.status === 'awaiting-approval');
  useEffect(() => {
    if (live) setOpen(true);
  }, [live]);
  const bodyId = useId();
  const elapsedMs = useElapsed(tool.status === 'running', tool.elapsedMs);
  const risk = RISK[tool.risk];

  const summary = summariseArgs(tool.args);

  return (
    <section
      // Named, which makes it a landmark: a screen-reader user navigating a
      // turn with six calls in it can move between them instead of reading
      // through them.
      aria-label={`Tool call: ${tool.name}`}
      className={cn(
        'tool-card',
        tool.status === 'awaiting-approval' && 'tool-card--awaiting',
      )}
    >
      <h4 className="contents">
        <button
          type="button"
          aria-expanded={open}
          aria-controls={bodyId}
          onClick={() => {
            setOpen((value) => !value);
          }}
          className="tool-card__header"
        >
          <ChevronRight
            className={cn(
              'tool-card__chevron disclosure-chevron',
              open && 'disclosure-chevron--open',
            )}
          />
          <StatusIcon status={tool.status} />
          <span className="tool-card__name">{tool.name}</span>
          {/* Labelled, because a pill reading `exec` is cryptic on its own —
              and because the band and the tool are often the same word. */}
          <Badge tone={risk.tone} aria-label={`Risk: ${risk.label}`}>
            {risk.label}
          </Badge>

          {summary !== '' && (
            <span className="tool-card__summary truncate">{summary}</span>
          )}

          <span className="tool-card__timing">
            {tool.status === 'running' && formatDuration(elapsedMs)}
            {tool.status === 'awaiting-approval' && 'waiting for you'}
            {tool.durationMs !== undefined && formatDuration(tool.durationMs)}
          </span>
        </button>
      </h4>

      {tool.approval !== undefined && (
        <ApprovalPrompt
          toolName={tool.name}
          approval={tool.approval}
          onAnswer={(approved, scope) => {
            onApprove(tool.id, approved, scope);
          }}
        />
      )}

      {tool.notices.length > 0 && (
        <div className="stack tool-card__notices">
          {tool.notices.map((notice) => (
            <Notice
              key={notice.id}
              kind={notice.notice}
              message={notice.message}
            />
          ))}
        </div>
      )}

      {/* Mounted only while open, rather than hidden. A conversation with
          thirty calls in it would otherwise carry thirty tool outputs — some of
          them tens of kilobytes — in the DOM to render nothing. */}
      {open && (
        <div id={bodyId} className="stack tool-card__body">
          {summary !== '' && (
            <Labelled label={t('tool.arguments')}>
              <pre className="tool-card__pre">{formatArgs(tool.args)}</pre>
            </Labelled>
          )}

          {subagent !== undefined && (
            <SubagentRun run={subagent} onApprove={onApprove} />
          )}

          {tool.content !== undefined && (
            <Labelled
              label={
                tool.status === 'error' ? t('tool.error') : t('tool.output')
              }
            >
              <Output content={tool.content} terminal={tool.name === 'exec'} />
              {tool.truncated && (
                <p className="tool-card__note">{t('tool.truncated')}</p>
              )}
            </Labelled>
          )}

          {tool.content === undefined && tool.status === 'running' && (
            <p className="tool-card__running">{t('tool.running')}</p>
          )}
        </div>
      )}
    </section>
  );
}

/**
 * A subagent's turn, inside the card of the call that started it.
 *
 * Depth reads as a raised inner surface and a hairline, not as an indent and not
 * as a shadow — the token layer has no shadow scale, and the answer it does have
 * for elevation is a surface plus a stroke. The label row names the agent so a
 * reader can tell whose `exec` they are approving, which is the one thing a
 * nested card must not leave ambiguous.
 */
function SubagentRun({
  run,
  onApprove,
}: {
  readonly run: SubagentPart;
  readonly onApprove: ToolCardProps['onApprove'];
}): JSX.Element {
  const { t } = useTranslation();

  // A run rebuilt from storage knows it happened and nothing else — its steps
  // are rows in the subagent's own session. Fetched here rather than folded
  // into the transcript because it is view state: it is wanted only while the
  // card is open, and this component is mounted only then.
  const fetched = useQuery({
    queryKey: queryKeys.messages(run.sessionKey),
    queryFn: ({ signal }) => api.messages(run.sessionKey, signal),
    enabled: !run.loaded,
    // A subagent session that has been deleted is a 404, and an expected one:
    // retrying it three times to show the same empty card helps nobody.
    retry: false,
  });

  const parts = run.loaded
    ? run.parts
    : partsOf(fetched.data?.messages ?? [], fetched.data?.subagentRuns ?? {});

  // Three states, named rather than derived from the query's flags at each use.
  // A *disabled* query reports `isPending` forever — it is waiting to be
  // allowed to run, not waiting for an answer — so `!fetched.isPending` as a
  // stand-in for "we know what is here" is false for every live run.
  const state = run.loaded
    ? 'ready'
    : fetched.isError
      ? 'unavailable'
      : fetched.isSuccess
        ? 'ready'
        : 'loading';

  return (
    <section
      className="stack subagent"
      aria-label={t('tool.subagentRun', { agent: run.label })}
    >
      <p className="subagent__header">
        <Bot className="subagent__icon" aria-hidden="true" />
        <span className="subagent__name">{run.label}</span>
        {run.model !== '' && (
          <span className="subagent__model">{run.model}</span>
        )}
        {!run.done && (
          <span className="subagent__status">{t('tool.subagentRunning')}</span>
        )}
      </p>

      {state === 'loading' && (
        <p className="tool-card__note">{t('tool.subagentLoading')}</p>
      )}
      {state === 'unavailable' && (
        <p className="tool-card__note">{t('tool.subagentUnavailable')}</p>
      )}
      {state === 'ready' && run.done && parts.length === 0 && (
        <p className="tool-card__note">{t('tool.subagentEmpty')}</p>
      )}

      <TurnParts parts={parts} streaming={!run.done} onApprove={onApprove} />
    </section>
  );
}

/**
 * A subagent session's stored rows as the parts of one run.
 *
 * Its opening user message is dropped: it is the task, which the delegating
 * card already shows as its argument, and repeating it inside the run would
 * make every delegation read as though it had been asked twice.
 *
 * The runs are passed through because a subagent may itself have delegated, and
 * they are what a nested `ask_*` call needs to become a card that offers to
 * fetch its own run rather than a bare tool result. Without them the recursion
 * stopped at the first level that had to be rebuilt from storage.
 */
function partsOf(
  messages: readonly StoredMessage[],
  subagentRuns: Readonly<Record<string, SubagentRunRef>>,
): readonly TurnPart[] {
  return fromStoredMessages(messages, subagentRuns).flatMap((item) =>
    item.kind === 'turn' ? item.parts : [],
  );
}

/**
 * A list of parts, in arrival order.
 *
 * One implementation for a turn and for a delegation, which is what makes a
 * nested `exec` card identical to a top-level one — including its approval
 * prompt, which is answered by the same frame wherever it is drawn.
 */
export function TurnParts({
  parts,
  streaming,
  onApprove,
  unanswered = false,
}: {
  readonly parts: readonly TurnPart[];
  /** True while the list is still growing — only its last part can be live. */
  readonly streaming: boolean;
  readonly onApprove: ToolCardProps['onApprove'];
  /** Expands reasoning on a turn that produced nothing else. */
  readonly unanswered?: boolean;
}): JSX.Element {
  const last = parts.at(-1);
  const hasAnswer = parts.some((part) => part.kind === 'text');

  return (
    <>
      {parts.map((part) => {
        const live = streaming && part === last;

        switch (part.kind) {
          case 'text':
            return <Markdown key={part.id} text={part.text} streaming={live} />;

          case 'reasoning':
            return (
              <ReasoningBlock
                key={part.id}
                text={part.text}
                live={live && !hasAnswer}
                expanded={unanswered}
              />
            );

          case 'tool':
            return <ToolCard key={part.id} tool={part} onApprove={onApprove} />;

          case 'notice':
            return (
              <Notice key={part.id} kind={part.notice} message={part.message} />
            );
        }
      })}
    </>
  );
}

function Labelled({
  label,
  children,
}: {
  readonly label: string;
  readonly children: React.ReactNode;
}): JSX.Element {
  return (
    <div className="stack tool-card__field">
      <span className="tool-card__label">{label}</span>
      {children}
    </div>
  );
}

/** `exec` output is a terminal; everything else is a code block. */
function Output({
  content,
  terminal,
}: {
  readonly content: string;
  readonly terminal: boolean;
}): JSX.Element {
  return (
    <pre
      className={cn(
        'tool-card__pre tool-card__output',
        terminal && 'tool-card__output--terminal',
      )}
    >
      {content === '' ? (
        <span className="tool-card__empty">(no output)</span>
      ) : (
        content
      )}
    </pre>
  );
}

function StatusIcon({
  status,
}: {
  readonly status: ToolPart['status'];
}): JSX.Element {
  const { t } = useTranslation();
  switch (status) {
    case 'running':
      return (
        <Loader2
          className="tool-card__status tool-card__status--running"
          aria-label={t('tool.runningLabel')}
        />
      );
    case 'awaiting-approval':
      return (
        <Terminal
          className="tool-card__status tool-card__status--awaiting"
          aria-label={t('tool.needsApproval')}
        />
      );
    case 'ok':
      return (
        <CheckCircle2
          className="tool-card__status tool-card__status--ok"
          aria-label={t('tool.succeeded')}
        />
      );
    case 'error':
      return (
        <AlertCircle
          className="tool-card__status tool-card__status--error"
          aria-label={t('tool.failed')}
        />
      );
    default:
      return <Wrench className="tool-card__status" />;
  }
}

/**
 * Elapsed milliseconds, ticking while `active`.
 *
 * `floorMs` is the server's last heartbeat, and it wins whenever it is ahead —
 * a tab that was backgrounded had its timers throttled, and the server's figure
 * is the true one.
 */
function useElapsed(active: boolean, floorMs: number): number {
  const [startedAtMs] = useState(() => Date.now());
  const [nowMs, setNowMs] = useState(startedAtMs);

  useEffect(() => {
    if (!active) return undefined;
    const timer = setInterval(() => {
      setNowMs(Date.now());
    }, 1000);
    return () => {
      clearInterval(timer);
    };
  }, [active]);

  return Math.max(nowMs - startedAtMs, floorMs);
}
