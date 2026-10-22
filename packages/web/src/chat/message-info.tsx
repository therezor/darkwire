/**
 * One message's figures, behind the info button on its line.
 *
 * The line under a message already says how long it took, what it wrote and
 * how fast. This is the rest: the model the provider reported, what it read,
 * how much of that was cached, and how long the first token took.
 */

import { Info } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { StepStats } from '@darkwire/protocol';

import { formatDuration } from '@/lib/format.js';
import { useFormat } from '@/lib/use-format.js';
import { Button } from '@/components/ui/button.js';
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover.js';

export function MessageInfo({
  stats,
  number,
}: {
  readonly stats: StepStats;
  readonly number: number;
}): JSX.Element {
  const { t } = useTranslation();

  return (
    <Popover>
      {/* The `<Button>` is written out here rather than wrapped in a component
          of its own. `asChild` clones this element to inject `onClick`, the
          ref and `aria-expanded`, and a component that does not spread its
          props swallows every one of them: a trigger that renders perfectly
          and opens nothing. */}
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          aria-label={t('turn.message.details', { number })}
        >
          <Info />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="popover--message-info">
        <MessageFigures stats={stats} />
      </PopoverContent>
    </Popover>
  );
}

/** One message's figures, as a list. */
export function MessageFigures({
  stats,
}: {
  readonly stats: StepStats;
}): JSX.Element {
  const { t } = useTranslation();
  const fmt = useFormat();
  const { usage, model, firstTokenMs } = stats;

  return (
    <dl className="message-info">
      {model !== undefined && <Row label={t('turn.model')} value={model} />}
      <Row label={t('turn.in')} value={fmt.tokens(usage.promptTokens)} />
      {usage.cachedTokens !== undefined && (
        <Row label={t('turn.cached')} value={fmt.tokens(usage.cachedTokens)} />
      )}
      {firstTokenMs !== undefined && (
        <Row
          label={t('turn.firstToken')}
          value={formatDuration(firstTokenMs)}
        />
      )}
    </dl>
  );
}

function Row({
  label,
  value,
}: {
  readonly label: string;
  readonly value: string;
}): JSX.Element {
  return (
    <div className="message-info__row">
      <dt className="message-info__label">{label}</dt>
      <dd className="message-info__value">{value}</dd>
    </div>
  );
}
