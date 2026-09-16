/**
 * Editing one workspace.
 *
 * A route of its own rather than a rename dialog, and the same shape as the
 * agent and provider editors: the list picks, this edits, the back link returns.
 * A dialog with a single box is the right size for a label and the wrong size
 * for everything else worth reading here: the folder the files are actually in,
 * how many conversations still point at it, and when it last moved.
 *
 * **The folder is a field, and changing it moves the tree.** It is the directory
 * name, so a save that includes it is a `rename(2)` plus a repoint of every
 * session that named the old one — done in one request, because a folder that
 * moved without its conversations is worse than either half. Two things it
 * cannot do, and both are refused rather than hidden: the default workspace's
 * folder *is* the workspace root and the parent of every other one, so its box
 * is inert; and a signed URL already in flight carries the old folder and stops
 * resolving. Those are minted for seconds at a time, and the alternative was a
 * folder nobody could ever correct.
 *
 * **The default's folder is `/`.** It is the workspace root itself —
 * `workspaceDirFor` maps that one id to `paths.workspace`, and every other
 * workspace is a directory inside it — so `/` is both what it is and the reason
 * it cannot move. This screen has rendered two wrong answers on the way here:
 * `workspace/default`, a directory that does not exist, and then `workspace`,
 * which collides with what the Files breadcrumb calls the root of whichever
 * workspace you are *in*. See `folder.ts`.
 *
 * **Delete is in the head, not at the bottom of the form.** A destructive action
 * does not belong in the reading order of the settings it would destroy — and
 * the flow behind it is `DeleteWorkspaceDialog`, shared with the list, because
 * "detach it" and "move seven conversations first" is two questions rather than
 * one cascading yes.
 */

import type { TFunction } from 'i18next';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Link, useNavigate, useParams } from '@tanstack/react-router';
import { ArrowLeft, Trash2 } from 'lucide-react';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import {
  RESERVED_WORKSPACE_IDS,
  deriveWorkspaceId,
  isWorkspaceId,
  type WorkspaceSummary,
} from '@darkwire/protocol';

import { Badge } from '@/components/ui/badge.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { toast } from '@/components/ui/toast.js';
import { RowActions } from '@/components/crud/row-actions.js';
import { api } from '@/lib/api.js';
import { useFormat } from '@/lib/use-format.js';
import { queryKeys } from '@/lib/query.js';
import {
  FieldGrid,
  SaveBar,
  Section,
  TextField,
} from '@/components/form/controls.js';
import { DeleteWorkspaceDialog } from './delete-workspace.js';
import { WORKSPACE_ROOT_PATH, folderLabel } from './folder.js';
import { useWorkspace } from './workspace-context.js';

/**
 * Creating a workspace, on the page that edits one.
 *
 * The same form, empty. The dialog it replaced asked for exactly these two
 * fields and then created the directory on submit — so an operator who changed
 * their mind in the editor had already left a folder on disk. Nothing is
 * written until Save here.
 *
 * The folder is still asked at creation, and that argument is unchanged: making
 * it is a `mkdir`, while changing it afterwards is a `rename(2)` under a tree
 * somebody may be working in, plus a repoint of every conversation that named
 * the old one.
 */
export function WorkspaceCreateRoute(): JSX.Element {
  const { t } = useTranslation();
  const workspaces = useQuery({
    queryKey: queryKeys.workspaces,
    queryFn: ({ signal }) => api.workspaces(signal),
  });

  if (workspaces.isPending) {
    return <p className="page__note">{t('workspaces.loadingOne')}</p>;
  }
  if (workspaces.isError) {
    return (
      <p role="alert" className="page__error">
        {t('workspaces.loadListError', { message: workspaces.error.message })}
      </p>
    );
  }

  return <Editor others={workspaces.data.workspaces} />;
}

