/**
 * The empty conversation.
 *
 * Not a marketing panel and not a tour. What a first-time reader of a
 * self-hosted agent needs to know is which model is about to answer them and
 * what it is allowed to do to their machine — the second of which is the thing
 * every hosted assistant leaves out, and the thing this one exists to make
 * explicit.
 *
 * **The model named here is the selected agent's, not the install's.** That
 * distinction is the whole value of the line: an agent may pin its own model, so
 * reading `/api/status` here would announce the install default to a
 * conversation about to run on a researcher's pinned model. A screen whose one
 * job is to say what will answer has to be right about it, or it is worse than
 * blank.
 *
 * **And it is this conversation's agent, not this browser's preference.** The
 * same argument one step further, because there are two answers and the
 * preference is the weaker one: it decides which agent a *new* conversation
 * starts on, while the row's binding decides which one answers an existing one.
 * This card renders for an empty transcript, which a bound conversation can
 * have — `/clear` empties one and a branch can start empty — so the preference
 * would name a different agent's model there. `useAgentChoice` is the
 * one place that rule lives, and the picker two lines below is reading it, so
 * the card that says what will answer had better read the same thing.
 *
 * **The suggested prompts are gone.** Three canned openers is a feature list
 * wearing the clothes of a shortcut: nobody wants to summarise the files in
 * this workspace, and an operator who did would type it faster than they could
 * read three sentences to find it. They also cost the screen its shape, putting
 * a list of buttons between the one paragraph that matters and the box.
 *
 * What is here instead is the keyboard hint. It is true forever and worth
 * reading once, so it belongs on the screen somebody sees before they have sent
 * anything — not under the composer on every render, where it crowds out the
 * context budget.
 */

import { useQuery } from '@tanstack/react-query';
import { Skull } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { Badge } from '@/components/ui/badge.js';
import { useAgentChoice } from '@/agents/use-agent-choice.js';

export function Welcome({
  sessionKey,
}: {
  /**
   * The conversation this card is standing in for, when there is one.
   *
   * Optional because the card is also what a tab with no session at all shows,
   * and because its own tests mount it as the leaf it is. Absent means there is
   * no binding to read and the answer is the preference — which is the same
   * answer `useAgentChoice` gives.
   */
  readonly sessionKey?: string;
}): JSX.Element {
  const { t } = useTranslation();
  // `match` rather than the raw id: it is this conversation's agent as the
  // listing resolves it, which is the same object the picker names.
  const { match } = useAgentChoice(sessionKey);

  // `/api/status` carries the *install's* model, which is the wrong answer
  // whenever the selected agent overrides it: a researcher pinned to one model
  // showed the default here, and the screen whose entire job is to say what is
  // about to answer said something else. `/api/agents` — which `useAgentChoice`
  // reads — resolves each agent's model after inheritance and after any
  // process-wide `--model` pin, which is the figure a turn will actually use.
  const status = useQuery({
    queryKey: queryKeys.status,
    queryFn: ({ signal }) => api.status(signal),
  });

  // Falling back to the install's model rather than to nothing: an agent id held
  // in `localStorage` can name an agent that has since been deleted, and a blank
  // line reads as "no model configured" — which is a different and alarming
  // claim. The picker beside the composer is what corrects the stale id.
  const provider = match?.provider ?? status.data?.provider ?? '';
  const model = match?.model ?? status.data?.model ?? '';

  return (
    <div className="stack welcome">
      <Skull className="welcome__mark" aria-hidden="true" />

      <div className="stack welcome__heading">
        <h1 className="welcome__title">{t('chat.ready')}</h1>
        {status.data?.configured === true && model !== '' && (
          <p className="cluster welcome__agent">
            <Badge tone="neutral">{provider}</Badge>
            <span className="welcome__model">{model}</span>
          </p>
        )}
      </div>

      <p className="welcome__note">{t('chat.welcomeNote')}</p>

      <p className="welcome__hint">{t('chat.welcomeHint')}</p>
    </div>
  );
}
