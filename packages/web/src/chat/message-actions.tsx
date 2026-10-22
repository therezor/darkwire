/**
 * What you can do to a message once it is on screen.
 *
 * Which actions belong on which side is not symmetry, it is what the action
 * means:
 *
 *  - **Edit** is only ever on a user message. You did not write the answer, and
 *    a transcript you can rewrite the model's half of is a transcript that
 *    proves nothing.
 *  - **Regenerate** is only ever on a turn. Re-rolling your own sentence is not
 *    a thing — that is what Edit is.
 *  - **Branch** is on both, and means "fork here": before the question on a user
 *    message, after the answer on a turn.
 *  - **Info** is only on a turn, because only a turn has a cost.
 *  - **Copy** is on both, and is the one action that asks the server nothing.
 *
 * Every message of a turn gets the same bar. Regenerate there re-runs from
 * just before that message and keeps what came earlier.
 *
 * Edit and Regenerate are disabled while a turn is running. Branch is not: it
 * forks into a new session and leaves this one alone, and the server refuses a
 * point the running turn has not finished. Everything that names a message is
 * offered only once that message has a `seq`. An optimistic bubble has no
 * address in storage yet, and the actions that need one cannot be honestly
 * offered before it lands.
 *
 * The bar reveals on hover with `opacity` and `:focus-within` rather than
 * `display: none`, so the buttons keep their place in the tab order. See
 * `chat.css` — a keyboard user has no pointer to hover with.
 */

import { Check, Copy, GitBranch, Pencil, RefreshCw } from 'lucide-react';
import type { JSX, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';

import { cn } from '@/lib/cn.js';
import { Button } from '@/components/ui/button.js';
import { useCopy } from '@/components/use-copy.js';

interface MessageActionsProps {
  /** What Copy puts on the clipboard. */
  readonly text: string;
  readonly onEdit?: (() => void) | undefined;
  readonly onRegenerate?: (() => void) | undefined;
  readonly onBranch?: (() => void) | undefined;
  /** The turn-details popover, rendered as-is. Absent on a user message. */
  readonly info?: ReactNode | undefined;
  /** True while a turn is running: nothing here may start a second one. */
  readonly busy: boolean;
  /**
   * Names for the buttons, where the defaults would repeat: a turn of ten
   * messages has ten of each, and a reader needs to know which one they are on.
   */
  readonly copyLabel?: string | undefined;
  readonly regenerateLabel?: string | undefined;
  readonly branchLabel?: string | undefined;
  readonly className?: string | undefined;
}

export function MessageActions({
  text,
  onEdit,
  onRegenerate,
  onBranch,
  info,
  busy,
  copyLabel,
  regenerateLabel,
  branchLabel,
  className,
}: MessageActionsProps): JSX.Element {
  const { t } = useTranslation();
  const { copied, copy } = useCopy(text);

  return (
    <div className={cn('cluster message-actions', className)}>
      <Button
        variant="ghost"
        size="icon"
        aria-label={copied ? 'Copied' : (copyLabel ?? 'Copy message')}
        onClick={copy}
      >
        {copied ? <Check /> : <Copy />}
      </Button>

      {onEdit !== undefined && (
        <Button
          variant="ghost"
          size="icon"
          aria-label={t('chat.editThis')}
          disabled={busy}
          onClick={onEdit}
        >
          <Pencil />
        </Button>
      )}

      {onRegenerate !== undefined && (
        <Button
          variant="ghost"
          size="icon"
          aria-label={regenerateLabel ?? t('chat.regenerate')}
          disabled={busy}
          onClick={onRegenerate}
        >
          <RefreshCw />
        </Button>
      )}

      {onBranch !== undefined && (
        <Button
          variant="ghost"
          size="icon"
          aria-label={branchLabel ?? t('chat.branchHere')}
          onClick={onBranch}
        >
          <GitBranch />
        </Button>
      )}

      {info}
    </div>
  );
}
