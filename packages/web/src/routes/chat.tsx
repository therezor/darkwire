/**
 * The chat view.
 *
 * The route assembles three things and owns one decision. The three: the
 * transcript, the composer, and the history fetch. The decision is where a
 * conversation's past comes from, which is subtler than it looks because there
 * are two sources and neither can wait for the other.
 *
 *  - **Storage** holds every completed message, and is fetched over REST for
 *    whatever session the URL names.
 *  - **The replay ring** holds the events of a turn that has *not* completed,
 *    which storage by definition cannot — and is therefore the only thing that
 *    can put a half-written answer back on screen after a reload.
 *
 * They arrive in either order, so the fetch is merged in rather than assigned:
 * `mergeHistory` puts the stored conversation underneath whatever the socket
 * has already built, keyed on id. Replacing would discard the turn the user is
 * watching; appending would render the conversation twice.
 *
 * The socket itself is not here. It hangs off the shell, which is the router's
 * root and therefore the only component that survives navigating to Settings
 * and back — see `chat/use-connection.ts`.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Link, useNavigate, useSearch } from '@tanstack/react-router';
import { useEffect, type JSX } from 'react';

import { api } from '@/lib/api.js';
import {
  approveTool,
  editMessage,
  regenerateTurn,
  sendUserMessage,
  stopTurn,
} from '@/lib/connection.js';
import { queryKeys } from '@/lib/query.js';
import { useTranslation } from 'react-i18next';
import { useTurnStore } from '@/state/turn.js';
import { toast } from '@/components/ui/toast.js';
import { AgentPicker } from '@/agents/agent-picker.js';
import { useAgentChoice } from '@/agents/use-agent-choice.js';
import { WorkspacePicker } from '@/workspaces/workspace-picker.js';
import { useAgent } from '@/agents/agent-context.js';
import { Composer } from '@/chat/composer.js';
import { useCommands } from '@/chat/use-commands.js';
import type { MessageAction } from '@/chat/message.js';
import { ContextStrip } from '@/context/context-strip.js';
import { TranscriptView } from '@/chat/transcript-view.js';
import { Welcome } from '@/chat/welcome.js';

export function ChatRoute(): JSX.Element {
  const { t } = useTranslation();
  const { session } = useSearch({ from: '/' });
  const navigate = useNavigate();
  // What the picker in the composer is showing. Carried on the message so a
  // conversation that has no row yet is created bound to it.
  const { agentId } = useAgent();

  const transcript = useTurnStore((state) => state.transcript);
  const busy = useTurnStore((state) => state.busy);
  const queueDepth = useTurnStore((state) => state.queueDepth);
  const connection = useTurnStore((state) => state.connection);
  const sessionKey = useTurnStore((state) => state.sessionKey);
  // The same hook the picker and `/agent` use, so all three agree on which
  // agent this conversation runs on. React Query dedupes the two queries behind
  // it, so asking here costs nothing the picker was not already paying.
  const agentChoice = useAgentChoice(sessionKey);

  const queryClient = useQueryClient();

  // The composer's slash commands. Assembled here rather than in the composer
  // because every one of them needs the router, the query cache or the socket,
  // and the composer is a leaf its own tests mount without any of the three.
  const runSlashCommand = useCommands();

  /**
   * Branch is the one action that is a request rather than a frame.
   *
   * It creates a session and starts no turn, so it needs an answer — the key of
   * the fork, which is where the user is then taken. Regenerate and edit go
   * over the socket instead, because both *start a turn* and every turn belongs
   * to the hub's queue.
   */
  const branch = useMutation({
    mutationFn: ({ key, seq }: { key: string; seq: number }) =>
      api.branchSession(key, seq),
    onSuccess: (fork) => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.sessions() });
      void navigate({ to: '/', search: { session: fork.key } });
    },
    onError: (error: Error) => {
      toast.error('Could not branch the session', error.message);
    },
  });

  function runAction(action: MessageAction): void {
    switch (action.kind) {
      case 'edit':
        // The attachments come along: an edit *replaces* the stored message, so
        // sending only the new wording deletes every file that was on it.
        editMessage(action.seq, action.text, action.attachments);
        return;
      case 'regenerate':
        regenerateTurn(action.seq);
        return;
      case 'branch':
        if (sessionKey !== undefined) {
          branch.mutate({ key: sessionKey, seq: action.seq });
        }
        return;
    }
  }

  // Whether a turn can run at all. The shell reads this too, so on a working
  // install it is already in the cache and costs nothing here.
  const status = useQuery({
    queryKey: queryKeys.status,
    queryFn: ({ signal }) => api.status(signal),
  });

  // Only for a session the URL named. A key the *server* minted has no history
  // by definition, and asking for it is a 404 on every fresh tab.
  const history = useQuery({
    queryKey: queryKeys.messages(session ?? ''),
    queryFn: ({ signal }) => api.messages(session ?? '', signal),
    enabled: session !== undefined,
    // A conversation that has not been spoken in has no stored row, so this is
    // a 404 until the first turn lands — an expected answer rather than a
    // failure worth three exponential backoffs.
    retry: false,
    // Against the app-wide 30 s, because this response *is* the transcript on a
    // switch and the usual invalidation cannot reach it. A tab only receives
    // events for the session it is attached to, so a turn that ends in a
    // conversation this tab has left invalidates nothing here — switching back
    // inside the window rendered a cached copy of a history that had moved on.
    // `session.resume` cannot cover the difference either: its cursor means
    // "everything before this is in storage", so a stale copy of storage is
    // indistinguishable from the truth.
    staleTime: 0,
  });

  useEffect(() => {
    const data = history.data;
    if (data === undefined) return;

    const state = useTurnStore.getState();
    // The one guard that is still needed: a fetch that resolves after the user
    // has already switched to another conversation would otherwise merge one
    // session's history into another's transcript.
    if (state.sessionKey !== session) return;
    state.mergeHistory(data.messages, data.subagentRuns, data.failures);
    // `sessionKey` is a dependency because the guard above reads it, and it
    // moves *after* this effect on a switch: the socket is attached by the
    // shell, which is this route's parent, and React runs a child's effects
    // first. So the first pass after switching to an already-fetched
    // conversation sees the previous key and correctly declines to merge.
    //
    // Without it there was no second pass. React Query's structural sharing
    // hands back the *same* `history.data` reference when a refetch is
    // deep-equal to the cache, so a transcript that can no longer change —
    // a finished automation run, any conversation nobody is adding to — never
    // produced a new reference, the effect never re-ran, and the chat stayed
    // empty until a reload. `mergeStoredHistory` dedupes by id and turn id, so
    // the extra pass this adds on a cold open is a no-op.
  }, [history.data, session, sessionKey]);

  // The welcome screen is for a conversation that is genuinely empty, not for
  // one whose history is still in flight — showing it and then replacing it
  // with a transcript is a flash of the wrong screen on every reload.
  const empty = transcript.length === 0 && !history.isFetching;

  return (
    <div className="chat">
      {empty ? (
        <div className="transcript__viewport">
          {/* The key, so the card names the agent *this* conversation runs on
              rather than the one a new conversation would start on. An empty
              transcript does not mean an unbound session: `/clear` leaves the
              binding in place, and so does a branch nobody has spoken in. */}
          <Welcome {...(sessionKey === undefined ? {} : { sessionKey })} />
        </div>
      ) : (
        <TranscriptView
          transcript={transcript}
          busy={busy}
          sessionKey={sessionKey}
          onApprove={approveTool}
          onAction={runAction}
        />
      )}

      {/* Above the composer rather than inside it: the pointer is a link, and
          the composer is a leaf that is rendered outside a router by its own
          tests. The route is where a route knows how to be navigated to. */}
      {status.data?.configured === false && (
        <p role="status" className="chat__setup-notice">
          {/* Three keys rather than one, because the sentence has a link in the
              middle of it. `Trans` would keep it whole and is the better answer
              if a second sentence ever needs this — it is not worth introducing
              the pattern, and teaching the extractor about it, for one. */}
          {t('chat.noModelBefore')}{' '}
          <Link to="/settings" search={{ panel: 'providers' }}>
            {t('chat.noModelLink')}
          </Link>{' '}
          {t('chat.noModelAfter')}
        </p>
      )}

      <Composer
        // What the next turn runs under, both halves of it: which agent, and
        // which workspace its files are in. In the session rather than the
        // sidebar, because choosing either is part of asking the question.
        //
        // Controls, and only controls. Where the session came from is not badged
        // beside them: it would put the word `web` under almost every message box
        // and say nothing — the same reason the list does not badge it either
        // (`sessions-page.tsx`). It reads from the turn details.
        //
        // The workspace one is second: the agent is the more frequent decision,
        // and the first position is the one the eye lands on.
        lead={
          // Wrapped rather than passed as a fragment: `.composer__meta` is a
          // `space-between` row, so two bare children would be pushed to
          // opposite ends of it instead of sitting together as one control
          // cluster.
          <div className="composer__pickers">
            <AgentPicker
              {...(sessionKey === undefined ? {} : { sessionKey })}
            />
            <WorkspacePicker
              {...(sessionKey === undefined ? {} : { sessionKey })}
            />
          </div>
        }
        // What `/effort` marks as in force. Resolved here rather than in the
        // composer because the binding lives behind `useAgentChoice`, which
        // needs the session key the composer deliberately does not take.
        agent={
          agentChoice.match?.reasoningEffort === undefined
            ? {}
            : { effort: agentChoice.match.reasoningEffort }
        }
        // The line under the box is the budget's: it is the one thing there that
        // changes, and a keyboard hint sharing the row never does. See
        // `composer.tsx`.
        meta={<ContextStrip sessionKey={sessionKey} />}
        busy={busy}
        queueDepth={queueDepth}
        connected={connection === 'open'}
        // Absent while the status query is in flight; treated as configured so
        // the composer does not flash a setup pointer on every page load of a
        // working install.
        configured={status.data?.configured ?? true}
        onStop={stopTurn}
        onCommand={runSlashCommand}
        onSend={(text, attachments) => {
          sendUserMessage(text, attachments, agentId);
          // The URL catches up with the session the server named, so a reload
          // or a shared link lands on the same conversation. `replace`, because
          // sending a message is not a navigation the back button should undo.
          if (session === undefined && sessionKey !== undefined) {
            void navigate({
              to: '/',
              search: { session: sessionKey },
              replace: true,
            });
          }
        }}
      />
    </div>
  );
}
