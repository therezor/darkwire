/**
 * The MCP servers panel: the servers this install talks to.
 *
 * The list is the configured servers joined to their live state, and the join
 * is the whole reason this screen needs two requests. `GET /api/settings` says
 * what an operator asked for; `GET /api/mcp` says what came of it. A server
 * that is unreachable is not something to write into `config.json`, so the row
 * takes its name and its transport from the first and its badge and its reason
 * from the second.
 *
 * Shaped like Providers, because it is the same kind of thing: a list that
 * picks, a `RowActions` kebab for what needs no form, and an editor route for
 * the rest.
 */

import { Plug, Pencil, Plus, Power, PowerOff, Trash2 } from 'lucide-react';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';
import { Link, useNavigate } from '@tanstack/react-router';
import type { Config, McpServerStatus } from '@ghostwire/protocol';

import { Badge } from '@/components/ui/badge.js';
import { Button } from '@/components/ui/button.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { DataList, DataListRow } from '@/components/crud/data-list.js';
import { ListSort } from '@/components/crud/list-sort.js';
import { Pagination } from '@/components/crud/pagination.js';
import { RowActions } from '@/components/crud/row-actions.js';
import type { Comparators } from '@/components/crud/sort.js';
import { useListPage } from '@/components/crud/use-list-page.js';
import { SearchFilter } from '@/components/ui/search-filter.js';
import { Section } from '@/components/form/controls.js';
import type { WebKey } from '@/i18n/keys.js';
import { toMcpEnabledPatch, transportOf } from './mcp-form.js';
import { useMcpServers, useRemoveMcpServer } from './use-mcp.js';
import { useSaveSettings } from './use-settings.js';

/** One configured server, with whatever the client knows about it. */
interface ServerRow {
  readonly id: string;
  readonly transport: string;
  readonly enabled: boolean;
  readonly status: McpServerStatus | undefined;
}

type SortKey = 'name' | 'transport' | 'status';

/** All three are text, and text reads from A. See `sortBy`. */
const ASCENDING_FIRST: readonly SortKey[] = ['name', 'transport', 'status'];

/**
 * How loudly a row's state should read.
 *
 * `warning` and not `danger` for a failure, deliberately: a server that is
 * unreachable is an ordinary state — a laptop closed, a container not started —
 * and the panel is read by someone watching a machine. The loudest thing on the
 * screen should be something that went wrong, not something that is retrying.
 */
const TONES: Readonly<
  Record<McpServerStatus['state'], 'success' | 'warning' | 'neutral' | 'info'>
> = {
  ready: 'success',
  connecting: 'info',
  needs_authorization: 'warning',
  failed: 'warning',
  disabled: 'neutral',
};

const STATE_LABELS: Readonly<Record<McpServerStatus['state'], WebKey>> = {
  ready: 'settings.mcp.state.ready',
  connecting: 'settings.mcp.state.connecting',
  needs_authorization: 'settings.mcp.state.needsAuthorization',
  failed: 'settings.mcp.state.failed',
  disabled: 'settings.mcp.state.disabled',
};

/** Ranked for the sort, worst first: "what is broken" is one press away. */
const STATE_ORDER: Readonly<Record<McpServerStatus['state'], number>> = {
  failed: 0,
  needs_authorization: 1,
  connecting: 2,
  disabled: 3,
  ready: 4,
};

function rank(row: ServerRow): number {
  return row.status === undefined ? 2 : STATE_ORDER[row.status.state];
}

const COMPARE: Comparators<ServerRow, SortKey> = {
  name: (a, b) => a.id.localeCompare(b.id),
  transport: (a, b) => a.transport.localeCompare(b.transport),
  status: (a, b) => rank(a) - rank(b),
};

