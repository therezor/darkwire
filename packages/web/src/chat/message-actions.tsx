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
 * Everything destructive is disabled while a turn is running, and everything
 * that names a message is disabled until that message has a `seq` — an
 * optimistic bubble has no address in storage yet, and the actions that need one
 * cannot be honestly offered before it lands.
 *
 * The bar reveals on hover with `opacity` and `:focus-within` rather than
 * `display: none`, so the buttons keep their place in the tab order. See
 * `chat.css` — a keyboard user has no pointer to hover with.
 */

import { Check, Copy, GitBranch, Pencil, RefreshCw } from 'lucide-react';
import type { JSX, ReactNode } from 'react';
import { useTranslation } from 'react-i18next';

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
}

export function MessageActions({
  text,
  onEdit,
  onRegenerate,
  onBranch,
  info,
  busy,
}: MessageActionsProps): JSX.Element {
  const { t } = useTranslation();
  const { copied, copy } = useCopy(text);

  return (
    <div className="cluster message-actions">
      <Button
        variant="ghost"
        size="icon"
        aria-label={copied ? 'Copied' : 'Copy message'}
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
          aria-label={t('chat.regenerate')}
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
          aria-label={t('chat.branchHere')}
          disabled={busy}
          onClick={onBranch}
        >
          <GitBranch />
        </Button>
      )}

      {info}
    </div>
  );
}
