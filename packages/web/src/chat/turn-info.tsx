/**
 * What a turn cost, behind the info button under it.
 *
 * Two sources, and the split is the point. A turn this tab watched happen
 * carries its own numbers on `turn.end`, so asking the server to repeat figures
 * the client just measured would be a round trip to learn what it already
 * knows. A turn from before the page loaded has no such event ever again —
 * which is exactly why the server persists `turn_stats` — so those are fetched
 * once per conversation and looked up by turn id.
 *
 * `turnRate` comes from `@ghostwire/protocol` rather than being computed here,
 * because the terminal reports the same figure and two implementations of one
 * division eventually disagree about what to do with a zero — or, now, about
 * which of two divisors to prefer.
 */

import { useQuery } from '@tanstack/react-query';
import { Info } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import {
  DEFAULT_WORKSPACE_ID,
  turnRate,
  type StopReason,
  type Usage,
} from '@ghostwire/protocol';

import { api } from '@/lib/api.js';
import { formatDuration } from '@/lib/format.js';
import { useFormat } from '@/lib/use-format.js';
import { queryKeys } from '@/lib/query.js';
import { Button } from '@/components/ui/button.js';
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover.js';
import type { TurnItem } from '@/state/transcript.js';

export function TurnInfo({
  turn,
  sessionKey,
}: {
  readonly turn: TurnItem;
  readonly sessionKey: string | undefined;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <Popover>
      {/* The `<Button>` is written out here rather than wrapped in a component
          of its own. `asChild` clones this element to inject `onClick`, the
          ref and `aria-expanded`, and a component that does not spread its
          props swallows every one of them — which is a trigger that renders
          perfectly and opens nothing. */}
      <PopoverTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={t('turn.details')}>
          <Info />
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="popover--turn-info">
        <TurnInfoBody turn={turn} sessionKey={sessionKey} />
      </PopoverContent>
    </Popover>
  );
}

