/**
 * Detaching a workspace, wherever it is asked for.
 *
 * Two screens ask now — the row kebab on the list and the kebab in the editor's
 * head — and the flow is not one dialog. A workspace whose conversations still
 * point at it cannot be detached: the server answers 409 with the count, and the
 * offer that resolves it is a *second* question. Copying that into both callers
 * would be copying the part with the shape, which is the half that drifts.
 *
 * So the whole flow lives here — the two mutations, the two dialogs, the read of
 * the typed 409, and the move off a workspace that is going away — and a caller
 * owns one piece of state: which workspace, if any, is being asked about.
 *
 * **Delete detaches, and the copy has to say so.** The folder and everything in
 * it stays on disk. Recreating a workspace with the same folder adopts it again,
 * which is what makes the round trip safe rather than merely reversible in
 * principle.
 */

import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useEffect, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { WorkspaceSummary } from '@ghostwire/protocol';

import { toast } from '@/components/ui/toast.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { ApiError, api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { DEFAULT_WORKSPACE_ID, useWorkspace } from './workspace-context.js';

export function DeleteWorkspaceDialog({
  workspace,
  onOpenChange,
  onDeleted,
}: {
  /** The workspace being asked about, or `undefined` for "nothing is". */
  readonly workspace: WorkspaceSummary | undefined;
  readonly onOpenChange: (open: boolean) => void;
  /** Called once the row is gone — the editor uses it to leave the page. */
  readonly onDeleted?: (workspace: WorkspaceSummary) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { workspaceId, select } = useWorkspace();

  /**
   * Which question is on screen.
   *
   * Not two `open` booleans and not the caller's state going `undefined` and
   * back: the refusal turns the first dialog into the second one about the same
   * workspace, so the subject has to survive the transition.
   */
  const [stage, setStage] = useState<'confirm' | 'blocked'>('confirm');
  const [sessionCount, setSessionCount] = useState(0);

  // Reopening on another workspace — or on the same one after a cancel — asks
  // the first question again. A caller that closed on the "move them first"
  // dialog and reopened would otherwise land straight back on it.
  useEffect(() => {
    if (workspace !== undefined) setStage('confirm');
  }, [workspace]);

  const refresh = (): void => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.workspaces });
    // The session list is scoped by workspace, and a delete moves sessions.
    void queryClient.invalidateQueries({ queryKey: ['sessions'] });
  };

  const remove = useMutation({
    mutationFn: (target: WorkspaceSummary) => api.deleteWorkspace(target.id),
    onSuccess: (result, target) => {
      // Moving off it first: staying on a workspace that no longer exists would
      // leave the Files page 404ing on every request.
      if (target.id === workspaceId) select(DEFAULT_WORKSPACE_ID);
      onOpenChange(false);
      refresh();
      toast.success(
        `Deleted ${target.name}`,
        'Its folder and files are still on disk.',
      );
      onDeleted?.(target);
    },
    onError: (error: Error) => {
      // The 409 is not a failure to report, it is a question to ask.
      const blocked = sessionCountOf(error);
      if (blocked !== undefined) {
        setSessionCount(blocked);
        setStage('blocked');
        return;
      }
      toast.error('Could not delete it', error.message);
    },
  });

  const move = useMutation({
    mutationFn: (target: WorkspaceSummary) =>
      api.moveWorkspaceSessions(target.id, DEFAULT_WORKSPACE_ID),
    onSuccess: (result, target) => {
      remove.mutate(target);
    },
    onError: (error: Error) => {
      toast.error('Could not move the sessions', error.message);
    },
  });

  return (
    <>
      <ConfirmDialog
        open={workspace !== undefined && stage === 'confirm'}
        onOpenChange={onOpenChange}
        title={t('workspaces.deleteTitle')}
        description={t('workspaces.deleteHint', {
          workspace: workspace?.name ?? '',
        })}
        confirmLabel="Delete"
        pending={remove.isPending}
        onConfirm={() => {
          if (workspace !== undefined) remove.mutate(workspace);
        }}
      />

      <ConfirmDialog
        open={workspace !== undefined && stage === 'blocked'}
        onOpenChange={onOpenChange}
        title={t('workspaces.moveTitle')}
        description={t('workspaces.blocked', {
          count: sessionCount,
          workspace: workspace?.name ?? '',
        })}
        confirmLabel="Move and delete"
        tone="primary"
        pending={move.isPending || remove.isPending}
        onConfirm={() => {
          if (workspace !== undefined) move.mutate(workspace);
        }}
      />
    </>
  );
}

/**
 * The count a 409 carried, if it is the one this flow knows how to answer.
 *
 * Reading the typed `details` rather than the message: the message is prose and
 * a wording change should not silently turn an offer to fix the problem into a
 * generic error toast.
 */
function sessionCountOf(error: Error): number | undefined {
  if (!(error instanceof ApiError) || error.status !== 409) return undefined;
  const count = (error.details as { sessionCount?: unknown } | undefined)
    ?.sessionCount;
  return typeof count === 'number' ? count : undefined;
}
