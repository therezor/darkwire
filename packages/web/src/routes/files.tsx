/**
 * The file browser.
 *
 * It shows the workspace: the one tree the agent's filesystem tools may reach,
 * and therefore the one tree a reader needs to see to know what a turn actually
 * did. Every path in and out is workspace-relative, and nothing here decides
 * whether a path is legal — `WorkspaceJail` does that on the server, for every
 * caller, and a second copy of those rules in a React component would be a
 * weaker one that no security test reads.
 *
 * The decisions:
 *
 *  - **The directory is in the URL, and so is the file open on top of it.** A
 *    file browser whose location lives in component state loses it on reload,
 *    cannot be linked, and turns the browser's own Back button into a way to
 *    leave the page entirely. Putting the open file there too is what lets both
 *    kinds of row be an `<a href>` — the same control Agents, Workspaces and
 *    Providers give their rows — instead of a `<button>` that opens a dialog
 *    only this component knows about.
 *  - **Filter and sort are not.** They are how the current view is being read,
 *    not where the reader is, and a URL that changed on every keystroke would
 *    fill the history with states nobody wants to walk back through.
 *  - **Deleting asks first.** It is the only irreversible action in the UI, the
 *    server refuses to recurse into a directory, and there is no undo below
 *    this. A dialog is the cheapest possible guard against a misplaced click.
 *  - **So does closing an editor with unsaved edits**, for the same reason: a
 *    dialog closes on `Escape` and on an outside click, which are two ways to
 *    lose work by one keypress.
 *  - **Uploads name their destination explicitly.** `POST /api/files/upload`
 *    takes the full target path, so the file lands in the directory being
 *    looked at rather than wherever the server would have guessed — and a drop
 *    onto the listing is the same call as the button.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Link, useNavigate, useSearch } from '@tanstack/react-router';
import {
  File as FileIcon,
  FilePlus,
  Folder,
  FolderOpen,
  FolderPlus,
  Pencil,
  Plus,
  Trash2,
  Upload,
} from 'lucide-react';
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type DragEvent,
  type JSX,
} from 'react';
import { useTranslation } from 'react-i18next';

import { DEFAULT_WORKSPACE_ID, type FileEntry } from '@ghostwire/protocol';

import { cn } from '@/lib/cn.js';
import { api } from '@/lib/api.js';
import { formatBytes } from '@/lib/format.js';
import { useFormat } from '@/lib/use-format.js';
import { queryKeys } from '@/lib/query.js';
import { Button } from '@/components/ui/button.js';
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogHeading,
  DialogSubheading,
} from '@/components/ui/dialog.js';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu.js';
import { SearchFilter } from '@/components/ui/search-filter.js';
import { toast } from '@/components/ui/toast.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { NameDialog } from '@/components/crud/name-dialog.js';
import { RowActions } from '@/components/crud/row-actions.js';
import { DataList, DataListRow } from '@/components/crud/data-list.js';
import { ListSort } from '@/components/crud/list-sort.js';
import { FilePreview } from '@/files/file-preview.js';
import {
  breadcrumbs,
  DEFAULT_SORT,
  filterEntries,
  joinPath,
  normalisePath,
  parentOf,
  ROOT_PATH,
  sortEntries,
  type SortKey,
  type SortOrder,
} from '@/files/paths.js';

/** A name reads from A; a size and a time are asked "which is biggest / newest". */
const ASCENDING_FIRST: readonly SortKey[] = ['name'];

/** What "New…" is being asked for. `undefined` means the dialog is closed. */
type NewKind = 'file' | 'directory';

