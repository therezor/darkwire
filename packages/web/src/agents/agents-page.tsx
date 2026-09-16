/**
 * The agents index.
 *
 * Built out of the same chrome as Files and Workspaces, deliberately and not by
 * resemblance: the same `page page--wide` container, the same `cluster
 * page__header` with its actions on the right, the same `list-toolbar` and
 * `SearchFilter`, the same `DataList` rows with the same `ListSort` beside the
 * filter, and the same kebab. CRUD screens that merely *look* similar drift the
 * moment any of them is touched; ones that share the components cannot.
 *
 * Creating happens in a dialog rather than in a form pinned above the list —
 * again matching the other two. A permanent create form is a permanent piece of
 * furniture for something done once a month, and it pushes the list, which is
 * what the page is for, down.
 *
 * **Deleting is here, and it asks first.** At the bottom of the editor instead,
 * removing an agent would be a navigation away and removing the wrong one a
 * single unguarded click once you got there.
 *
 * Selecting an agent to *use* is not on this page. That is in the composer,
 * where the choice is actually made; this page is for keeping the list.
 */

import { useQuery } from '@tanstack/react-query';
import { Link, useNavigate } from '@tanstack/react-router';
import {
  BrainCircuit,
  Copy,
  Pencil,
  Plus,
  Power,
  PowerOff,
  Trash2,
} from 'lucide-react';
import { useMemo, useState, type JSX } from 'react';
import type { TFunction } from 'i18next';
import { useTranslation } from 'react-i18next';

import {
  AgentEntrySchema,
  DEFAULT_AGENT_ID,
  deriveAgentId,
  type AgentEntry,
} from '@darkwire/protocol';

import { Badge } from '@/components/ui/badge.js';
import { Button } from '@/components/ui/button.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { SearchFilter } from '@/components/ui/search-filter.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { RowActions } from '@/components/crud/row-actions.js';
import { DataList, DataListRow } from '@/components/crud/data-list.js';
import { ListSort } from '@/components/crud/list-sort.js';
import { Pagination } from '@/components/crud/pagination.js';
import { useListPage } from '@/components/crud/use-list-page.js';
import type { Comparators } from '@/components/crud/sort.js';
import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { useSaveSettings, useSettings } from '@/settings/use-settings.js';
import {
  toAgentDeletePatch,
  toAgentEnabledPatch,
  toNewAgentPatch,
} from './agents-form.js';
import { useAgent } from './agent-context.js';

/** One row of the list, with everything it renders already resolved. */
interface AgentRow {
  readonly id: string;
  readonly label: string;
  readonly model: string;
  readonly enabled: boolean;
  readonly entry: AgentEntry;
  readonly isDefault: boolean;
}

type SortKey = 'name' | 'model' | 'status';

/** Every column is text, so all three read from A. */
const ASCENDING_FIRST: readonly SortKey[] = ['name', 'model', 'status'];

/**
 * What the Status column says.
 *
 * A word rather than the absence of one. An `off` badge beside the name and
 * nothing at all when the agent is on reads as "we had nothing to say about
 * this row" rather than as "this one runs".
 */
function statusLabel(row: AgentRow): string {
  return row.enabled ? 'Enabled' : 'Disabled';
}

const COMPARE: Comparators<AgentRow, SortKey> = {
  name: (a, b) => a.label.localeCompare(b.label),
  model: (a, b) => a.model.localeCompare(b.model),
  // Compared as the label rather than as the boolean, so the column sorts the
  // way it reads: ascending is Disabled before Enabled, which is D before E.
  status: (a, b) => statusLabel(a).localeCompare(statusLabel(b)),
};

/**
 * What an agent is offering, in one line, without opening it.
 *
 * Counts rather than names. An agent's map holds every tool the install has, so
 * listing them would fill the column with the same five words on every row —
 * what differs between agents is how many of them are on, and how many of those
 * stop to ask.
 */
function summarise(entry: AgentEntry, t: TFunction): string {
  const permissions = Object.values(entry.tools);
  const enabled = permissions.filter(
    (permission) => permission !== 'deny',
  ).length;
  const asking = permissions.filter(
    (permission) => permission === 'ask',
  ).length;

  const parts = [t('agents.summaryTools', { count: enabled })];
  if (asking > 0) parts.push(t('agents.summaryAsking', { count: asking }));
  if (entry.systemPrompt.trim() !== '') {
    parts.push(t('agents.summaryOwnPrompt'));
  }
  return parts.join(' · ');
}

