/**
 * The conversations list.
 *
 * Sessions were the one thing in this app with no management screen. The
 * sidebar carried the whole feature — a hand-rolled list with a bare
 * `DropdownMenu`, no search, no sort, and a Delete that fired on one click with
 * nothing between it and the transcript — while agents, workspaces, files and
 * jobs all had a page built from `components/crud`. This is the same frame they
 * use, and the sidebar now delegates its two actions to the hooks beside this
 * file rather than owning them.
 *
 * **The search goes to the server, which no other list here does.** Every other
 * screen filters `filterRows` over rows already in memory, and `sort.ts` says
 * why: those lists are one request, so a request per keystroke would be slower
 * and no more correct. Sessions are not that. They are a SQLite table an
 * automation job appends to every five minutes, so filtering the page in hand
 * would search the newest 25 conversations and report nothing for the one from
 * last month — a search that is confidently wrong, which is worse than none.
 *
 * The sort control offers three columns and not four. There is no "most
 * messages": the count is a correlated subquery, so ordering by it would scan
 * the messages table once per session row — see `SessionStore.listSessions`.
 *
 * **Every session, whatever workspace it is in.** This was once scoped to a
 * switcher in the sidebar, which meant a conversation moved to another
 * workspace silently left the list and the only way back was to guess where it
 * had gone. A workspace says where a conversation's *files* are, not which
 * drawer it is filed in — so it is a column here, not a filter.
 */

import { Link, useNavigate } from '@tanstack/react-router';
import { MessageSquare, Pencil, Plus, Trash2 } from 'lucide-react';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { SessionSummary } from '@ghostwire/protocol';

import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { DataList, DataListRow } from '@/components/crud/data-list.js';
import { ListSort } from '@/components/crud/list-sort.js';
import { NameDialog } from '@/components/crud/name-dialog.js';
import { Pagination } from '@/components/crud/pagination.js';
import { RowActions } from '@/components/crud/row-actions.js';
import { usePagination } from '@/components/crud/use-pagination.js';
import { Badge } from '@/components/ui/badge.js';
import { Button } from '@/components/ui/button.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { SearchFilter } from '@/components/ui/search-filter.js';
import { newSession } from '@/lib/connection.js';
import { useFormat } from '@/lib/use-format.js';
import { useAgent } from '@/agents/agent-context.js';
import { useWorkspace } from '@/workspaces/workspace-context.js';
import {
  useDeleteSession,
  useRenameSession,
  useSessionPage,
} from './use-sessions.js';
import type { SessionSortKey } from './use-sessions.js';

/** Only the title reads from A. A time is asked "which is newest". */
const ASCENDING_FIRST: readonly SessionSortKey[] = ['title'];

/**
 * Which origins get a badge, and which is simply the norm.
 *
 * `web` is every conversation a person started, so badging it would put the same
 * word on almost every row and say nothing. What is worth marking is the rows
 * *nobody* started by hand — this is the badge `SessionStore.listSessions` asks
 * for in the note about a five-minute job filling a recency-sorted list.
 */
const BADGED_ORIGINS = new Set(['automation', 'subagent']);

