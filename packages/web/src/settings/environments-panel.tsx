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
 * **Any installed environment can be warmed, from its own row.** An instance is
 * keyed on the workspace, the definition and the network, and neither the agent
 * nor the conversation is part of it, so one started here is the one a turn goes
 * on to reuse. The action sits in the row's menu beside Edit and Delete because
 * the row already says which definition it is about: a picker somewhere else
 * would ask again for something the operator had just pointed at.
 */

import { useState, type JSX } from 'react';
import {
  useMutation,
  useQuery,
  useQueryClient,
  type UseMutationResult,
} from '@tanstack/react-query';
import { Link, useNavigate } from '@tanstack/react-router';
import {
  Box,
  Pencil,
  Play,
  Plus,
  RotateCw,
  Square,
  Trash2,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';

import type { EnvironmentSummary } from '@darkwire/protocol';

import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { Badge } from '@/components/ui/badge.js';
import { Button } from '@/components/ui/button.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { SearchFilter } from '@/components/ui/search-filter.js';
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogHeading,
  DialogSubheading,
} from '@/components/ui/dialog.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { DataList, DataListRow } from '@/components/crud/data-list.js';
import { ListSort } from '@/components/crud/list-sort.js';
import { Pagination } from '@/components/crud/pagination.js';
import { RowActions } from '@/components/crud/row-actions.js';
import type { Comparators } from '@/components/crud/sort.js';
import { useListPage } from '@/components/crud/use-list-page.js';
import { Section, SelectField } from '@/components/form/controls.js';
import { useEnvironments, useRemoveEnvironment } from './use-environment.js';

/**
 * Starting, stopping and restarting a container, all one endpoint.
 *
 * Shared by the two halves rather than written twice: they invalidate the same
 * query, and a second copy of that is the one that eventually forgets to.
 */
function useSandbox(): UseMutationResult<
  Awaited<ReturnType<typeof api.manageSandbox>>,
  Error,
  Parameters<typeof api.manageSandbox>[0]
> {
  const client = useQueryClient();
  return useMutation({
    mutationFn: api.manageSandbox,
    onSuccess: async () => {
      await client.invalidateQueries({
        queryKey: queryKeys.environmentInstances,
      });
    },
  });
}

type SortKey = 'name' | 'image';

/**
 * `node@sha256:8a34…` as `node`.
 *
 * A digest is sixty-four characters that tell two definitions apart only in
 * the pathological case where they share a tag, and on a row it wrapped to
 * three lines and pushed everything else down. The editor still shows the
 * whole thing, because that is the screen where the exact image is the
 * question being asked.
 */
function imageName(image: string): string {
  const at = image.indexOf('@');
  return at === -1 ? image : image.slice(0, at);
}

/** Both are text, and text reads from A. */
const ASCENDING_FIRST: readonly SortKey[] = ['name', 'image'];

const COMPARE: Comparators<EnvironmentSummary, SortKey> = {
  name: (a, b) => a.name.localeCompare(b.name),
  image: (a, b) =>
    imageName(a.definition?.image ?? '').localeCompare(
      imageName(b.definition?.image ?? ''),
    ),
};