export function WorkspaceEditorRoute(): JSX.Element {
  const { t } = useTranslation();
  const { workspaceId } = useParams({ from: '/workspaces/$workspaceId' });

  const workspaces = useQuery({
    queryKey: queryKeys.workspaces,
    queryFn: ({ signal }) => api.workspaces(signal),
  });

  if (workspaces.isPending) {
    return <p className="page__note">{t('workspaces.loadingOne')}</p>;
  }
  if (workspaces.isError) {
    return (
      <p role="alert" className="page__error">
        {t('workspaces.loadOneError', { message: workspaces.error.message })}
      </p>
    );
  }

  const workspace = workspaces.data.workspaces.find(
    (candidate) => candidate.id === workspaceId,
  );

  // A stale link — a bookmark to one that was detached or moved, or a hand-typed
  // id. Saying so beats an empty form that silently creates it on the first save.
  if (workspace === undefined) {
    return (
      <div className="stack page page--wide">
        <p role="alert" className="page__error">
          {t('workspaces.noSuchWorkspace', { id: workspaceId })}
        </p>
        <Link to="/workspaces" className="page__back">
          <ArrowLeft aria-hidden="true" />
          {t('workspaces.backToWorkspaces')}
        </Link>
      </div>
    );
  }

  // Remounts on a change of workspace, so one workspace's edits cannot survive
  // into the next one's boxes — including after a move, where the id in the URL
  // is the thing that changed.
  return (
    <Editor
      key={workspace.id}
      workspace={workspace}
      others={workspaces.data.workspaces}
    />
  );
}