export function AgentsRoute(): JSX.Element {
  const { t } = useTranslation();
  const settings = useSettings();
  const { save, saving } = useSaveSettings();
  const navigate = useNavigate();
  const { agentId: active, select } = useAgent();

  const [pendingDelete, setPendingDelete] = useState<AgentRow | undefined>(
    undefined,
  );

  const agents = useQuery({
    queryKey: queryKeys.agents,
    queryFn: ({ signal }) => api.agents(signal),
  });

  const list = settings.data?.config.agents.list ?? {};

  /**
   * The default first, then the operator's order.
   *
   * `default` is named explicitly rather than trusted to be in `list`. The
   * schema prefaults an entry for it, so in practice it always is — but it is
   * the agent every unbound conversation runs on, and a list that could omit the
   * one actually in use is not worth the line it would save.
   */
  const ids = useMemo(
    () => [
      DEFAULT_AGENT_ID,
      ...Object.keys(list).filter((id) => id !== DEFAULT_AGENT_ID),
    ],
    [list],
  );

  /** The agents that delegate to the one about to be deleted, by label. */
  const dependants = useMemo((): readonly string[] => {
    const target = pendingDelete?.id;
    if (target === undefined) return [];
    return Object.entries(list)
      .filter(
        ([id, entry]) =>
          id !== target && entry.subagents.some((ref) => ref.id === target),
      )
      .map(([id, entry]) => (entry.label === '' ? id : entry.label));
  }, [list, pendingDelete]);

  const all = useMemo((): readonly AgentRow[] => {
    const resolved = agents.data?.agents ?? [];
    return ids.map((id) => {
      const entry = list[id] ?? AgentEntrySchema.parse({});
      // `/api/agents` reports the model a turn would actually use, but it omits
      // the disabled ones entirely — so the stored value is the fallback, or a
      // switched-off agent would show a dash where its model is.
      const model =
        resolved.find((agent) => agent.id === id)?.model ?? entry.model;
      const isDefault = id === DEFAULT_AGENT_ID;
      return {
        id,
        label: entry.label === '' ? id : entry.label,
        model: model === '' ? '—' : model,
        // The default agent's own flag is ignored by the resolver — an install
        // with no agent at all is not a state anything downstream can serve —
        // so the column reports what is true rather than what is stored.
        enabled: isDefault || entry.enabled,
        entry,
        isDefault,
      };
    });
  }, [ids, list, agents.data]);

  // The agent list is a slice of the settings tree, so it arrives whole — the
  // page is a slice of what is already here rather than a second request.
  const { filter, setFilter, sort, setSort, matched, pagination, rows } =
    useListPage({
      rows: all,
      initialSort: { key: 'name', descending: false },
      haystack: (row) => `${row.id} ${row.label}`,
      comparators: COMPARE,
      // The default is the agent every other one was created as a copy of, so it
      // heads the list in both directions rather than sorting among the agents it
      // seeded.
      group: (row) => (row.isDefault ? 0 : 1),
      tiebreak: (a, b) => a.label.localeCompare(b.label),
    });

  const taken = useMemo(() => new Set(ids), [ids]);

  const open = (id: string): void => {
    void navigate({ to: '/agents/$agentId', params: { agentId: id } });
  };

  /**
   * Opens the editor once the agent it names actually exists.
   *
   * **On success, not on the next line.** `save` is fire-and-forget, so
   * navigating straight after it took the editor to an agent the settings cache
   * had never seen — which is the "There is no agent called …" path, and is why
   * duplicating one appeared to do nothing at all.
   */
  const createThenOpen = (
    id: string,
    patch: ReturnType<typeof toNewAgentPatch>,
  ): void => {
    save(patch, {
      onSuccess: () => {
        open(id);
      },
    });
  };

  /**
   * A copy, under a name that does not collide.
   *
   * The suffix is counted rather than fixed: "Reviewer copy" is taken the
   * second time, and a duplicate that silently did nothing because of it was
   * indistinguishable from one that was broken.
   */
  const duplicate = (row: AgentRow): void => {
    let label = `${row.label} copy`;
    let copyId = deriveAgentId(label);
    for (let n = 2; taken.has(copyId); n += 1) {
      label = `${row.label} copy ${String(n)}`;
      copyId = deriveAgentId(label);
    }

    createThenOpen(copyId, toNewAgentPatch(copyId, label, row.entry));
  };

  /**
   * Switching one off, which is the reversible half of deleting it.
   *
   * Its prompt, tools and permissions stay in the settings; it simply stops
   * being something a turn can run on, and stops being offered in the composer.
   */
  const toggleEnabled = (row: AgentRow): void => {
    save(toAgentEnabledPatch(row.id, row.entry, !row.enabled));
    // Same reason a delete moves the selection: an agent that has just been
    // switched off is one the server will refuse, so nothing may stay pointed
    // at it.
    if (row.enabled && active === row.id) select(DEFAULT_AGENT_ID);
  };

  const remove = (row: AgentRow): void => {
    save(toAgentDeletePatch(row.id));
    setPendingDelete(undefined);
    // Anything pointed at the agent that just went has to move, or the next
    // conversation would name one the server will refuse.
    if (active === row.id) select(DEFAULT_AGENT_ID);
  };

  return (
    <div className="stack page page--wide">
      <div className="cluster page__header">
        <h1 className="page__title">{t('agents.title')}</h1>
        <span className="spacer" />
        {/* A link, not a dialog: creating an agent is the same form as editing
            one, and nothing is written until it is saved. */}
        <Button asChild>
          <Link to="/agents/new">
            <Plus />
            {t('agents.newAgent')}
          </Link>
        </Button>
      </div>

      <p className="page__note">{t('agents.note')}</p>

      <div className="row list-toolbar">
        <SearchFilter
          value={filter}
          label={t('agents.filter')}
          onValueChange={setFilter}
        />
        <ListSort
          options={[
            { key: 'name', label: t('common.name') },
            { key: 'model', label: t('agents.model') },
            { key: 'status', label: t('common.status') },
          ]}
          sort={sort}
          ascendingFirst={ASCENDING_FIRST}
          onChange={setSort}
        />
      </div>

      {settings.isPending && (
        <p className="page__note">{t('common.loading')}</p>
      )}
      {settings.isError && (
        <p role="alert" className="page__error">
          {t('agents.loadError', { message: settings.error.message })}
        </p>
      )}

      {settings.isSuccess &&
        (matched.length === 0 ? (
          <p className="page__note">{t('agents.noMatch', { filter })}</p>
        ) : (
          <DataList label={t('agents.title')}>
            {rows.map((row) => (
              <DataListRow
                key={row.id}
                primary={
                  <Link
                    to="/agents/$agentId"
                    params={{ agentId: row.id }}
                    className="data-list__open"
                    aria-label={`Edit ${row.label}`}
                  >
                    <BrainCircuit />
                    <span className="stack agents__name">
                      <span className="agents__name-row">
                        <span className="truncate">{row.label}</span>
                        {row.isDefault && <Badge>default</Badge>}
                      </span>
                      <span className="agents__summary truncate">
                        {summarise(row.entry, t)}
                      </span>
                    </span>
                  </Link>
                }
                meta={
                  <>
                    <span className="data-list__code">{row.model}</span>
                    <Badge tone={row.enabled ? 'success' : 'neutral'}>
                      {statusLabel(row)}
                    </Badge>
                  </>
                }
                actions={
                  <RowActions label={row.label}>
                    <DropdownMenuItem
                      onSelect={() => {
                        open(row.id);
                      }}
                    >
                      <Pencil />
                      Edit
                    </DropdownMenuItem>
                    {/* A copy is the fastest route to a variant of something
                          that already works, which is most of what a second
                          agent is. */}
                    <DropdownMenuItem
                      onSelect={() => {
                        duplicate(row);
                      }}
                    >
                      <Copy />
                      Duplicate
                    </DropdownMenuItem>
                    {/* No Rename. The name is a field in the editor, which is
                          one press away and is where every other thing about
                          an agent is changed — a second way to edit one field,
                          with its own dialog and its own patch builder, was a
                          shortcut that had to be kept correct twice. */}
                    {/* Reversible where Delete is not: a disabled agent keeps
                          its prompt and permissions and simply stops being
                          something a turn can run on. No confirmation, for the
                          same reason — switching it back on is the same click. */}
                    {!row.isDefault && (
                      <DropdownMenuItem
                        onSelect={() => {
                          toggleEnabled(row);
                        }}
                      >
                        {row.enabled ? <PowerOff /> : <Power />}
                        {row.enabled ? 'Disable' : 'Enable'}
                      </DropdownMenuItem>
                    )}
                    {/* Switching the default off would leave an install with
                          no agent at all, which is not a state anything
                          downstream can serve. */}
                    {!row.isDefault && (
                      <DropdownMenuItem
                        className="menu__item--danger"
                        onSelect={() => {
                          setPendingDelete(row);
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

      {settings.isSuccess && (
        <Pagination
          pagination={pagination}
          total={matched.length}
          label={t('agents.title')}
        />
      )}

      <ConfirmDialog
        open={pendingDelete !== undefined}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(undefined);
        }}
        title={t('agents.deleteTitle')}
        // The delegation sentence is only shown when there is one, and it names
        // the agents rather than saying "some": the server strips those
        // references as part of the delete, and an edit made on the operator's
        // behalf should be one they were told about before they agreed to it.
        description={
          `${pendingDelete?.label ?? ''} is removed from the settings. ` +
          'Its sessions keep their history and fall back to the default agent.' +
          (dependants.length === 0
            ? ''
            : ` ${dependants.join(' and ')} ${dependants.length === 1 ? 'delegates' : 'delegate'} to it; that delegation is removed too.`)
        }
        confirmLabel="Delete"
        pending={saving}
        onConfirm={() => {
          if (pendingDelete !== undefined) remove(pendingDelete);
        }}
      />
    </div>
  );
}
