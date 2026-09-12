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

import { AlertCircle } from 'lucide-react';
import type { WebKey } from '@/i18n/keys.js';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { ApprovalScope, Attachment } from '@ghostwire/protocol';

import { cn } from '@/lib/cn.js';
import { useFormat } from '@/lib/use-format.js';
import { Button } from '@/components/ui/button.js';
import { AutoGrowTextarea } from '@/components/auto-grow-textarea.js';
import { useWorkspace } from '@/workspaces/workspace-context.js';
import type { TranscriptItem, TurnItem, UserItem } from '@/state/transcript.js';
import { AttachmentList } from './attachment-list.js';
import { MessageActions } from './message-actions.js';
import { Notice } from './notice.js';
import { TurnParts } from './tool-card.js';
import { TurnInfo } from './turn-info.js';

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
  readonly sessionKey: string | undefined;
  readonly onApprove: (
    callId: string,
    approved: boolean,
    scope: ApprovalScope,
  ) => void;
  readonly onAction: (action: MessageAction) => void;
}

export function Message({
  item,
  streaming,
  busy,
  sessionKey,
  onApprove,
  onAction,
}: MessageProps): JSX.Element {
  const { t } = useTranslation();

  switch (item.kind) {
    case 'user':
      return <UserMessage item={item} busy={busy} onAction={onAction} />;
    case 'turn':
      return (
        <TurnMessage
          turn={item}
          streaming={streaming}
          busy={busy}
          sessionKey={sessionKey}
          onApprove={onApprove}
          onAction={onAction}
        />
      );
    case 'steer':
      return (
        <p className="steer">{t('chat.steeredMidTurn', { text: item.text })}</p>
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
          Sending…
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
  sessionKey,
  onApprove,
  onAction,
}: {
  readonly turn: TurnItem;
  readonly streaming: boolean;
  readonly busy: boolean;
  readonly sessionKey: string | undefined;
  readonly onApprove: MessageProps['onApprove'];
  readonly onAction: (action: MessageAction) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const hasReasoning = turn.parts.some((part) => part.kind === 'reasoning');
  const unanswered = isUnanswered(turn);

  return (
    <article className="stack turn">
      <TurnParts
        parts={turn.parts}
        // Only the trailing part of an unfinished turn is still growing.
        streaming={streaming && !turn.done}
        onApprove={onApprove}
        unanswered={unanswered}
      />

      {streaming && !turn.done && turn.parts.length === 0 && (
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

      <TurnFooter
        turn={turn}
        busy={busy}
        sessionKey={sessionKey}
        onAction={onAction}
      />
    </article>
  );
}

/**
 * A finished turn that produced no answer and called no tool.
 *
 * The failure it exists for is a small local model — or any reasoning model with
 * a low token cap — that spends its whole response in the reasoning channel and
 * returns empty content with no tool calls. `loop.ts` has nothing to continue on,
 * so it ends the turn as `complete`; the transcript then holds a single reasoning
 * part, which renders as a collapsed strip above a footer. The user sees what
 * looks like an empty message and is given no reason for it, which reads as the
 * app having lost the answer rather than the model never having written one.
 *
 * Three exclusions, each because something else already explains the silence:
 *
 *  - **A failure** prints its own message, and two lines saying the turn went
 *    wrong is one too many.
 *  - **`aborted`** means the user pressed Stop. An answer is missing because
 *    they asked for it to be, and the footer already says "Stopped."
 *  - **`max_iterations` / `wall_timeout`** are unreachable here, because
 *    `loop.ts` appends a sentence explaining itself and that sentence is a text
 *    part. Named anyway, so a future path that stops without writing one does
 *    not start claiming the model said nothing.
 *
 * A turn with no parts at all satisfies this too, which is correct: an empty
 * `<article>` and a footer is the same non-explanation.
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
 * A footer on every finished turn saying "complete" is a line of noise on every
 * message. The three that are worth a sentence are the ones where the answer
 * stops short of what was asked, and the user is otherwise left to infer it
 * from an answer that simply ends.
 */
const STOP_REASONS: Record<string, WebKey> = {
  aborted: 'chat.stopReasons.aborted',
  max_iterations: 'chat.stopReasons.max_iterations',
  wall_timeout: 'chat.stopReasons.wall_timeout',
  error: 'chat.stopReasons.error',
};

function TurnFooter({
  turn,
  busy,
  sessionKey,
  onAction,
}: {
  readonly turn: TurnItem;
  readonly busy: boolean;
  readonly sessionKey: string | undefined;
  readonly onAction: (action: MessageAction) => void;
}): JSX.Element | null {
  // Before the `return null` below: a hook may not be called conditionally, and
  // this component has an early exit.
  const { t } = useTranslation();
  const fmt = useFormat();

  // The footer now always renders on a finished turn, because it carries the
  // action bar. It is still one footer rather than two — a row of buttons under
  // a row of metadata would be two things saying "this turn is over".
  if (!turn.done) return null;

  // The key, then the sentence. `complete` is absent from the map on purpose,
  // so an unlisted reason stays `undefined` and renders no footer line at all
  // rather than resolving to a missing key.
  const reasonKey =
    turn.stopReason === undefined ? undefined : STOP_REASONS[turn.stopReason];
  const reason = reasonKey === undefined ? undefined : t(reasonKey);
  const usage = turn.usage;
  // Narrowed here rather than asserted inside the callbacks below: a `!` in a
  // closure is a claim the compiler cannot check at the point it runs.
  const { firstSeq, lastSeq } = turn;
  const text = turn.parts
    .filter((part) => part.kind === 'text')
    .map((part) => part.text)
    .join('\n\n');

  return (
    <footer className="turn__footer">
      {reason !== undefined && (
        <span className="turn__stop-reason">{reason}</span>
      )}
      {usage !== undefined && (
        <span className="turn__usage">
          {fmt.tokens(usage.promptTokens)} in ·{' '}
          {fmt.tokens(usage.completionTokens)} out
          {usage.cachedTokens !== undefined &&
            usage.cachedTokens > 0 &&
            ` · ${fmt.tokens(usage.cachedTokens)} cached`}
        </span>
      )}
      {turn.iterations > 1 && <span>{turn.iterations} iterations</span>}
      {turn.model !== '' && <span className="truncate">{turn.model}</span>}

      <span className="spacer" />

      <div className="turn__actions">
        <MessageActions
          text={text}
          busy={busy}
          info={<TurnInfo turn={turn} sessionKey={sessionKey} />}
          {...(firstSeq === undefined
            ? {}
            : {
                onRegenerate: () => {
                  onAction({ kind: 'regenerate', seq: firstSeq });
                },
              })}
          {...(lastSeq === undefined
            ? {}
            : {
                // After the answer: branching from a turn means continuing from
                // what it said, down a different path.
                onBranch: () => {
                  onAction({ kind: 'branch', seq: lastSeq });
                },
              })}
        />
      </div>
    </footer>
  );
}
