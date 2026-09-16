/**
 * The environments an operator installed, and the containers held open for them.
 *
 * Two halves, and the split is what the operator can act on. The **installed**
 * list is the policy: each row is a file under the policy directory, and the
 * editor behind it is the only door into that directory the browser has. The
 * **running** list is lifecycle only. Stopping an instance gets a fresh one on
 * the next command, from the same definition.
 *
 * Read from the server on every visit rather than cached across one: an edited
 * definition must report its new capabilities and its new gateway verdict the
 * moment it changes, and an operator who just fixed one is the likeliest visitor.
 *
 * **Any installed environment can be warmed.** An instance is keyed on the
 * workspace, the definition and the network, all of which this form supplies,
 * and neither the agent nor the conversation is part of it. So one started here
 * is the one a turn goes on to reuse.
 */

import { useState, type JSX } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Link, useNavigate } from '@tanstack/react-router';
import { Box, Pencil, Plus, Trash2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';

import type { EnvironmentSummary } from '@darkwire/protocol';

import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { Badge } from '@/components/ui/badge.js';
import { Button } from '@/components/ui/button.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { SearchFilter } from '@/components/ui/search-filter.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { DataList, DataListRow } from '@/components/crud/data-list.js';
import { ListSort } from '@/components/crud/list-sort.js';
import { Pagination } from '@/components/crud/pagination.js';
import { RowActions } from '@/components/crud/row-actions.js';
import type { Comparators } from '@/components/crud/sort.js';
import { useListPage } from '@/components/crud/use-list-page.js';
import { Section } from '@/components/form/controls.js';
import { useEnvironments, useRemoveEnvironment } from './use-environment.js';

type SortKey = 'name' | 'image';

/** Both are text, and text reads from A. */
const ASCENDING_FIRST: readonly SortKey[] = ['name', 'image'];

const COMPARE: Comparators<EnvironmentSummary, SortKey> = {
  name: (a, b) => a.name.localeCompare(b.name),
  image: (a, b) =>
    (a.definition?.image ?? '').localeCompare(b.definition?.image ?? ''),
};

export function EnvironmentsPanel(): JSX.Element {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const installed = useEnvironments();
  const { remove, removing } = useRemoveEnvironment();
  const [pendingDelete, setPendingDelete] = useState<
    EnvironmentSummary | undefined
  >(undefined);

  const all = installed.data?.environments ?? [];
  const { filter, setFilter, sort, setSort, matched, pagination, rows } =
    useListPage({
      rows: all,
      initialSort: { key: 'name', descending: false },
      // The image is in the haystack because it is on screen under every name,
      // and a list that shows a value it will not match on reads as broken.
      haystack: (entry) => `${entry.name} ${entry.definition?.image ?? ''}`,
      comparators: COMPARE,
      tiebreak: (a, b) => a.name.localeCompare(b.name),
    });

  return (
    <>
      <Section
        title={t('settings.environments.installedTitle')}
        description={t('settings.environments.installedDescription')}
      >
        {installed.isPending && <p>{t('settings.environments.loading')}</p>}
        {installed.isError && (
          <p role="alert" className="page__error">
            {t('settings.environments.loadError', {
              message: installed.error.message,
            })}
          </p>
        )}

        {/* Trailing and in the default variant, like "New agent" and "New
            provider". A link, not a dialog: creating one is the same form as
            editing one, and nothing is written until it is saved. */}
        <div className="cluster">
          <span className="spacer" />
          <Button asChild>
            <Link to="/settings/environments/new">
              <Plus />
              {t('settings.environments.newEnvironment')}
            </Link>
          </Button>
        </div>

        {/* Only once there is something to narrow. A search box over two rows is
            furniture, and the empty-state sentence reads better without one. */}
        {all.length > 0 && (
          <div className="row list-toolbar">
            <SearchFilter
              value={filter}
              label={t('settings.environments.filter')}
              onValueChange={setFilter}
            />
            <ListSort
              options={[
                { key: 'name', label: t('common.name') },
                { key: 'image', label: t('settings.environments.image') },
              ]}
              sort={sort}
              ascendingFirst={ASCENDING_FIRST}
              onChange={setSort}
            />
          </div>
        )}

        {!installed.isPending && all.length === 0 ? (
          // Says what to do rather than rendering an empty list. A blank panel
          // reads as a broken screen; this reads as a step not taken yet.
          <p className="page__note">
            {t('settings.environments.noneInstalled')}
          </p>
        ) : matched.length === 0 && all.length > 0 ? (
          <p className="page__note">
            {t('settings.environments.noMatch', { filter })}
          </p>
        ) : (
          <DataList label={t('settings.environments.installedTitle')}>
            {rows.map((entry) => (
              <DataListRow
                key={entry.name}
                primary={
                  // A file that did not parse has nothing to load into the
                  // form, so it is named rather than opened. Deleting it is the
                  // way out, and that stays in the kebab.
                  entry.definition === undefined ? (
                    <span className="data-list__open">
                      <Box />
                      <span className="truncate">{entry.name}</span>
                    </span>
                  ) : (
                    <Link
                      to="/settings/environments/$name"
                      params={{ name: entry.name }}
                      className="data-list__open"
                      aria-label={t('settings.environments.editOne', {
                        name: entry.name,
                      })}
                    >
                      <Box />
                      <span className="truncate">{entry.name}</span>
                    </Link>
                  )
                }
                meta={
                  <>
                    {entry.definition !== undefined && (
                      <>
                        {/* The image is what tells two definitions apart, so it
                            stays on the row. It breaks anywhere it has to: a
                            digest has no spaces to break at. */}
                        <span className="data-list__code">
                          {entry.definition.image}
                        </span>
                        <span>
                          {t('settings.environments.limitsLine', {
                            memory: entry.definition.limits.memoryMb,
                            cpus: entry.definition.limits.cpus,
                            pids: entry.definition.limits.pidsMax,
                          })}
                        </span>
                      </>
                    )}
                    {/* Warnings, not refusals: a weakened definition is usable,
                        and an operator who chose the weakening deserves to be
                        reminded rather than blocked. */}
                    {entry.weakened.length > 0 && (
                      <Badge tone="warning">
                        {t('settings.environments.weakened', {
                          what: entry.weakened.join(', '),
                        })}
                      </Badge>
                    )}
                    {entry.gatewayProblem !== undefined && (
                      <Badge tone="warning">
                        {t('settings.environments.gatewayProblem', {
                          why: entry.gatewayProblem,
                        })}
                      </Badge>
                    )}
                    {entry.problem !== undefined && (
                      <span role="alert">{entry.problem}</span>
                    )}
                  </>
                }
                actions={
                  <RowActions label={entry.name}>
                    {/* A file that did not parse has nothing to load into the
                        form, so the way out of one is the delete below. */}
                    <DropdownMenuItem
                      disabled={entry.definition === undefined}
                      onSelect={() => {
                        void navigate({
                          to: '/settings/environments/$name',
                          params: { name: entry.name },
                        });
                      }}
                    >
                      <Pencil />
                      {t('common.edit')}
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      className="menu__item--danger"
                      onSelect={() => {
                        setPendingDelete(entry);
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
          label={t('settings.environments.installedTitle')}
        />

        <ConfirmDialog
          open={pendingDelete !== undefined}
          onOpenChange={(open) => {
            if (!open) setPendingDelete(undefined);
          }}
          title={t('settings.environments.deleteTitle')}
          description={t('settings.environments.deleteHint', {
            name: pendingDelete?.name ?? '',
          })}
          confirmLabel={t('common.delete')}
          pending={removing}
          onConfirm={() => {
            if (pendingDelete === undefined) return;
            // Closed on success, not on the press: a delete the server refuses
            // because an agent still uses it should leave the question on
            // screen with that sentence.
            remove(pendingDelete.name, {
              onSuccess: () => {
                setPendingDelete(undefined);
              },
            });
          }}
        />
      </Section>

      {all.length > 0 && <RunningContainers />}
    </>
  );
}

/**
 * The containers the environment service is holding open.
 *
 * Lifecycle only, and nothing here is gated: stopping a busy instance costs the
 * command in flight and nothing else, because the next one starts a fresh
 * container from the same definition.
 */
function RunningContainers(): JSX.Element {
  const { t } = useTranslation();
  const client = useQueryClient();
  const [environment, setEnvironment] = useState('');
  const [workspace, setWorkspace] = useState('default');

  const installed = useEnvironments();
  // Every installed definition, because every environment is shared now. The
  // filter this replaced kept only `shared: true` ones, and with that flag gone
  // it would have quietly offered nothing forever.
  const warmable = (installed.data?.environments ?? []).filter(
    (entry) => entry.definition !== undefined,
  );
  const instances = useQuery({
    queryKey: queryKeys.environmentInstances,
    queryFn: ({ signal }) => api.sandboxes(signal),
    refetchInterval: 5000,
    retry: false,
  });
  const mutation = useMutation({
    mutationFn: api.manageSandbox,
    onSuccess: async () => {
      await client.invalidateQueries({
        queryKey: queryKeys.environmentInstances,
      });
    },
  });

  return (
    <Section
      title={t('settings.environments.runningTitle')}
      description={t('settings.environments.runningDescription')}
    >
      {instances.error && <p role="status">{instances.error.message}</p>}
      {mutation.error && <p role="alert">{mutation.error.message}</p>}
      {warmable.length > 0 && (
        <>
          <label>
            {t('settings.environments.environment')}{' '}
            <select
              value={environment}
              onChange={(event) => {
                setEnvironment(event.target.value);
              }}
            >
              <option value="">{t('settings.environments.select')}</option>
              {warmable.map((entry) => (
                <option key={entry.name} value={entry.name}>
                  {entry.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            {t('settings.environments.workspace')}{' '}
            <input
              value={workspace}
              onChange={(event) => {
                setWorkspace(event.target.value);
              }}
            />
          </label>
          <Button
            disabled={!environment || !workspace || mutation.isPending}
            onClick={() => {
              mutation.mutate({
                op: 'start',
                environment,
                workspace,
                agent: 'operator',
                session: 'operator',
                // No network, which is also an agent's default. An instance's
                // egress is part of its identity, so warming with one an agent
                // did not ask for would start a container nothing reuses.
                network: { mode: 'none', allow: [], hosts: [], dns: [] },
              });
            }}
          >
            {t('settings.environments.start')}
          </Button>
        </>
      )}
      {instances.data?.instances.length === 0 && (
        <p>{t('settings.environments.noneRunning')}</p>
      )}
      <ul className="stack">
        {instances.data?.instances.map((instance) => (
          <li key={instance.id}>
            <p>
              <code>{instance.id}</code> · {instance.workspace} ·{' '}
              {instance.environment} ·{' '}
              {t('settings.environments.activeCommands', {
                count: instance.busy,
              })}
            </p>
            {instance.agents.length > 0 && (
              <p>
                {t('settings.environments.agents', {
                  agents: instance.agents.join(', '),
                })}
              </p>
            )}
            <Button
              disabled={mutation.isPending}
              onClick={() => {
                mutation.mutate({ op: 'stop', instance: instance.id });
              }}
            >
              {t('settings.environments.stop')}
            </Button>
            <Button
              disabled={mutation.isPending}
              onClick={() => {
                mutation.mutate({ op: 'restart', instance: instance.id });
              }}
            >
              {t('settings.environments.restart')}
            </Button>
          </li>
        ))}
      </ul>
    </Section>
  );
}