function TurnInfoBody({
  turn,
  sessionKey,
}: {
  readonly turn: TurnItem;
  readonly sessionKey: string | undefined;
}): JSX.Element {
  const { t } = useTranslation();
  const fmt = useFormat();

  // Only for a turn the live stream did not describe. A session the user is
  // sitting in front of never reaches this.
  const stored = useQuery({
    queryKey: queryKeys.turns(sessionKey ?? ''),
    queryFn: ({ signal }) => api.turns(sessionKey ?? '', signal),
    enabled: sessionKey !== undefined && turn.usage === undefined,
    retry: false,
  });

  // What made this session, for the sessions a person did not. It reads from
  // the session row rather than the turn because origin belongs to the whole
  // session, and it is fetched here rather than under the composer because a
  // badge saying `web` on almost every session says nothing — the same
  // reasoning as `BADGED_ORIGINS` in `sessions/sessions-page.tsx`.
  //
  // Costs nothing until asked: this body only mounts when the popover opens.
  const session = useQuery({
    queryKey: queryKeys.session(sessionKey ?? ''),
    queryFn: ({ signal }) => api.session(sessionKey ?? '', signal),
    enabled: sessionKey !== undefined,
    // A 404 until the first turn lands, which is the normal state of a new
    // session rather than a failure worth retrying.
    retry: false,
  });

  const origin = session.data?.origin ?? '';
  const row = stored.data?.turns.find((entry) => entry.turnId === turn.id);

  /**
   * Which workspace this turn ran in, and whether saying so tells anyone
   * anything.
   *
   * A turn in flight has no stored row yet, so it is running in wherever the
   * session is bound now. A stored one carries its own — and the two genuinely
   * differ once a conversation has been moved, which is the case this row
   * exists for: a transcript can span several workspaces, and only this says
   * which files a given turn could reach.
   *
   * Hidden when it is the default *and* the session is still there, on the same
   * argument the `origin` row makes below: a row that is always present and
   * always says the same word is a row nobody reads.
   */
  const ranIn = row?.workspaceId ?? session.data?.workspaceId ?? '';
  const boundTo = session.data?.workspaceId ?? '';
  const showWorkspace =
    ranIn !== '' && (ranIn !== DEFAULT_WORKSPACE_ID || ranIn !== boundTo);

  const usage: Usage | undefined = turn.usage ?? row?.usage;
  const model = turn.model === '' ? (row?.model ?? '') : turn.model;
  const provider = turn.provider === '' ? (row?.provider ?? '') : turn.provider;
  const iterations =
    turn.iterations > 0 ? turn.iterations : (row?.iterations ?? 0);
  const stopReason: StopReason | undefined = turn.stopReason ?? row?.stopReason;
  const elapsedMs =
    turn.elapsedMs ??
    (row === undefined
      ? undefined
      : Math.max(0, row.endedAtMs - row.startedAtMs));
  // The live turn first and the stored row second, the same fall-through every
  // field above uses — which is what makes the panel identical before and after
  // a reload.
  const generationMs = turn.generationMs ?? row?.generationMs;
  const generationTokens = turn.generationTokens ?? row?.generationTokens;
  const firstTokenMs = turn.firstTokenMs ?? row?.firstTokenMs;

  if (usage === undefined) {
    return <p className="turn-info__empty">{t('turn.none')}</p>;
  }

  // `generationMs` and `generationTokens` are plumbed but never given rows of
  // their own. The rate's numerator is deliberately not the `Out` figure above
  // — a request whose reply arrived in one frame contributes to neither — and
  // showing the pair would invite a reader to divide `Out` by it and read the
  // mismatch as a bug.
  const rate = turnRate(usage, { generationMs, generationTokens, elapsedMs });

  return (
    <dl className="turn-info">
      {/* Absent for `web`, and absent for a session with no row yet. Every
          session someone opened by hand is `web`, so naming it would be a row
          that is always there and never tells anyone anything. */}
      {origin !== '' && origin !== 'web' && (
        <Row label={t('turn.origin')} value={origin} />
      )}
      {showWorkspace && <Row label={t('turn.workspace')} value={ranIn} />}
      <Row label={t('turn.model')} value={model === '' ? '—' : model} />
      <Row
        label={t('turn.provider')}
        value={provider === '' ? '—' : provider}
      />
      <Row label={t('turn.in')} value={fmt.tokens(usage.promptTokens)} />
      <Row label={t('turn.out')} value={fmt.tokens(usage.completionTokens)} />
      {usage.cachedTokens !== undefined && (
        <Row label={t('turn.cached')} value={fmt.tokens(usage.cachedTokens)} />
      )}
      {usage.reasoningTokens !== undefined && (
        <Row
          label={t('turn.reasoning')}
          value={fmt.tokens(usage.reasoningTokens)}
        />
      )}
      {elapsedMs !== undefined && (
        <Row label={t('turn.elapsed')} value={formatDuration(elapsedMs)} />
      )}
      {/* What the reader waited through before anything appeared. On a local
          model loading its weights this is most of the turn, and this is the
          row that says so — the rate no longer does, because the rate no
          longer divides by it. */}
      {firstTokenMs !== undefined && (
        <Row
          label={t('turn.firstToken')}
          value={formatDuration(firstTokenMs)}
        />
      )}
      {/* Absent rather than zero when there is nothing to divide: a turn that
          produced no tokens has no rate, and one measured at zero milliseconds
          was not measured. */}
      {rate !== undefined && (
        <Row label={t('turn.rate')} value={`${rate.toFixed(1)} tok/s`} />
      )}
      <Row label={t('turn.steps')} value={String(iterations)} />
      {stopReason !== undefined && (
        <Row label={t('turn.stopped')} value={stopReason.replace('_', ' ')} />
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
    <div className="turn-info__row">
      <dt className="turn-info__label">{label}</dt>
      <dd className="turn-info__value">{value}</dd>
    </div>
  );
}