export function McpPanel({ config }: { readonly config: Config }): JSX.Element {
  const { t } = useTranslation();
  const [pendingDelete, setPendingDelete] = useState<ServerRow | undefined>(
    undefined,
  );
  const { save, saving } = useSaveSettings();
  const { remove, removing } = useRemoveMcpServer();
  const navigate = useNavigate();
  const live = useMcpServers();

  const byId = new Map(
    (live.data?.servers ?? []).map((server) => [server.id, server]),
  );
  const all: readonly ServerRow[] = Object.entries(config.tools.mcpServers).map(
    ([id, server]) => ({
      id,
      transport: transportOf(server),
      enabled: server.enabled,
      status: byId.get(id),
    }),
  );

  const { filter, setFilter, sort, setSort, matched, pagination, rows } =
    useListPage({
      rows: all,
      initialSort: { key: 'name', descending: false },
      // The transport is in the haystack because it is on screen under every
      // name: a list that shows a value it will not match on reads as broken.
      haystack: (row) => `${row.id} ${row.transport}`,
      comparators: COMPARE,
      tiebreak: (a, b) => a.id.localeCompare(b.id),
    });

  return (
    <div className="stack settings-panel">
      <Section
        title={t('settings.mcp.title')}
        description={t('settings.mcp.description')}
      >
        <div className="cluster">
          <span className="spacer" />
          {/* A link, not a dialog: adding a server is the same form as editing
              one, and nothing is written until it is saved. */}
          <Button asChild>
            <Link to="/settings/mcp/new">
              <Plus />
              {t('settings.mcp.newServer')}
            </Link>
          </Button>
        </div>

        {/* Only once there is something to narrow. A search box over two rows
            is furniture. */}
        {all.length > 0 && (
          <div className="row list-toolbar">
            <SearchFilter
              value={filter}
              label={t('settings.mcp.filter')}
              onValueChange={setFilter}
            />
            <ListSort
              options={[
                { key: 'name', label: t('common.name') },
                { key: 'transport', label: t('settings.mcp.transport') },
                { key: 'status', label: t('common.status') },
              ]}
              sort={sort}
              ascendingFirst={ASCENDING_FIRST}
              onChange={setSort}
            />
          </div>
        )}

        {all.length === 0 ? (
          <p className="page__note">{t('settings.mcp.none')}</p>
        ) : matched.length === 0 ? (
          <p className="page__note">{t('settings.mcp.noMatch', { filter })}</p>
        ) : (
          <DataList label={t('settings.mcp.title')}>
            {rows.map((row) => (
              <DataListRow
                key={row.id}
                primary={
                  <Link
                    to="/settings/mcp/$serverId"
                    params={{ serverId: row.id }}
                    className="data-list__open"
                    aria-label={t('settings.mcp.editServer', { name: row.id })}
                  >
                    <Plug />
                    <span className="row">
                      <span className="truncate">{row.id}</span>
                    </span>
                  </Link>
                }
                meta={
                  <>
                    <span className="data-list__code">{row.transport}</span>
                    {/* A badge with a word in it, not a bare coloured dot:
                        colour alone is the one encoding some readers do not
                        receive. */}
                    <Badge
                      tone={
                        row.status === undefined
                          ? 'neutral'
                          : TONES[row.status.state]
                      }
                    >
                      {row.status === undefined
                        ? t('settings.mcp.state.unknown')
                        : t(STATE_LABELS[row.status.state])}
                    </Badge>
                    {row.status !== undefined &&
                      row.status.tools.length > 0 && (
                        <span>
                          {t('settings.mcp.toolCount', {
                            count: row.status.tools.length,
                          })}
                        </span>
                      )}
                    {/* The sentence an operator came for. It is the only place
                        the reason exists — see `McpServerStatus.lastError`. */}
                    {row.status?.lastError !== undefined && (
                      <span className="data-list__detail truncate">
                        {row.status.lastError}
                      </span>
                    )}
                  </>
                }
                actions={
                  <RowActions label={row.id}>
                    <DropdownMenuItem
                      onSelect={() => {
                        void navigate({
                          to: '/settings/mcp/$serverId',
                          params: { serverId: row.id },
                        });
                      }}
                    >
                      <Pencil />
                      {t('common.edit')}
                    </DropdownMenuItem>
                    {/* Reversible where Delete is not, so it does not ask. */}
                    <DropdownMenuItem
                      disabled={saving}
                      onSelect={() => {
                        save(toMcpEnabledPatch(row.id, !row.enabled));
                      }}
                    >
                      {row.enabled ? <PowerOff /> : <Power />}
                      {row.enabled ? t('common.disable') : t('common.enable')}
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      className="menu__item--danger"
                      onSelect={() => {
                        setPendingDelete(row);
                      }}
                    >
                      <Trash2 />
                      {t('common.delete')}
                    </DropdownMenuItem>
                  </RowActions>
                }
              />
            ))}
          </DataList>
        )}

        <Pagination
          pagination={pagination}
          total={matched.length}
          label={t('settings.mcp.title')}
        />
      </Section>

      <ConfirmDialog
        open={pendingDelete !== undefined}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(undefined);
        }}
        title={t('settings.mcp.deleteTitle')}
        description={t('settings.mcp.deleteHint', {
          name: pendingDelete?.id ?? '',
        })}
        confirmLabel={t('common.delete')}
        pending={removing}
        onConfirm={() => {
          if (pendingDelete === undefined) return;
          // Closed on success, not on the press: a delete that failed should
          // leave the question on screen with its error.
          remove(pendingDelete.id, {
            onSuccess: () => {
              setPendingDelete(undefined);
            },
          });
        }}
      />
    </div>
  );
}