function Editor({
  workspace,
  others,
}: {
  /** Absent is create: the same form, seeded empty, writing nothing until Save. */
  readonly workspace?: WorkspaceSummary;
  /** Every workspace, so a folder another one already holds is refused here. */
  readonly others: readonly WorkspaceSummary[];
}): JSX.Element {
  const { t } = useTranslation();
  const fmt = useFormat();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const { workspaceId: selected, select } = useWorkspace();

  const creating = workspace === undefined;
  const [name, setName] = useState(workspace?.name ?? '');
  const [folder, setFolder] = useState(workspace?.id ?? '');
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  const nameChanged = name.trim() !== (workspace?.name ?? '');
  const folderChanged = folder.trim() !== (workspace?.id ?? '');
  const folderError = folderProblem(folder.trim(), workspace, others, t);
  const dirty = nameChanged || folderChanged;

  const save = useMutation({
    mutationFn: () =>
      workspace === undefined
        ? // Empty folder means "derive it from the name", which the server does.
          api.createWorkspace(
            name.trim(),
            folder.trim() === '' ? undefined : folder.trim(),
          )
        : api.updateWorkspace(workspace.id, {
            ...(nameChanged ? { name: name.trim() } : {}),
            ...(folderChanged ? { folder: folder.trim() } : {}),
          }),
    onSuccess: (updated) => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.workspaces });
      // A move changes the id every other scope is keyed on, so the session
      // list and the file listings have to be re-asked rather than answered
      // from entries filed under a folder that no longer exists.
      void queryClient.invalidateQueries({ queryKey: ['sessions'] });
      void queryClient.invalidateQueries({ queryKey: ['files'] });

      if (workspace === undefined) {
        // Straight into the editor for the workspace just made, on success
        // rather than on the press: the folder is real on disk now, and the
        // page that describes it is the one that can move or remove it.
        void navigate({
          to: '/workspaces/$workspaceId',
          params: { workspaceId: updated.id },
          replace: true,
        });
        toast.success(
          `Created ${updated.name}`,
          `Its folder is ${folderLabel(updated)}.`,
        );
        return;
      }

      if (updated.id !== workspace.id) {
        // The switcher and the URL both name the old folder. Moving them is not
        // a nicety: staying on it would leave the Files page 404ing on every
        // request, exactly as a delete would.
        if (selected === workspace.id) select(updated.id);
        void navigate({
          to: '/workspaces/$workspaceId',
          params: { workspaceId: updated.id },
          replace: true,
        });
        toast.success(
          `Moved to ${folderLabel(updated)}`,
          'Its sessions came with it.',
        );
        return;
      }
      toast.success(`Renamed to ${updated.name}`);
    },
    onError: (error: Error) => {
      toast.error('Could not save it', error.message);
    },
  });

  const onSave = (): void => {
    // Empty is the one thing the server refuses outright, and a save that
    // bounced off a 400 to say so would be a round trip to learn what the box
    // already knows.
    if (name.trim() === '' || folderError !== undefined) return;
    save.mutate();
  };

  const now = Date.now();

  return (
    <div className="stack page page--wide">
      <div className="editor__head">
        <Link to="/workspaces" className="page__back">
          <ArrowLeft aria-hidden="true" />
          {t('workspaces.title')}
        </Link>

        <div className="cluster editor__title">
          <h1 className="page__title">
            {workspace?.name ?? t('workspaces.newTitle')}
          </h1>
          {workspace?.isDefault === true && <Badge>default</Badge>}
          <span className="spacer" />
          {/* The default is the parent of every other workspace; there is no
              coherent thing removing it could mean — and neither is there for
              one that does not exist yet. */}
          {workspace !== undefined && !workspace.isDefault && (
            <RowActions label={workspace.name}>
              <DropdownMenuItem
                className="menu__item--danger"
                onSelect={() => {
                  setConfirmingDelete(true);
                }}
              >
                <Trash2 />
                {t('workspaces.deleteWorkspace')}
              </DropdownMenuItem>
            </RowActions>
          )}
        </div>

        <p className="page__note">
          {workspace === undefined
            ? t('workspaces.newHint')
            : workspace.isDefault
              ? t('workspaces.defaultNote')
              : t('workspaces.namedNote', {
                  count: workspace.sessionCount,
                  updated: fmt.relativeTime(workspace.updatedAtMs, now),
                })}
        </p>
      </div>

      <Section
        title={t('workspaces.identity')}
        description={t('workspaces.identityDesc')}
      >
        <FieldGrid>
          <TextField
            label={t('common.name')}
            value={name}
            placeholder={workspace?.id ?? t('workspaces.namePlaceholder')}
            onValueChange={setName}
            hint={t('workspaces.nameHint')}
          />

          <TextField
            label={t('workspaces.folder')}
            // The default's folder is stated, not hinted. A placeholder is the
            // wrong slot for a fact: drawn in the muted tier, it means "nothing
            // here yet", so the one box on the screen whose answer is fixed
            // would also be the one that looks empty. A value, and inert — `/`
            // is the root every other workspace is a directory inside, so there
            // is no rename of it that does not mean relocating the whole tree.
            value={workspace?.isDefault === true ? WORKSPACE_ROOT_PATH : folder}
            className="workspaces__folder-input"
            spellCheck={false}
            disabled={workspace?.isDefault === true}
            placeholder={workspace?.id ?? deriveWorkspaceId(name)}
            onValueChange={setFolder}
            error={folderError}
            hint={
              creating
                ? // The derived slug, live. It is what Save would actually
                  // produce, and watching it change while the name is typed is
                  // what explains a second box next to the name at all.
                  folder.trim() === '' && name.trim() !== ''
                  ? `Creates ${WORKSPACE_ROOT_PATH}${deriveWorkspaceId(name)}. You can move it later from this screen.`
                  : t('workspaces.folderHint')
                : workspace.isDefault
                  ? t('workspaces.folderDefault')
                  : folderChanged && folderError === undefined
                    ? `Moves the folder to ${WORKSPACE_ROOT_PATH}${folder.trim()} and brings its sessions with it.`
                    : t('workspaces.folderHintEdit')
            }
          />
        </FieldGrid>
      </Section>

      <SaveBar
        dirty={dirty}
        saving={save.isPending}
        onSave={onSave}
        onRevert={() => {
          setName(workspace?.name ?? '');
          setFolder(workspace?.id ?? '');
        }}
      />

      <DeleteWorkspaceDialog
        workspace={confirmingDelete ? workspace : undefined}
        onOpenChange={(open) => {
          if (!open) setConfirmingDelete(false);
        }}
        onDeleted={() => {
          void navigate({ to: '/workspaces' });
        }}
      />
    </div>
  );
}

/**
 * What is wrong with the folder that was typed, if anything.
 *
 * The store's own three rules, checked a layer early so the message can point at
 * the box rather than arriving as a toast after a directory did not move. The
 * default is not judged at all — its box cannot be typed in.
 */
function folderProblem(
  folder: string,
  /** Absent while creating: there is no own-folder to exempt from the checks. */
  workspace: WorkspaceSummary | undefined,
  others: readonly WorkspaceSummary[],
  t: TFunction,
): string | undefined {
  // Creating with an empty box is legal — the server derives the folder from
  // the name — so there is nothing to judge until something is typed.
  if (workspace === undefined && folder === '') return undefined;
  if (workspace?.isDefault === true || folder === workspace?.id) {
    return undefined;
  }
  if (folder === '') return t('workspaces.folderShape');
  if (!isWorkspaceId(folder)) return t('workspaces.folderShape');
  if (RESERVED_WORKSPACE_IDS.has(folder)) return t('workspaces.folderReserved');
  if (others.some((candidate) => candidate.id === folder)) {
    return t('workspaces.folderTaken');
  }
  return undefined;
}