export function FilesRoute(): JSX.Element {
  const { t } = useTranslation();
  const fmt = useFormat();
  const {
    path,
    workspace: fromUrl,
    file: openPath,
  } = useSearch({ from: '/files' });
  // The URL wins when it has one, so a link to a file is complete and
  // shareable — this page's own doctrine is that its location lives in the
  // address bar. Without one it is the default workspace, which is the *parent*
  // of every named one: they appear here as ordinary folders and are opened by
  // clicking into them. That is why this page needs no workspace control of its
  // own — the tree is the navigation. `/workspaces` is where they are created
  // and renamed, and its rows link back here with the parameter set.
  const workspace = fromUrl ?? DEFAULT_WORKSPACE_ID;
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const directory = normalisePath(path ?? ROOT_PATH);
  const [previewDirty, setPreviewDirty] = useState(false);
  const [discarding, setDiscarding] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<FileEntry | undefined>(
    undefined,
  );
  const [renaming, setRenaming] = useState<FileEntry | undefined>(undefined);
  const [creating, setCreating] = useState<NewKind | undefined>(undefined);
  const [filter, setFilter] = useState('');
  const [sort, setSort] = useState<SortOrder>(DEFAULT_SORT);
  const [dropping, setDropping] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);

  const listing = useQuery({
    queryKey: queryKeys.files(workspace, directory),
    // `.` rather than `''` for the root: the query parameter's default only
    // applies when it is absent, so an empty string would reach the jail as one.
    queryFn: ({ signal }) =>
      api.files(workspace, directory === ROOT_PATH ? '.' : directory, signal),
  });

  const refresh = (): void => {
    void queryClient.invalidateQueries({
      queryKey: queryKeys.files(workspace, directory),
    });
  };

  /**
   * This page's address, written in one place.
   *
   * Every row, every menu item and every close builds its destination through
   * here, because a search object in TanStack Router *replaces* the whole query
   * string rather than merging into it — a hand-written `{ file }` somewhere
   * would silently drop the workspace and the directory with it.
   */
  const searchFor = (
    file?: string,
  ): { path?: string; workspace: string; file?: string } => ({
    // `path` is omitted at the root rather than sent as `.`: the parameter's
    // default only applies when it is absent, and a URL that says `?path=` for
    // "the top of the workspace" is a longer way of saying nothing.
    ...(directory === ROOT_PATH ? {} : { path: directory }),
    workspace,
    ...(file === undefined ? {} : { file }),
  });

  /**
   * The file the URL says is open, resolved against the listing.
   *
   * From `listing.data` rather than the filtered rows, so typing in the filter
   * box does not close the dialog over it. A path that is not in this directory
   * — a stale link, a file someone else deleted — resolves to nothing and the
   * dialog simply does not open, which is the same answer the row would give.
   */
  const preview = useMemo(
    () =>
      listing.data?.entries.find(
        (entry) => entry.path === openPath && !entry.isDirectory,
      ),
    [listing.data, openPath],
  );

  const upload = useMutation({
    mutationFn: async (files: readonly File[]) => {
      // Sequential rather than `Promise.all`: the upload route has a body limit
      // per request and the workspace is a filesystem, so ten parallel writes
      // buy nothing and make a partial failure harder to report.
      for (const file of files) {
        await api.upload(workspace, joinPath(directory, file.name), file);
      }
      return files.length;
    },
    onSuccess: (count) => {
      toast.success(t('files.uploaded', { count }));
      refresh();
    },
    onError: (error: Error) => {
      toast.error('Upload failed', error.message);
    },
  });

  const remove = useMutation({
    // A directory always goes recursively, because the dialog behind this has
    // already said so and counted what it holds. The flag exists to stop a
    // *stray request* from recursing, not to make the UI ask twice.
    mutationFn: (entry: FileEntry) =>
      api.deleteFile(workspace, entry.path, entry.isDirectory),
    onSuccess: (result, entry) => {
      toast.success(`Deleted ${entry.name}`);
      setPendingDelete(undefined);
      refresh();
      // The dialog would close on its own once the row is gone from the
      // listing, but the address would keep naming a file that no longer
      // exists — and that address is what a reload and a shared link read.
      if (openPath === entry.path) {
        void navigate({ to: '/files', search: searchFor() });
      }
    },
    onError: (error: Error) => {
      toast.error('Could not delete it', error.message);
    },
  });

  /**
   * What the folder about to be deleted holds.
   *
   * Fetched when the dialog opens rather than listed up front: the count is the
   * one thing that turns "Delete drafts?" into a decision, and asking for it
   * per row would be one request per directory on every listing.
   */
  const pendingContents = useQuery({
    queryKey: queryKeys.files(workspace, pendingDelete?.path ?? ''),
    queryFn: ({ signal }) =>
      api.files(workspace, pendingDelete?.path ?? '.', signal),
    enabled: pendingDelete?.isDirectory === true,
  });

  const create = useMutation({
    mutationFn: ({
      kind,
      name,
    }: {
      readonly kind: NewKind;
      readonly name: string;
    }) => {
      const target = joinPath(directory, name.trim());
      // An empty file rather than a placeholder line: what the reader asked for
      // is a name to start typing under, and anything written into it is
      // content they did not write.
      return kind === 'file'
        ? api.writeText(workspace, target, '')
        : api.createDirectory(workspace, target);
    },
    onSuccess: (entry, { kind }) => {
      setCreating(undefined);
      refresh();
      toast.success(`Created ${entry.name}`);
      // Straight into it, which is the only reason to have made it.
      if (kind === 'file') {
        void navigate({ to: '/files', search: searchFor(entry.path) });
      }
    },
    onError: (error: Error) => {
      toast.error('Could not create it', error.message);
    },
  });

  /**
   * Renaming, which is a move whose destination shares a parent.
   *
   * The new name is joined to the entry's *own* parent rather than to the
   * directory on screen. They are the same today, but a rename that silently
   * relocated a row would be a bug that only appeared once anything could show
   * an entry from somewhere else — and the cost of being right about it now is
   * one function call.
   */
  const rename = useMutation({
    mutationFn: ({
      entry,
      name,
    }: {
      readonly entry: FileEntry;
      readonly name: string;
    }) =>
      api.moveFile(
        workspace,
        entry.path,
        joinPath(parentOf(entry.path), name.trim()),
      ),
    onSuccess: (entry, { entry: previous }) => {
      setRenaming(undefined);
      refresh();
      toast.success(`Renamed to ${entry.name}`);
      // An open preview is now pointing at a path that no longer exists, so it
      // follows the file rather than silently 404ing on the next save.
      if (openPath === previous.path) {
        void navigate({ to: '/files', search: searchFor(entry.path) });
      }
    },
    onError: (error: Error) => {
      toast.error('Could not rename it', error.message);
    },
  });

  /** Stable, so the editor's `useEffect` does not re-fire on every render here. */
  const handleDirtyChange = useCallback((dirty: boolean) => {
    setPreviewDirty(dirty);
  }, []);

  /** Closing is a navigation now: the open file is a search parameter. */
  const closePreview = (): void => {
    setPreviewDirty(false);
    setDiscarding(false);
    void navigate({ to: '/files', search: searchFor() });
  };

  // Whatever the last file's editor reported does not describe this one. Back,
  // Forward and a link all change the open file without going through
  // `closePreview`, so the reset belongs on the parameter rather than on the
  // one path out that happens to be a button.
  useEffect(() => {
    setPreviewDirty(false);
    setDiscarding(false);
  }, [openPath]);

  const entries = useMemo(
    () => sortEntries(filterEntries(listing.data?.entries ?? [], filter), sort),
    [listing.data, filter, sort],
  );

  const onDrop = (event: DragEvent<HTMLDivElement>): void => {
    event.preventDefault();
    setDropping(false);
    const files = [...event.dataTransfer.files];
    if (files.length > 0) upload.mutate(files);
  };

  const now = Date.now();
  const total = listing.data?.entries.length ?? 0;

  return (
    <div className="stack page page--wide">
      <div className="cluster page__header">
        <h1 className="page__title">{t('files.title')}</h1>
        <span className="spacer" />
        {/* One trigger rather than two buttons, and the phone is what decided
            it: "New file", "New folder" and "Upload" spelled out side by side
            are wider than a handset, so the header wrapped into a ragged
            second line with one action stranded under the title. They are also
            the same act asked about two things, which is what a menu is for —
            Upload stays outside it because it is a different act entirely. */}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost">
              <Plus />
              {t('common.new')}
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="floating--menu">
            <DropdownMenuItem
              onSelect={() => {
                setCreating('file');
              }}
            >
              <FilePlus />
              {t('common.newFile')}
            </DropdownMenuItem>
            <DropdownMenuItem
              onSelect={() => {
                setCreating('directory');
              }}
            >
              <FolderPlus />
              {t('common.newFolder')}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <Button
          disabled={upload.isPending}
          onClick={() => {
            fileInput.current?.click();
          }}
        >
          <Upload />
          {upload.isPending ? 'Uploading…' : 'Upload'}
        </Button>
        <input
          ref={fileInput}
          type="file"
          multiple
          aria-label={t('files.upload')}
          className="sr-only"
          onChange={(event) => {
            const files = [...(event.target.files ?? [])];
            // Reset first: picking the same file twice in a row is otherwise
            // not a change event, and the second attempt does nothing at all.
            event.target.value = '';
            if (files.length > 0) upload.mutate(files);
          }}
        />
      </div>

      {/* Where you are, on its own line above the controls — the same place the
          other lists put their explanatory note, and for the same reason: it
          describes the page rather than acting on the list. */}
      <Breadcrumbs
        path={directory}
        workspace={workspace}
        onNavigate={() => {
          setFilter('');
        }}
      />

      <div className="row list-toolbar">
        <SearchFilter
          value={filter}
          label={t('files.filter')}
          onValueChange={setFilter}
        />
        <ListSort
          options={[
            { key: 'name', label: t('common.name') },
            { key: 'size', label: t('files.size') },
            { key: 'modified', label: t('files.modified') },
          ]}
          sort={sort}
          ascendingFirst={ASCENDING_FIRST}
          onChange={setSort}
        />
      </div>

      {listing.isPending && <p className="page__note">{t('common.loading')}</p>}
      {listing.isError && (
        <p role="alert" className="page__error">
          {t('files.listError', { message: listing.error.message })}
        </p>
      )}

      {listing.isSuccess && (
        <div
          className={cn(
            'file-drop',
            total === 0 && 'file-drop--empty',
            dropping && 'file-drop--over',
          )}
          onDragOver={(event) => {
            event.preventDefault();
            setDropping(true);
          }}
          onDragLeave={(event) => {
            // Only when the pointer actually left the region: `dragleave` also
            // fires crossing into a child, which would flicker the highlight
            // once per row the cursor passes over.
            if (
              !event.currentTarget.contains(event.relatedTarget as Node | null)
            ) {
              setDropping(false);
            }
          }}
          onDrop={onDrop}
        >
          {total === 0 ? (
            <div className="stack file-drop__empty">
              <Upload />
              <p className="file-drop__empty-title">{t('files.empty')}</p>
              <p className="file-drop__empty-hint">{t('files.emptyHint')}</p>
            </div>
          ) : entries.length === 0 ? (
            <p className="page__note">
              {t('files.noMatch', { filter, count: total })}
            </p>
          ) : (
            <DataList label={t('files.title')}>
              {entries.map((entry) => (
                <DataListRow
                  key={entry.path}
                  primary={
                    // One `<Link>` for both kinds, which is what every other
                    // CRUD list in the app gives its rows. A directory changes
                    // the listing and a file opens a dialog over it, but both
                    // are a change of address here, so both are middle-
                    // clickable, openable in a new tab, and reachable by Back.
                    <Link
                      to="/files"
                      search={
                        entry.isDirectory
                          ? { path: entry.path, workspace }
                          : searchFor(entry.path)
                      }
                      className={cn(
                        'data-list__open',
                        entry.isDirectory && 'data-list__open--directory',
                      )}
                      onClick={() => {
                        // The filter describes the listing being left. Going
                        // into a directory is the same route, so the component
                        // does not remount and would otherwise carry the filter
                        // into a directory nobody typed it for.
                        if (entry.isDirectory) setFilter('');
                      }}
                    >
                      {entry.isDirectory ? <Folder /> : <FileIcon />}
                      <span className="truncate">{entry.name}</span>
                    </Link>
                  }
                  meta={
                    <>
                      {/* A directory has no size of its own to report, and the
                          em dash is what the Size column used to say so. It
                          still reads as "nothing to say here" beside a time. */}
                      <span>
                        {entry.isDirectory ? '—' : formatBytes(entry.sizeBytes)}
                      </span>
                      <span>{fmt.relativeTime(entry.modifiedAtMs, now)}</span>
                    </>
                  }
                  actions={
                    <RowActions label={entry.name}>
                      <DropdownMenuItem
                        onSelect={() => {
                          if (entry.isDirectory) {
                            setFilter('');
                            void navigate({
                              to: '/files',
                              search: { path: entry.path, workspace },
                            });
                          } else {
                            void navigate({
                              to: '/files',
                              search: searchFor(entry.path),
                            });
                          }
                        }}
                      >
                        {entry.isDirectory ? <FolderOpen /> : <FileIcon />}
                        {entry.isDirectory ? 'Open' : 'Edit'}
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        onSelect={() => {
                          setRenaming(entry);
                        }}
                      >
                        <Pencil />
                        Rename
                      </DropdownMenuItem>
                      <DropdownMenuItem
                        className="menu__item--danger"
                        onSelect={() => {
                          setPendingDelete(entry);
                        }}
                      >
                        <Trash2 />
                        Delete
                      </DropdownMenuItem>
                    </RowActions>
                  }
                />
              ))}
            </DataList>
          )}
        </div>
      )}

      <Dialog
        open={preview !== undefined}
        onOpenChange={(open) => {
          if (open) return;
          // The one thing here that can lose work. `Escape` and a click on the
          // overlay both arrive as this, so the guard has to live at the point
          // the dialog closes rather than on a button.
          if (previewDirty) setDiscarding(true);
          else closePreview();
        }}
      >
        <DialogContent className="dialog--preview">
          <DialogHeader>
            <DialogHeading>{preview?.name ?? ''}</DialogHeading>
            <DialogSubheading>{preview?.path ?? ''}</DialogSubheading>
          </DialogHeader>
          {preview !== undefined && (
            <FilePreview
              entry={preview}
              workspace={workspace}
              onDirtyChange={handleDirtyChange}
            />
          )}
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={discarding}
        onOpenChange={(open) => {
          if (!open) setDiscarding(false);
        }}
        title={t('files.discardTitle')}
        description={`${preview?.path ?? ''} has changes that were never saved. Closing loses them.`}
        cancelLabel="Keep editing"
        confirmLabel="Discard"
        onConfirm={closePreview}
      />

      <NameDialog
        open={creating !== undefined}
        onOpenChange={(open) => {
          if (!open) setCreating(undefined);
        }}
        title={creating === 'directory' ? 'New folder' : 'New file'}
        description={`Created in ${directory === ROOT_PATH ? 'the workspace root' : directory}.`}
        // Nothing here validates the name: a separator or a traversal in it is
        // the jail's call, on the server, and the error comes back as the toast
        // any other refusal would.
        fieldLabel={creating === 'directory' ? 'Folder name' : 'File name'}
        placeholder={creating === 'directory' ? 'drafts' : 'notes.md'}
        pending={create.isPending}
        onSubmit={(name) => {
          if (creating !== undefined) create.mutate({ kind: creating, name });
        }}
      />

      <NameDialog
        open={renaming !== undefined}
        onOpenChange={(open) => {
          if (!open) setRenaming(undefined);
        }}
        title={renaming?.isDirectory === true ? 'Rename folder' : 'Rename file'}
        description={
          renaming?.isDirectory === true
            ? 'Everything inside moves with it. Nothing is copied.'
            : 'The file keeps its contents and its history on disk.'
        }
        fieldLabel="New name"
        initialValue={renaming?.name ?? ''}
        submitLabel="Rename"
        pending={rename.isPending}
        onSubmit={(name) => {
          if (renaming !== undefined) rename.mutate({ entry: renaming, name });
        }}
      />

      <ConfirmDialog
        open={pendingDelete !== undefined}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(undefined);
        }}
        title={
          pendingDelete?.isDirectory === true
            ? 'Delete this folder?'
            : 'Delete this file?'
        }
        description={`${pendingDelete?.path ?? ''} is removed from the workspace. There is no undo.`}
        confirmLabel="Delete"
        pending={remove.isPending}
        onConfirm={() => {
          if (pendingDelete !== undefined) remove.mutate(pendingDelete);
        }}
      >
        {/* The count is what makes this a decision rather than a reflex:
            "Delete drafts?" and "Delete drafts and the 47 things in it?" are
            different questions, and only one of them is the one being asked. */}
        {pendingDelete?.isDirectory === true && pendingContents.isSuccess && (
          <p
            className={cn(
              'notice',
              pendingContents.data.entries.length > 0 && 'notice--danger',
            )}
          >
            <Trash2 />
            <span>
              {pendingContents.data.entries.length === 0
                ? t('files.folderEmpty')
                : t('files.folderContents', {
                    count: pendingContents.data.entries.length,
                  })}
            </span>
          </p>
        )}
      </ConfirmDialog>
    </div>
  );
}

