/**
 * The plan the agent is running on, above the box you type into.
 *
 * It answers "where has this got to", which is a question asked *while a turn
 * is running*, so it sits where the turn is being watched rather than behind a
 * dialog. The same argument put the context budget under the composer.
 *
 * **The store is the truth, not the transcript.** A `todo` call carries the list
 * in its arguments, and reading it from there would be one fewer request — but
 * `/tasks clear` from the terminal or a chat app writes the store and leaves no
 * tool call behind, and a panel built on the transcript would go on showing a
 * plan nothing is running. So the list is fetched, and the transcript is used
 * only as the signal that it has moved.
 *
 * That signal is the last **top-level** `todo` call that succeeded. Top-level
 * because a subagent runs in its own session with its own list, and its calls
 * are nested inside the delegating card rather than beside them — so scanning
 * the turn's own parts is the whole of the filter. Succeeded because a refused
 * call still carries arguments, and refetching on one would be refetching to
 * learn nothing changed.
 *
 * It renders nothing at all when there is no plan. A fresh tab has a session key
 * the socket minted and no stored row behind it, so the request 404s — and an
 * empty box above the composer of a conversation that has not started is
 * answering a question nobody asked.
 *
 * Clear is in the body rather than beside the heading, because the heading is a
 * `summary` and `summary` is already a button: a second one inside it has to
 * cancel the disclosure toggle on every click to work at all, and announces
 * itself as a control nested in a control.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Circle, CircleCheck, CircleDashed } from 'lucide-react';
import { useEffect, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { TaskStatus } from '@darkwire/protocol';

import { Button } from '@/components/ui/button.js';
import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { useTurnStore } from '@/state/turn.js';
import type { Transcript } from '@/state/transcript.js';

/** One icon per state, from the one set every other glyph comes from. */
const ICONS = {
  done: CircleCheck,
  doing: CircleDashed,
  todo: Circle,
} as const satisfies Record<TaskStatus, typeof Circle>;

/**
 * The id of the last successful top-level `todo` call, or the empty string.
 *
 * A string rather than a count, so a second call that happens to produce the
 * same number of tasks still moves it.
 */
export function lastTodoCallId(transcript: Transcript): string {
  let id = '';
  for (const item of transcript) {
    if (item.kind !== 'turn') continue;
    for (const part of item.parts) {
      if (
        part.kind === 'tool' &&
        part.name === 'todo' &&
        part.status === 'ok'
      ) {
        id = part.id;
      }
    }
  }
  return id;
}

export function TaskPanel({
  sessionKey,
}: {
  readonly sessionKey: string | undefined;
}): JSX.Element | null {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const marker = useTurnStore((state) => lastTodoCallId(state.transcript));

  const tasks = useQuery({
    queryKey: queryKeys.tasks(sessionKey ?? ''),
    queryFn: ({ signal }) => api.tasks(sessionKey ?? '', signal),
    enabled: sessionKey !== undefined,
    // A conversation that has not started 404s, and that is not a condition
    // worth retrying three times above the composer.
    retry: false,
  });

  // Mid-turn. `turn.end` already invalidates the whole `['sessions']` prefix, so
  // this is only about the four or five times a list moves inside one turn.
  useEffect(() => {
    if (marker === '' || sessionKey === undefined) return;
    void queryClient.invalidateQueries({
      queryKey: queryKeys.tasks(sessionKey),
    });
  }, [marker, sessionKey, queryClient]);

  const clear = useMutation({
    mutationFn: () => api.clearTasks(sessionKey ?? ''),
    onSuccess: () =>
      queryClient.invalidateQueries({
        queryKey: queryKeys.tasks(sessionKey ?? ''),
      }),
  });

  const list = tasks.data?.tasks ?? [];
  if (list.length === 0) return null;

  const done = list.filter((task) => task.status === 'done').length;

  return (
    <details className="tasks" open>
      <summary className="tasks__summary">
        <span className="tasks__title">{t('tasks.title')}</span>
        <span className="tasks__count">
          {t('tasks.progress', { done, total: list.length })}
        </span>
      </summary>
      <ol className="tasks__list">
        {list.map((task, index) => {
          const Icon = ICONS[task.status];
          return (
            <li
              // The list is replaced wholesale on every write and has no ids, so
              // the position is the only stable key there is — and it is stable
              // for exactly as long as the render it belongs to.
              key={`${String(index)}-${task.text}`}
              className={`tasks__item tasks__item--${task.status}`}
            >
              <Icon className="tasks__icon" aria-hidden="true" />
              <span className="tasks__text">{task.text}</span>
            </li>
          );
        })}
      </ol>
      <div className="tasks__actions">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => {
            clear.mutate();
          }}
          disabled={clear.isPending}
        >
          {t('tasks.clear')}
        </Button>
      </div>
    </details>
  );
}
