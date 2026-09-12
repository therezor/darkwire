/**
 * The workspaces list.
 *
 * A page rather than a dialog hanging off the sidebar switcher, and that is not
 * a cosmetic distinction. A workspace is a folder the agent works in and a
 * scope every conversation belongs to — the same *kind* of thing as a file or
 * an agent. Managing it in a modal buys a second row stylesheet, an inline
 * rename form, and a delete one click away from the folder; the last two are
 * bugs that only look like styling.
 *
 * So: the same `page page--wide` frame as Files and Agents, the same
 * `list-toolbar` and `SearchFilter`, the same `DataList`, the same kebab, and
 * the same `ConfirmDialog` in front of the one irreversible action.
 *
 * **The row opens an editor, exactly as an agent's does.** The name cell is a
 * link with the workspace's own icon at the head of it, and there is no Rename
 * item in the kebab any more — the name is a field on the screen the row opens,
 * which is where the folder, the conversation count and everything else about a
 * workspace is read. A second way to edit one field, with its own dialog and its
 * own mutation, was a shortcut that had to be kept correct twice.
 *
 * The second line under the name is the folder, and it earns its place now that
 * the two are chosen separately: "Client Acme" living in `/acme` is something
 * you would otherwise have to open the row to find out.
 *
 * **Delete detaches**, and both the copy and the flow behind it live in
 * `DeleteWorkspaceDialog`, shared with the editor.
 */

import { useQuery } from '@tanstack/react-query';
import { Link, useNavigate } from '@tanstack/react-router';
import { Folder, Pencil, Plus, Trash2 } from 'lucide-react';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { WorkspaceSummary } from '@ghostwire/protocol';

import { Badge } from '@/components/ui/badge.js';
import { Button } from '@/components/ui/button.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { SearchFilter } from '@/components/ui/search-filter.js';
import { RowActions } from '@/components/crud/row-actions.js';
import { DataList, DataListRow } from '@/components/crud/data-list.js';
import { ListSort } from '@/components/crud/list-sort.js';
import { Pagination } from '@/components/crud/pagination.js';
import { useListPage } from '@/components/crud/use-list-page.js';
import type { Comparators } from '@/components/crud/sort.js';
import { api } from '@/lib/api.js';
import { useFormat } from '@/lib/use-format.js';
import { queryKeys } from '@/lib/query.js';
import { folderLabel } from '@/workspaces/folder.js';
import { DeleteWorkspaceDialog } from '@/workspaces/delete-workspace.js';

type SortKey = 'name' | 'sessions' | 'updated';

/** Only the name reads from A. A count and a time are asked "which is biggest / newest". */
const ASCENDING_FIRST: readonly SortKey[] = ['name'];

const COMPARE: Comparators<WorkspaceSummary, SortKey> = {
  name: (a, b) => a.name.localeCompare(b.name),
  sessions: (a, b) => a.sessionCount - b.sessionCount,
  updated: (a, b) => a.updatedAtMs - b.updatedAtMs,
};

