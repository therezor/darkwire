/**
 * A result the server posted into the conversation: a background job's output,
 * or a scheduled job's report.
 *
 * Drawn as a tool card rather than a user bubble, because nobody typed it. It
 * reads from the message's typed `from`, never from the header line the model
 * reads.
 */

import { ChevronRight } from 'lucide-react';
import { useId, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { Delivered } from '@darkwire/protocol';

import { Output, StatusIcon } from '@/chat/tool-card.js';
import { cn } from '@/lib/cn.js';

export function DeliveredCard({
  from,
  text,
}: {
  readonly from: Delivered;
  readonly text: string;
}): JSX.Element {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const bodyId = useId();
  const name =
    from.kind === 'exec'
      ? t('delivered.job', { id: from.id })
      : t('delivered.scheduled');

  return (
    <section aria-label={name} className="tool-card">
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
          <StatusIcon status={from.status === 'ok' ? 'ok' : 'error'} />
          <span className="tool-card__name">{name}</span>
          <span className="tool-card__summary truncate">{from.label}</span>
          <span className="tool-card__timing">
            {t(`delivered.status.${from.status}`)}
          </span>
        </button>
      </h4>
      {open && (
        <div id={bodyId} className="stack tool-card__body">
          <Output content={text} terminal={from.kind === 'exec'} />
        </div>
      )}
    </section>
  );
}
