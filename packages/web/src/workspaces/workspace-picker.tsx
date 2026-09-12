/**
 * Which workspace this conversation's files are in, in the conversation.
 *
 * The sibling of `AgentPicker`, in the same composer row and for the same
 * reason: choosing where a turn's files live is part of asking the question,
 * not part of configuring the application.
 *
 * It means two different things and says so, which is the whole of its design:
 *
 *  - **Before the first message** there is no session row, so the choice is only
 *    a preference for the conversation about to start. It is kept in the
 *    workspace context, which is also what `newSession` sends.
 *  - **After the first message** the binding lives on the session row, and
 *    changing it is a real edit — `PATCH /api/sessions/:key`. A frame naming a
 *    workspace is ignored for a session that already exists, deliberately, so
 *    this is the only way to move one.
 *
 * **Why this is not the sidebar switcher.** The two controls write different
 * things and that is what keeps them from contradicting each other:
 *
 * | | sidebar `WorkspaceSwitcher` | this |
 * |---|---|---|
 * | writes | the preference | the binding, when there is one |
 * | scopes | the session list, the Files page, where the *next* session is born | which workspace *this* conversation's tools use |
 * | PATCHes | never | once a row exists |
 *
 * Where they meet — the preference — `adopt` reconciles: this calls it on
 * success so the sidebar and Files follow immediately, and the hub's
 * re-broadcast then arrives carrying the same value, so the effect in
 * `use-connection.ts` does not re-fire. That effect selects a string and holds a
 * stable `adopt`, so an unchanged workspace re-renders nothing; it only follows
 * the server when the reported value genuinely moves, which is exactly when
 * following it is right.
 *
 * The move takes effect from the *next* turn. A turn already running captured
 * its jail when it started, so it finishes in the workspace it began in — which
 * is why the server does not refuse a move mid-turn.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Link } from '@tanstack/react-router';
import { ChevronDown, Folder, Settings2, TriangleAlert } from 'lucide-react';
import { useEffect, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { DEFAULT_WORKSPACE_ID } from '@ghostwire/protocol';

import { Button } from '@/components/ui/button.js';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.js';
import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { toast } from '@/components/ui/toast.js';
import { folderLabel } from './folder.js';
import { useWorkspace } from './workspace-context.js';

export function WorkspacePicker({
  sessionKey,
}: {
  readonly sessionKey?: string;
}): JSX.Element {
  const { t } = useTranslation();
  const { workspaceId: preferred, select, adopt } = useWorkspace();
  const queryClient = useQueryClient();

  const workspaces = useQuery({
    queryKey: queryKeys.workspaces,
    queryFn: ({ signal }) => api.workspaces(signal),
  });

  // A conversation nobody has spoken in has no row, so this 404s until the
  // first turn lands. That is the signal, not a failure: no row means the
  // choice is still only a preference.
  const stored = useQuery({
    queryKey: queryKeys.session(sessionKey ?? ''),
    queryFn: ({ signal }) => api.session(sessionKey ?? '', signal),
    enabled: sessionKey !== undefined,
    retry: false,
  });

  // `SessionSummary.workspaceId` carries a schema default, unlike `agentId`, so
  // this is `undefined` in exactly one case: the query 404'd and there is no
  // row. Do not "fix" it to `?? DEFAULT_WORKSPACE_ID` — that would make an
  // unspoken conversation look bound and turn the first pick into a PATCH.
  const bound = stored.data?.workspaceId;
  const current = bound ?? preferred;
  const rows = workspaces.data?.workspaces ?? [];
  const match = rows.find((row) => row.id === current);
  const label = match?.name ?? current;
  // Only once the listing has actually arrived. While it is in flight every id
  // looks missing, and a picker that flagged the workspace on each cold load
  // would cry wolf until the query settled.
  const missing = workspaces.isSuccess && match === undefined;

  // A stale *preference* is corrected here, because this is the first place that
  // holds both the remembered id and the list to check it against — the
  // workspace context is mounted above the data layer and has no listing.
  //
  // Only a preference, never a binding: a workspace detached in another tab
  // leaves this conversation pointing at a folder that is still on disk, and
  // silently moving somebody's session to fix a label would be a worse answer
  // than saying so. The remembered id is otherwise only ever corrected in the
  // browser that did the detaching.
  useEffect(() => {
    if (!missing || bound !== undefined) return;
    select(DEFAULT_WORKSPACE_ID);
  }, [missing, bound, select]);

  const move = useMutation({
    mutationFn: (workspaceId: string) =>
      api.moveSessionToWorkspace(sessionKey ?? '', workspaceId),
    onSuccess: (updated) => {
      queryClient.setQueryData(queryKeys.session(updated.key), updated);
      // The unscoped prefix: the row leaves one workspace-scoped list and joins
      // another, so both have to refetch.
      void queryClient.invalidateQueries({ queryKey: queryKeys.sessions() });
      // `sessionCount` on two workspace rows just moved. Deliberately *not*
      // `['files']` — file listings are keyed by workspace, so a move changes
      // which key is read rather than what is behind the old one.
      void queryClient.invalidateQueries({ queryKey: queryKeys.workspaces });
      // The switcher follows the conversation rather than claiming to have set
      // it — see `adopt` in the context.
      adopt(updated.workspaceId);
    },
    onError: (error: Error) => {
      toast.error(t('workspaces.moveFailed'), error.message);
    },
  });

  const choose = (workspaceId: string): void => {
    if (workspaceId === current) return;
    if (bound === undefined) {
      // No row yet: this is the workspace the conversation will be created in.
      select(workspaceId);
      return;
    }
    move.mutate(workspaceId);
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className={
            missing ? 'composer__picker is-missing' : 'composer__picker'
          }
          aria-label={
            missing
              ? t('workspaces.missingLabel', { id: current })
              : t('workspaces.pickerLabel', { name: label })
          }
        >
          {missing ? (
            <TriangleAlert aria-hidden="true" />
          ) : (
            <Folder aria-hidden="true" />
          )}
          <span className="truncate">{label}</span>
          <ChevronDown className="composer__picker-caret" aria-hidden="true" />
        </Button>
      </DropdownMenuTrigger>

      <DropdownMenuContent align="start" side="top" className="floating--menu">
        {/* Said before the list rather than as an empty selection. The radio
            group below matches nothing when the bound workspace is gone, and a
            menu with no item checked reads as a rendering bug rather than as a
            conversation pointing at a workspace that is no longer listed. */}
        {missing && (
          <DropdownMenuLabel className="composer__picker-notice" role="alert">
            {t('workspaces.missingNotice', { id: current })}
          </DropdownMenuLabel>
        )}

        <DropdownMenuLabel>
          {bound === undefined
            ? t('workspaces.forThisSession')
            : t('workspaces.moveThisSession')}
        </DropdownMenuLabel>

        <DropdownMenuRadioGroup value={current} onValueChange={choose}>
          {rows.map((workspace) => (
            <DropdownMenuRadioItem key={workspace.id} value={workspace.id}>
              <span className="truncate">{workspace.name}</span>
              <span className="composer__picker-hint">
                {folderLabel(workspace)}
              </span>
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>

        <DropdownMenuSeparator />

        <DropdownMenuItem asChild>
          <Link to="/workspaces">
            <Settings2 />
            {t('workspaces.manage')}
          </Link>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