export function SessionsRoute(): JSX.Element {
  const { t } = useTranslation();
  const fmt = useFormat();
  const navigate = useNavigate();
  const { workspaceId } = useWorkspace();
  const { agentId } = useAgent();

  const [filter, setFilter] = useState('');
  const [sort, setSort] = useState<{
    key: SessionSortKey;
    descending: boolean;
  }>({
    key: 'updated',
    descending: true,
  });
  const [renaming, setRenaming] = useState<SessionSummary | undefined>(
    undefined,
  );
  const [pendingDelete, setPendingDelete] = useState<
    SessionSummary | undefined
  >(undefined);

  // Page first, total after: the request needs a page number before there is a
  // response to count. Changing the search or the order changes which rows are
  // being paged, so each of them sends the reader back to page 1.
  const page = usePagination({
    resetOn: `${filter}|${sort.key}|${String(sort.descending)}`,
  });

  const sessions = useSessionPage({
    page: page.page,
    q: filter,
    sort: sort.key,
    desc: sort.descending,
  });

  const rename = useRenameSession();
  const remove = useDeleteSession();

  const total = sessions.data?.total ?? 0;
  const pagination = page.withTotal(total);
  const rows = sessions.data?.sessions ?? [];
  const now = Date.now();

  const title = (session: SessionSummary): string =>
    session.title === '' ? t('sessions.untitled') : session.title;

  function startChat(): void {
    // Nothing is written until the first message lands — the same as the
    // sidebar's New session, and for the same reason.
    const key = newSession(workspaceId, agentId);
    void navigate({ to: '/', search: { session: key } });
  }

  return (
    <div className="stack page page--wide">
      <div className="cluster page__header">
        <h1 className="page__title">{t('sessions.title')}</h1>
        <span className="spacer" />
        {/* A button rather than a link, because there is no address to go to
            until a conversation has been started. */}
        <Button onClick={startChat}>
          <Plus />
          {t('sessions.newSession')}
        </Button>
      </div>

      <p className="page__note">{t('sessions.note')}</p>

      <div className="row list-toolbar">
        <SearchFilter
          value={filter}
          label={t('sessions.filter')}
          onValueChange={setFilter}
        />
        <ListSort
          options={[
            { key: 'updated', label: t('sessions.updated') },
            { key: 'created', label: t('sessions.created') },
            { key: 'title', label: t('sessions.titleColumn') },
          ]}
          sort={sort}
          ascendingFirst={ASCENDING_FIRST}
          onChange={setSort}
        />
      </div>

      {sessions.isPending && (
        <p className="page__note">{t('common.loading')}</p>
      )}
      {sessions.isError && (
        <p role="alert" className="page__error">
          {t('sessions.loadFailed')}
        </p>
      )}

      {sessions.data !== undefined &&
        (rows.length === 0 ? (
          <p className="page__note">
            {filter === ''
              ? t('sessions.none')
              : t('sessions.noMatch', { filter })}
          </p>
        ) : (
          <DataList label={t('sessions.title')}>
            {rows.map((session) => (
              <DataListRow
                key={session.key}
                primary={
                  <Link
                    to="/"
                    search={{ session: session.key }}
                    className="data-list__open"
                    aria-label={t('sessions.open', { title: title(session) })}
                  >
                    <MessageSquare />
                    <span className="truncate">{title(session)}</span>
                  </Link>
                }
                meta={
                  <>
                    {BADGED_ORIGINS.has(session.origin) && (
                      <Badge tone="neutral">{session.origin}</Badge>
                    )}
                    {/* The count carries its own noun: there is no column
                        heading above it any more, and a bare `12` beside a
                        timestamp is a number nobody can name. */}
                    <span>
                      {t('sessions.messageCount', {
                        count: session.messageCount,
                      })}
                    </span>
                    <span>{fmt.relativeTime(session.updatedAtMs, now)}</span>
                  </>
                }
                actions={
                  <RowActions label={title(session)}>
                    <DropdownMenuItem
                      onSelect={() => {
                        setRenaming(session);
                      }}
                    >
                      <Pencil />
                      {t('sessions.rename')}
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      className="menu__item--danger"
                      onSelect={() => {
                        setPendingDelete(session);
                      }}
                    >
                      <Trash2 />
                      {t('sessions.delete')}
                    </DropdownMenuItem>
                  </RowActions>
                }
              />
            ))}
          </DataList>
        ))}

      {sessions.data !== undefined && (
        <Pagination
          pagination={pagination}
          total={total}
          label={t('sessions.title')}
        />
      )}

      <NameDialog
        open={renaming !== undefined}
        onOpenChange={(open) => {
          if (!open) setRenaming(undefined);
        }}
        title={t('sessions.renameTitle')}
        fieldLabel={t('sessions.titleColumn')}
        initialValue={renaming?.title ?? ''}
        submitLabel={t('sessions.rename')}
        pending={rename.pending}
        onSubmit={(value) => {
          if (renaming === undefined) return;
          // Closed on success, not on the press: a rename that failed should
          // leave the box on screen with what was typed in it.
          rename.mutate(
            { key: renaming.key, title: value },
            {
              onSuccess: () => {
                setRenaming(undefined);
              },
            },
          );
        }}
      />

      <ConfirmDialog
        open={pendingDelete !== undefined}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(undefined);
        }}
        title={t('sessions.deleteTitle')}
        description={t('sessions.deleteHint', {
          title: pendingDelete === undefined ? '' : title(pendingDelete),
        })}
        confirmLabel={t('sessions.delete')}
        tone="danger"
        pending={remove.pending}
        onConfirm={() => {
          if (pendingDelete === undefined) return;
          remove.mutate(pendingDelete.key, {
            onSuccess: () => {
              setPendingDelete(undefined);
            },
          });
        }}
      />
    </div>
  );
}