/**
 * Where you are, and every step of the way back to the root.
 *
 * Links, like the rows under them: a crumb is a directory, a directory is an
 * address, and a control that changes the address without an `href` cannot be
 * opened in a second tab or told apart from a button by anything reading the
 * page. `onNavigate` is what is left over — clearing the filter, which is view
 * state and not part of the address.
 */
function Breadcrumbs({
  path,
  workspace,
  onNavigate,
}: {
  readonly path: string;
  readonly workspace: string;
  readonly onNavigate: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const crumbs = breadcrumbs(path);

  return (
    <nav aria-label={t('files.breadcrumb')}>
      <ol className="breadcrumbs">
        {crumbs.map((crumb, index) => {
          const last = index === crumbs.length - 1;
          return (
            <li key={crumb.path}>
              {index > 0 && <span className="breadcrumbs__separator">/</span>}
              {last ? (
                // The current directory is text, not a link to itself.
                <span aria-current="page" className="breadcrumbs__current">
                  {crumb.label}
                </span>
              ) : (
                <Link
                  to="/files"
                  search={{
                    ...(crumb.path === ROOT_PATH ? {} : { path: crumb.path }),
                    workspace,
                  }}
                  className="breadcrumbs__link"
                  onClick={onNavigate}
                >
                  {crumb.label}
                </Link>
              )}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}
