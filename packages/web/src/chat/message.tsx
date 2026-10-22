/**
 * One transcript item.
 *
 * The user's side is a bubble; the agent's side is not. That asymmetry is
 * deliberate and it is what the layout is for: a user message is short, bounded
 * and belongs to a person, so a right-aligned bubble reads correctly. An answer
 * is long, contains code blocks and tables, and is the page's actual content —
 * putting it in a bubble would give it a container that fights every wide thing
 * inside it and a width nobody wants to read at.
 *
 * A turn renders its parts in arrival order, which is the whole reason
 * `transcript.ts` keeps them as a list: text, then a tool card, then more text
 * is a shape a `{ text, tools[] }` object cannot express, and reordering it
 * would make the answer describe a call that appears below it.
 */

import {
  AlertCircle,
  Copy,
  GitBranch,
  Info,
  MoreHorizontal,
  RefreshCw,
} from 'lucide-react';
import type { WebKey } from '@/i18n/keys.js';
import { memo, useRef, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import {
  turnRate,
  type ApprovalScope,
  type Attachment,
  type ExecRule,
  type StepStats,
} from '@darkwire/protocol';

import { cn } from '@/lib/cn.js';
import { formatDuration } from '@/lib/format.js';
import { useFormat } from '@/lib/use-format.js';
import { Button } from '@/components/ui/button.js';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.js';
import {
  Popover,
  PopoverAnchor,
  PopoverContent,
} from '@/components/ui/popover.js';
import { useCopy } from '@/components/use-copy.js';
import { AutoGrowTextarea } from '@/components/auto-grow-textarea.js';
import { useWorkspace } from '@/workspaces/workspace-context.js';
import {
  messagesOf,
  type TranscriptItem,
  type TurnItem,
  type TurnPiece,
  type UserItem,
} from '@/state/transcript.js';
import { AttachmentList } from './attachment-list.js';
import { MessageActions } from './message-actions.js';
import { Notice } from './notice.js';
import { TurnParts } from './tool-card.js';
import { MessageFigures, MessageInfo } from './message-info.js';

/**
 * What the transcript asks the route to do, as one callback rather than four
 * props threaded through two components.
 *
 * Every variant names a `seq`, because every one of them addresses a point in
 * stored history — which is why the actions that need one are not offered until
 * the message has it.
 */
export type MessageAction =
  | {
      readonly kind: 'edit';
      readonly seq: number;
      readonly text: string;
      /**
       * The message's existing attachments, carried through unchanged.
       *
       * An edit replaces the stored message, so anything left out of it is
       * deleted. Omitting these silently stripped every attachment from a
       * message whose wording was corrected — the editor has no attachment
       * affordance, so there was no way to notice until the answer came back
       * without them.
       */
      readonly attachments: readonly Attachment[];
    }
  | { readonly kind: 'regenerate'; readonly seq: number }
  | { readonly kind: 'branch'; readonly seq: number };

interface MessageProps {
  readonly item: TranscriptItem;
  /** True for the newest turn while the session is busy. */
  readonly streaming: boolean;
  /** True while any turn is running — what disables everything destructive. */
  readonly busy: boolean;
  readonly onApprove: (
    callId: string,
    approved: boolean,
    scope: ApprovalScope,
    rule?: ExecRule,
  ) => void;
  readonly onAction: (action: MessageAction) => void;
}

/**
 * Memoised because every delta hands the transcript a new array, and only the
 * item it grew is a new object. The rest keep their identity and skip.
 */
export const Message = memo(MessageItem);

function MessageItem({
  item,
  streaming,
  busy,
  onApprove,
  onAction,
}: MessageProps): JSX.Element {
  switch (item.kind) {
    case 'user':
      return <UserMessage item={item} busy={busy} onAction={onAction} />;
    case 'turn':
      return (
        <TurnMessage
          turn={item}
          streaming={streaming}
          busy={busy}
          onApprove={onApprove}
          onAction={onAction}
        />
      );
    case 'notice':
      return <Notice kind={item.notice} message={item.message} />;
  }
}

function UserMessage({
  item,
  busy,
  onAction,
}: {
  readonly item: UserItem;
  readonly busy: boolean;
  readonly onAction: (action: MessageAction) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const { workspaceId } = useWorkspace();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(item.text);

  // Until storage has answered for this message there is nothing to address,
  // so the actions that name a `seq` are not offered rather than offered and
  // broken.
  const seq = item.seq;

  if (editing && seq !== undefined) {
    return (
      <div className="stack message-user">
        <form
          className="stack message-user__editor"
          onSubmit={(event) => {
            event.preventDefault();
            if (draft.trim() === '') return;
            setEditing(false);
            onAction({
              kind: 'edit',
              seq,
              text: draft,
              attachments: item.attachments,
            });
          }}
        >
          <AutoGrowTextarea
            aria-label={t('chat.editMessage')}
            value={draft}
            autoFocus
            onChange={(event) => {
              setDraft(event.target.value);
            }}
            onKeyDown={(event) => {
              if (event.key === 'Escape') {
                setDraft(item.text);
                setEditing(false);
                return;
              }
              // Cmd/Ctrl+Enter saves; a bare Enter is a newline, because an
              // edit is usually of something long enough to have needed one.
              if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
                event.preventDefault();
                if (draft.trim() === '') return;
                setEditing(false);
                onAction({
                  kind: 'edit',
                  seq,
                  text: draft,
                  attachments: item.attachments,
                });
              }
            }}
          />
          <div className="cluster message-user__editor-actions">
            <Button type="submit" size="sm" disabled={draft.trim() === ''}>
              Save
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={() => {
                setDraft(item.text);
                setEditing(false);
              }}
            >
              Cancel
            </Button>
          </div>
        </form>
      </div>
    );
  }

  return (
    <div className="stack message-user">
      <div
        className={cn(
          'message-user__bubble',
          item.pending && 'message-user__bubble--pending',
        )}
      >
        {item.text}
      </div>

      <AttachmentList attachments={item.attachments} workspace={workspaceId} />

      {item.pending && (
        <span className="message-user__pending" role="status">
          {item.steering ? t('chat.steerPending') : 'Sending…'}
        </span>
      )}

      {!item.pending && (
        <div className="message-user__actions">
          <MessageActions
            text={item.text}
            busy={busy}
            {...(seq === undefined
              ? {}
              : {
                  onEdit: () => {
                    setDraft(item.text);
                    setEditing(true);
                  },
                  // Before the question: branching from something you said means
                  // asking it again down a different path.
                  onBranch: () => {
                    onAction({ kind: 'branch', seq: seq - 1 });
                  },
                })}
          />
        </div>
      )}
    </div>
  );
}

function TurnMessage({
  turn,
  streaming,
  busy,
  onApprove,
  onAction,
}: {
  readonly turn: TurnItem;
  readonly streaming: boolean;
  readonly busy: boolean;
  readonly onApprove: MessageProps['onApprove'];
  readonly onAction: (action: MessageAction) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const hasReasoning = turn.parts.some((part) => part.kind === 'reasoning');
  const unanswered = isUnanswered(turn);
  const pieces = messagesOf(turn.parts);
  const last = pieces.at(-1);
  const lastMessage = pieces.findLast((piece) => piece.kind === 'message');
  // Said once, at the start of the turn's last line.
  const reason =
    turn.done && turn.stopReason !== undefined
      ? STOP_REASONS[turn.stopReason]
      : undefined;

  /**
   * What a message's line acts on, or nothing while it is still being written.
   *
   * Its own seqs once `step.end` named them. The last message of a finished
   * turn falls back to the turn's: one stopped or failed before its message
   * ended, and rows stored before they carried figures, still end in a line
   * that can be re-run and branched.
   */
  const lineOf = (
    piece: Extract<TurnPiece, { kind: 'message' }>,
  ): Line | undefined => {
    const closing = turn.done && piece === lastMessage;
    if (piece.end === undefined && !closing) return undefined;
    return {
      number: piece.number,
      text: piece.text,
      stats: piece.end?.stats,
      firstSeq: piece.end?.firstSeq ?? turn.firstSeq,
      lastSeq: closing
        ? (turn.lastSeq ?? piece.end?.lastSeq)
        : piece.end?.lastSeq,
      reason: closing ? reason : undefined,
    };
  };

  return (
    <article className="stack turn">
      {pieces.map((piece) => {
        if (piece.kind === 'steer') {
          return (
            <TurnParts
              key={piece.part.id}
              parts={[piece.part]}
              streaming={false}
              onApprove={onApprove}
            />
          );
        }
        const line = lineOf(piece);
        return (
          <div key={piece.key} className="stack turn__message">
            <TurnParts
              parts={piece.parts}
              // Only the trailing part of an unfinished turn is still growing.
              streaming={streaming && !turn.done && piece === last}
              onApprove={onApprove}
              unanswered={unanswered}
            />
            {line !== undefined && (
              <MessageLine line={line} busy={busy} onAction={onAction} />
            )}
          </div>
        );
      })}

      {streaming && !turn.done && isWorking(turn) && (
        <p className="turn__thinking" role="status">
          <span className="thinking-dots" aria-hidden="true">
            <span />
            <span />
            <span />
          </span>{' '}
          {t('chat.thinking')}
        </p>
      )}

      {turn.failure !== undefined && (
        <p className="turn__failure">
          <AlertCircle />
          <span>
            {turn.failure.message}
            {turn.failure.retryable && ' Sending the message again may work.'}
          </span>
        </p>
      )}

      {unanswered && (
        <p className="turn__unanswered">
          <AlertCircle />
          <span>
            {t(
              hasReasoning
                ? 'chat.noAnswer.reasoningOnly'
                : 'chat.noAnswer.silent',
            )}
          </span>
        </p>
      )}

      {/* A turn that ended before it wrote anything still gets its line, so
          it can be re-run. */}
      {turn.done && lastMessage === undefined && (
        <div className="stack turn__message">
          <MessageLine
            line={{
              number: 1,
              text: '',
              stats: undefined,
              firstSeq: turn.firstSeq,
              lastSeq: turn.lastSeq,
              reason,
            }}
            busy={busy}
            onAction={onAction}
          />
        </div>
      )}
    </article>
  );
}

/** What one message's line shows and acts on. */
interface Line {
  /** From 1, among the turn's messages. */
  readonly number: number;
  /** What Copy takes. */
  readonly text: string;
  /** Absent on a message that ended without `step.end` naming it. */
  readonly stats: StepStats | undefined;
  /** Regenerate re-runs from here. */
  readonly firstSeq: number | undefined;
  /** Branch copies up to here. */
  readonly lastSeq: number | undefined;
  /** Why the turn stopped, on its last line only. */
  readonly reason: WebKey | undefined;
}

/**
 * The line under a message: how long it took, what it wrote and how fast, and
 * what can be done to it.
 *
 * Every message gets the same one, the last included, and nothing is added up
 * for the turn. Regenerate re-runs from just before the message, keeping what
 * came earlier. Branch copies the conversation up to its end, and works while
 * the turn is still running because those rows are final.
 *
 * A pointer gets the actions on hover. A touch screen has no hover, and four
 * buttons after every message is most of what a phone would show, so it gets
 * the row menu the lists use. Both are rendered and the stylesheet keeps one.
 */
function MessageLine({
  line,
  busy,
  onAction,
}: {
  readonly line: Line;
  readonly busy: boolean;
  readonly onAction: (action: MessageAction) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const fmt = useFormat();
  const { number, stats, firstSeq, lastSeq } = line;

  const rate = stats === undefined ? undefined : turnRate(stats.usage, stats);
  const figures = [
    ...(stats === undefined
      ? []
      : [
          formatDuration(stats.elapsedMs),
          t('turn.message.out', {
            out: fmt.tokens(stats.usage.completionTokens),
          }),
        ]),
    ...(rate === undefined
      ? []
      : [t('turn.message.rate', { rate: fmt.tokens(Math.round(rate)) })]),
  ].join(' · ');

  const onRegenerate =
    firstSeq === undefined
      ? undefined
      : () => {
          onAction({ kind: 'regenerate', seq: firstSeq });
        };
  const onBranch =
    lastSeq === undefined
      ? undefined
      : () => {
          onAction({ kind: 'branch', seq: lastSeq });
        };

  return (
    <footer className="turn__line">
      {(line.reason !== undefined || figures !== '') && (
        <span className="turn__figures">
          {line.reason !== undefined && (
            <span className="turn__stop-reason">{t(line.reason)}</span>
          )}
          {line.reason !== undefined && figures !== '' && ' · '}
          {figures}
        </span>
      )}

      <div className="turn__line-actions">
        <MessageActions
          className="message-actions--line"
          text={line.text}
          busy={busy}
          copyLabel={t('turn.message.copy', { number })}
          regenerateLabel={t('turn.message.regenerate', { number })}
          branchLabel={t('turn.message.branch', { number })}
          onRegenerate={onRegenerate}
          onBranch={onBranch}
          info={
            stats === undefined ? undefined : (
              <MessageInfo stats={stats} number={number} />
            )
          }
        />
        <div className="turn__line-more">
          <MessageMenu
            number={number}
            stats={stats}
            text={line.text}
            busy={busy}
            onRegenerate={onRegenerate}
            onBranch={onBranch}
          />
        </div>
      </div>
    </footer>
  );
}

/**
 * The same actions as a menu, for a screen with no hover.
 *
 * Details is a menu item that opens the figures where the menu was. The menu
 * hands focus back to its button as it closes, which the popover would read as
 * focus leaving it and close at once, so that hand-back is skipped for it.
 */
function MessageMenu({
  number,
  stats,
  text,
  busy,
  onRegenerate,
  onBranch,
}: {
  readonly number: number;
  readonly stats: StepStats | undefined;
  readonly text: string;
  readonly busy: boolean;
  readonly onRegenerate: (() => void) | undefined;
  readonly onBranch: (() => void) | undefined;
}): JSX.Element {
  const { t } = useTranslation();
  const { copy } = useCopy(text);
  const [details, setDetails] = useState(false);
  const opening = useRef(false);

  return (
    <Popover open={details} onOpenChange={setDetails}>
      <DropdownMenu>
        <PopoverAnchor asChild>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="icon"
              aria-label={t('turn.message.more', { number })}
            >
              <MoreHorizontal />
            </Button>
          </DropdownMenuTrigger>
        </PopoverAnchor>
        <DropdownMenuContent
          align="end"
          className="floating--menu"
          onCloseAutoFocus={(event) => {
            if (!opening.current) return;
            opening.current = false;
            event.preventDefault();
          }}
        >
          <DropdownMenuItem onSelect={copy}>
            <Copy />
            {t('turn.menu.copy')}
          </DropdownMenuItem>
          {onRegenerate !== undefined && (
            <DropdownMenuItem disabled={busy} onSelect={onRegenerate}>
              <RefreshCw />
              {t('turn.menu.regenerate')}
            </DropdownMenuItem>
          )}
          {onBranch !== undefined && (
            <DropdownMenuItem onSelect={onBranch}>
              <GitBranch />
              {t('chat.branchHere')}
            </DropdownMenuItem>
          )}
          {stats !== undefined && (
            <DropdownMenuItem
              onSelect={() => {
                opening.current = true;
                setDetails(true);
              }}
            >
              <Info />
              {t('turn.menu.details')}
            </DropdownMenuItem>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      {stats !== undefined && (
        <PopoverContent align="end" className="popover--message-info">
          <MessageFigures stats={stats} />
        </PopoverContent>
      )}
    </Popover>
  );
}

/**
 * A running turn with nothing on screen that is moving.
 *
 * The indicator used to appear only while a turn had no parts at all, which is
 * true for about as long as it takes the first token to arrive. After that it
 * never came back, so the gap between a tool finishing and the next thing
 * starting showed a finished card, a static sentence under the composer and a
 * red Stop button, and nothing anywhere saying the turn was still running. On
 * an agent that runs forty tools that gap is most of the turn.
 *
 * Two shapes qualify, and between them they are every moment when the model is
 * working and the transcript is not:
 *
 *  - **Nothing yet.** The first token has not arrived.
 *  - **The last part has finished.** A tool card that says `ok` or `error` is
 *    not going to change again, and the next `tool.call` or text delta has not
 *    landed. The model is generating in between. The end of a message is the
 *    same moment, written down.
 *
 * Everything else already has something of its own that moves: streaming text
 * grows, a running tool card spins and counts, an approval prompt is waiting on
 * the reader rather than on the model.
 */
function isWorking(turn: TurnItem): boolean {
  const last = turn.parts.at(-1);
  if (last === undefined) return true;
  // A message that just ended, or a steer just read, is the model starting
  // the next one.
  if (last.kind === 'step' || last.kind === 'steer') return true;
  return (
    last.kind === 'tool' && (last.status === 'ok' || last.status === 'error')
  );
}

/**
 * A finished turn that produced no answer and called no tool.
 *
 * The failure it exists for is a small local model — or any reasoning model with
 * a low token cap — that spends its whole response in the reasoning channel and
 * returns empty content with no tool calls. `loop.ts` has nothing to continue on,
 * so it ends the turn as `complete`; the transcript then holds a single reasoning
 * part, which renders as a collapsed strip above a line. The user sees what
 * looks like an empty message and is given no reason for it, which reads as the
 * app having lost the answer rather than the model never having written one.
 *
 * Three exclusions, each because something else already explains the silence:
 *
 *  - **A failure** prints its own message, and two lines saying the turn went
 *    wrong is one too many.
 *  - **`aborted`** means the user pressed Stop. An answer is missing because
 *    they asked for it to be, and the last line already says so.
 *  - **`max_iterations` / `wall_timeout`** are unreachable here, because
 *    `loop.ts` appends a sentence explaining itself and that sentence is a text
 *    part. Named anyway, so a future path that stops without writing one does
 *    not start claiming the model said nothing.
 *
 * A turn with no parts at all satisfies this too, which is correct: an empty
 * `<article>` and a line is the same non-explanation.
 */
function isUnanswered(turn: TurnItem): boolean {
  if (!turn.done || turn.failure !== undefined) return false;
  if (turn.stopReason === 'aborted') return false;
  if (
    turn.stopReason === 'max_iterations' ||
    turn.stopReason === 'wall_timeout'
  ) {
    return false;
  }
  return !turn.parts.some(
    (part) => part.kind === 'text' || part.kind === 'tool',
  );
}

/**
 * The stop reason, and only when it is not `complete`.
 *
 * "Complete" at the end of every turn is noise on every turn. The ones worth
 * saying are those where the answer stops short of what was asked, and the
 * user is otherwise left to infer it from an answer that simply ends.
 */
const STOP_REASONS: Record<string, WebKey> = {
  aborted: 'chat.stopReasons.aborted',
  max_iterations: 'chat.stopReasons.max_iterations',
  wall_timeout: 'chat.stopReasons.wall_timeout',
  error: 'chat.stopReasons.error',
};