export function WorkspacesRoute(): JSX.Element {
  const { t } = useTranslation();
  const fmt = useFormat();
  const navigate = useNavigate();

  const [pendingDelete, setPendingDelete] = useState<
    WorkspaceSummary | undefined
  >(undefined);

  const workspaces = useQuery({
    queryKey: queryKeys.workspaces,
    queryFn: ({ signal }) => api.workspaces(signal),
  });

  const all = workspaces.data?.workspaces ?? [];
  // The whole registry is already in memory — this list is one request, not one
  // page of one — so the page is a slice rather than a second fetch.
  const { filter, setFilter, sort, setSort, matched, pagination, rows } =
    useListPage({
      rows: all,
      initialSort: { key: 'name', descending: false },
      // Filtering on the folder too: it is on screen under every name, and a list
      // that shows a value it will not match on reads as broken.
      haystack: (workspace) => `${workspace.name} ${workspace.id}`,
      comparators: COMPARE,
      // The default workspace holds every other one, so it is the parent of the
      // list rather than a peer in it. It stays at the top in both directions for
      // the same reason a directory does in Files.
      group: (workspace) => (workspace.isDefault ? 0 : 1),
      tiebreak: (a, b) => a.name.localeCompare(b.name),
    });

  const now = Date.now();

  return (
    <div className="stack page page--wide">
      <div className="cluster page__header">
        <h1 className="page__title">{t('workspaces.title')}</h1>
        <span className="spacer" />
        {/* A link, not a dialog: creating a workspace is the same form as
            editing one, and no directory is made until it is saved. */}
        <Button asChild>
          <Link to="/workspaces/new">
            {/* The bare mark, as on Agents and New session. A plus *inside* a
                folder said "add a folder", which is the implementation — what
                the button does is add a workspace, and the label says so. */}
            <Plus />
            {t('workspaces.new')}
          </Link>
        </Button>
      </div>

      <p className="page__note">{t('workspaces.note')}</p>

      <div className="row list-toolbar">
        <SearchFilter
          value={filter}
          label={t('workspaces.filter')}
          onValueChange={setFilter}
        />
        <ListSort
          options={[
            { key: 'name', label: t('common.name') },
            { key: 'sessions', label: t('workspaces.sessions') },
            { key: 'updated', label: t('workspaces.updated') },
          ]}
          sort={sort}
          ascendingFirst={ASCENDING_FIRST}
          onChange={setSort}
        />
      </div>

      {workspaces.isPending && (
        <p className="page__note">{t('common.loading')}</p>
      )}
      {workspaces.isError && (
        <p role="alert" className="page__error">
          {t('workspaces.loadError', { message: workspaces.error.message })}
        </p>
      )}

      {workspaces.isSuccess &&
        (matched.length === 0 ? (
          <p className="page__note">
            {t('common.noMatches', { filter, count: all.length })}
          </p>
        ) : (
          <DataList label={t('workspaces.title')}>
            {rows.map((workspace) => (
              <DataListRow
                key={workspace.id}
                primary={
                  <Link
                    to="/workspaces/$workspaceId"
                    params={{ workspaceId: workspace.id }}
                    className="data-list__open"
                    aria-label={`Edit ${workspace.name}`}
                  >
                    {/* A folder, because that is what a workspace is. One
                          icon for both kinds — that the default holds the
                          others is what the badge beside it says, and a second
                          glyph saying the same thing is a second thing to
                          learn. */}
                    <Folder />
                    <span className="stack workspaces__name">
                      <span className="workspaces__name-row">
                        <span className="truncate">{workspace.name}</span>
                        {workspace.isDefault && <Badge>default</Badge>}
                      </span>
                      {/* Rooted at `/`, so the column reads `/` against
                            `/acme` and the nesting is visible rather than
                            described. The default is not in a folder called
                            `default` — it *is* the root — and it is not called
                            `workspace` either, which is the Files breadcrumb's
                            name for whichever workspace you are in. See
                            `workspaces/folder.ts`. */}
                      <span className="workspaces__folder truncate">
                        {folderLabel(workspace)}
                      </span>
                    </span>
                  </Link>
                }
                meta={
                  <>
                    {/* The count carries its own noun. There is no column
                        heading above it any more, and a bare `12` beside a
                        timestamp is a number nobody can name. */}
                    <span>
                      {t('workspaces.sessionCount', {
                        count: workspace.sessionCount,
                      })}
                    </span>
                    <span>{fmt.relativeTime(workspace.updatedAtMs, now)}</span>
                  </>
                }
                actions={
                  <RowActions label={workspace.name}>
                    {/* No Rename. The name is a field in the editor, which is
                          one press away and is where the folder and everything
                          else about a workspace is read. */}
                    <DropdownMenuItem
                      onSelect={() => {
                        void navigate({
                          to: '/workspaces/$workspaceId',
                          params: { workspaceId: workspace.id },
                        });
                      }}
                    >
                      <Pencil />
                      Edit
                    </DropdownMenuItem>
                    {/* The default is the parent of every other workspace;
                          there is no coherent thing removing it could mean. */}
                    {!workspace.isDefault && (
                      <DropdownMenuItem
                        className="menu__item--danger"
                        onSelect={() => {
                          setPendingDelete(workspace);
                        }}
                      >
                        <Trash2 />
                        Delete
                      </DropdownMenuItem>
                    )}
                  </RowActions>
                }
              />
            ))}
          </DataList>
        ))}

      {workspaces.isSuccess && (
        <Pagination
          pagination={pagination}
          total={matched.length}
          label={t('workspaces.title')}
        />
      )}

      <DeleteWorkspaceDialog
        workspace={pendingDelete}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(undefined);
        }}
      />
    </div>
  );
}