export function EnvironmentsPanel(): JSX.Element {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const installed = useEnvironments();
  const { remove, removing } = useRemoveEnvironment();
  const [pendingDelete, setPendingDelete] = useState<
    EnvironmentSummary | undefined
  >(undefined);
  /** The row whose menu asked to warm one, and the workspace to warm it in. */
  const [pendingStart, setPendingStart] = useState<
    EnvironmentSummary | undefined
  >(undefined);
  const [workspace, setWorkspace] = useState('');
  const workspaces = useQuery({
    queryKey: queryKeys.workspaces,
    queryFn: ({ signal }) => api.workspaces(signal),
  });
  const start = useSandbox();

  // The default workspace until someone picks another, derived rather than
  // synced into state: an effect that writes the list's answer back would
  // render twice on arrival and fight anyone who cleared the box in between.
  const fallback = workspaces.data?.workspaces.find((entry) => entry.isDefault);
  const chosen = workspace === '' ? (fallback?.id ?? '') : workspace;

  const all = installed.data?.environments ?? [];
  const { filter, setFilter, sort, setSort, matched, pagination, rows } =
    useListPage({
      rows: all,
      initialSort: { key: 'name', descending: false },
      // The image is in the haystack because it is on screen under every name,
      // and a list that shows a value it will not match on reads as broken.
      haystack: (entry) =>
        `${entry.name} ${imageName(entry.definition?.image ?? '')}`,
      comparators: COMPARE,
      tiebreak: (a, b) => a.name.localeCompare(b.name),
    });

  return (
    <div className="stack settings-panel">
      <Section
        title={t('settings.environments.installedTitle')}
        description={t('settings.environments.installedDescription')}
      >
        {installed.isPending && (
          <p className="page__note">{t('settings.environments.loading')}</p>
        )}
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
                          {imageName(entry.definition.image)}
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
                    {/* A file that did not parse cannot be started either: the
                        service re-reads the definition before it builds an
                        argv, and would refuse the same way. */}
                    <DropdownMenuItem
                      disabled={entry.definition === undefined}
                      onSelect={() => {
                        setPendingStart(entry);
                      }}
                    >
                      <Play />
                      {t('settings.environments.start')}
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
        {/* Only the workspace, because the row said which definition. An
            instance is keyed on the two of them, so this is the whole
            question. */}
        <Dialog
          open={pendingStart !== undefined}
          onOpenChange={(open) => {
            if (!open) setPendingStart(undefined);
          }}
        >
          <DialogContent>
            <DialogHeader>
              <DialogHeading>
                {t('settings.environments.startTitle')}
              </DialogHeading>
              <DialogSubheading>
                {t('settings.environments.startHint', {
                  name: pendingStart?.name ?? '',
                })}
              </DialogSubheading>
            </DialogHeader>

            <SelectField
              label={t('settings.environments.workspace')}
              value={chosen}
              placeholder={t('settings.environments.selectWorkspace')}
              options={(workspaces.data?.workspaces ?? []).map((entry) => ({
                value: entry.id,
                label: entry.name,
              }))}
              onValueChange={setWorkspace}
            />

            {start.error && (
              <p role="alert" className="page__error">
                {start.error.message}
              </p>
            )}

            <DialogFooter>
              <Button
                variant="ghost"
                onClick={() => {
                  setPendingStart(undefined);
                }}
              >
                {t('common.cancel')}
              </Button>
              <Button
                variant="primary"
                disabled={!chosen || start.isPending}
                onClick={() => {
                  if (pendingStart === undefined) return;
                  start.mutate(
                    {
                      op: 'start',
                      environment: pendingStart.name,
                      workspace: chosen,
                      agent: 'operator',
                      session: 'operator',
                      // No network, which is also an agent's default. An
                      // instance's egress is part of its identity, so warming
                      // with one an agent did not ask for would start a
                      // container nothing reuses.
                      network: { mode: 'none', allow: [], hosts: [], dns: [] },
                    },
                    // Closed on success, not on the press: a refusal should
                    // leave the question on screen with the sentence for it.
                    {
                      onSuccess: () => {
                        setPendingStart(undefined);
                      },
                    },
                  );
                }}
              >
                {t('settings.environments.start')}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      </Section>

      <RunningContainers />
    </div>
  );
}

/**
 * The containers the environment service is holding open.
 *
 * **Absent until one is.** A section whose whole content was "no containers are
 * running" was a card explaining its own emptiness on every visit, and that is
 * the state this panel is in nearly all the time: a container starts on its own
 * when a tool first needs one, so an operator has nothing to do about it.
 *
 * Lifecycle only, and nothing here is gated: stopping a busy instance costs the
 * command in flight and nothing else, because the next one starts a fresh
 * container from the same definition. Starting one is on the definition's own
 * row above, which is where the answer to "which environment" already is.
 */
function RunningContainers(): JSX.Element | undefined {
  const { t } = useTranslation();
  const instances = useQuery({
    queryKey: queryKeys.environmentInstances,
    queryFn: ({ signal }) => api.sandboxes(signal),
    refetchInterval: 5000,
    retry: false,
  });
  const mutation = useSandbox();

  const running = instances.data?.instances ?? [];
  // The error still has to reach somebody: a service that stopped answering is
  // the one case where an empty list means something went wrong rather than
  // nothing is running.
  if (running.length === 0 && !instances.error) return undefined;

  return (
    <Section
      title={t('settings.environments.runningTitle')}
      description={t('settings.environments.runningDescription')}
    >
      {instances.error && (
        <p role="status" className="page__note">
          {instances.error.message}
        </p>
      )}
      {mutation.error && (
        <p role="alert" className="page__error">
          {mutation.error.message}
        </p>
      )}

      {running.length > 0 && (
        <DataList label={t('settings.environments.runningTitle')}>
          {running.map((instance) => (
            <DataListRow
              key={instance.id}
              primary={
                <span className="data-list__open">
                  <Box />
                  <span className="truncate data-list__code">
                    {instance.id}
                  </span>
                </span>
              }
              meta={
                <>
                  {/* The workspace, and nothing about who is using it: the
                      agents holding a container change between two polls, so a
                      list that named them rewrote itself while being read. */}
                  <span>
                    {t('settings.environments.inWorkspace', {
                      workspace: instance.workspace,
                    })}
                  </span>
                  {/* A badge only while something is running in it, because
                      "0 active commands" on every idle row is a column of
                      noise saying nothing changed. */}
                  {instance.busy > 0 && (
                    <Badge tone="warning">
                      {t('settings.environments.activeCommands', {
                        count: instance.busy,
                      })}
                    </Badge>
                  )}
                </>
              }
              actions={
                <RowActions label={instance.id}>
                  <DropdownMenuItem
                    disabled={mutation.isPending}
                    onSelect={() => {
                      mutation.mutate({ op: 'restart', instance: instance.id });
                    }}
                  >
                    <RotateCw />
                    {t('settings.environments.restart')}
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    disabled={mutation.isPending}
                    onSelect={() => {
                      mutation.mutate({ op: 'stop', instance: instance.id });
                    }}
                  >
                    <Square />
                    {t('settings.environments.stop')}
                  </DropdownMenuItem>
                </RowActions>
              }
            />
          ))}
        </DataList>
      )}
    </Section>
  );
}
