/**
 * The Providers panel: the list, and the way into each endpoint.
 *
 * The list is what the operator has *configured*, not what the registry knows,
 * and that inversion is the whole point: a provider is an endpoint that exists
 * because someone added it, not a row that always exists and might have
 * settings. Two Ollama servers are two rows.
 *
 * Shaped like Agents, because it is the same kind of thing: a list that picks,
 * a `RowActions` kebab for the acts that need no form, and **an editor route**
 * for the rest. Creating one asks the single question the editor cannot — the
 * type, which is fixed for the life of the instance — and then opens the editor,
 * exactly as "New agent" asks for a name and opens that.
 *
 * There is no "refresh every provider's models" button here any more. It asked
 * every configured endpoint at once, so one closed laptop made the whole action
 * look broken, and it answered a question nobody on this screen was asking.
 * Fetching a catalogue is a thing you do *to an endpoint*, and it lives in that
 * endpoint's editor.
 *
 * **It has the toolbar now, which it was alone in not having.** This was the one
 * `DataList` screen with no filter and no sort — not a decision, just the screen
 * that shipped before those moved into `components/crud`. "Shaped like Agents"
 * has to include the row of controls above the rows, or the resemblance stops at
 * the parts someone remembered to copy.
 */

import { useQuery } from '@tanstack/react-query';
import { Pencil, Plug, Plus, Power, PowerOff, Trash2 } from 'lucide-react';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { Link, useNavigate } from '@tanstack/react-router';
import type { ProviderInstanceInfo } from '@darkwire/protocol';

import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
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
import { toProviderEnabledPatch } from './provider-form.js';
import { useRemoveProvider } from './use-provider.js';
import { useSaveSettings } from './use-settings.js';

type SortKey = 'name' | 'endpoint' | 'status';

/** All three are text, and text reads from A. See `sortBy`. */
const ASCENDING_FIRST: readonly SortKey[] = ['name', 'endpoint', 'status'];

const COMPARE: Comparators<ProviderInstanceInfo, SortKey> = {
  name: (a, b) => a.displayName.localeCompare(b.displayName),
  endpoint: (a, b) => a.apiBase.localeCompare(b.apiBase),
  // Disabled first ascending, so "what is switched off" is one press away —
  // which is the question this column is ever asked.
  status: (a, b) => Number(a.enabled) - Number(b.enabled),
};

export function ProvidersPanel(): JSX.Element {
  const { t } = useTranslation();
  const [pendingDelete, setPendingDelete] = useState<
    ProviderInstanceInfo | undefined
  >(undefined);
  const { save, saving } = useSaveSettings();
  const { remove, removing } = useRemoveProvider();
  const navigate = useNavigate();

  const providers = useQuery({
    queryKey: queryKeys.providers,
    queryFn: ({ signal }) => api.providers(signal),
  });

  const all = providers.data?.instances ?? [];
  const { filter, setFilter, sort, setSort, matched, pagination, rows } =
    useListPage({
      rows: all,
      initialSort: { key: 'name', descending: false },
      // The endpoint is in the haystack because it is on screen under every name:
      // a list that shows a value it will not match on reads as broken.
      haystack: (instance) => `${instance.displayName} ${instance.apiBase}`,
      comparators: COMPARE,
      tiebreak: (a, b) => a.displayName.localeCompare(b.displayName),
    });

  if (providers.isPending) {
    return <p className="page__note">{t('providers.loading')}</p>;
  }
  if (providers.isError) {
    return (
      <p role="alert" className="page__error">
        {t('providers.loadError', { message: providers.error.message })}
      </p>
    );
  }

  return (
    <Section
      title={t('providers.title')}
      description={t('providers.panelDesc')}
    >
      {/* Trailing and in the default variant, like "New agent" and "New
          workspace". A primary fill on the one control that opens a dialog made
          the create the loudest thing in a panel whose subject is the list. */}
      <div className="cluster">
        <span className="spacer" />
        {/* A link, not a dialog: adding an endpoint is the same form as
            editing one, and nothing is written until it is saved. */}
        <Button asChild>
          <Link to="/settings/providers/new">
            <Plus />
            {t('providers.newProvider')}
          </Link>
        </Button>
      </div>

      {/* Only once there is something to narrow. A search box over two rows is
          furniture, and the empty-state sentence below reads better without a
          control above it implying there is a list to filter. */}
      {all.length > 0 && (
        <div className="row list-toolbar">
          <SearchFilter
            value={filter}
            label={t('providers.filter')}
            onValueChange={setFilter}
          />
          <ListSort
            options={[
              { key: 'name', label: t('common.name') },
              { key: 'endpoint', label: t('providers.endpoint') },
              { key: 'status', label: t('common.status') },
            ]}
            sort={sort}
            ascendingFirst={ASCENDING_FIRST}
            onChange={setSort}
          />
        </div>
      )}

      {all.length === 0 ? (
        <p className="page__note">{t('providers.none')}</p>
      ) : matched.length === 0 ? (
        <p className="page__note">{t('providers.noMatch', { filter })}</p>
      ) : (
        <DataList label={t('providers.title')}>
          {rows.map((instance) => (
            <DataListRow
              key={instance.id}
              primary={
                <Link
                  to="/settings/providers/$instanceId"
                  params={{ instanceId: instance.id }}
                  className="data-list__open"
                  aria-label={`Edit ${instance.displayName}`}
                >
                  <Plug />
                  <span className="row">
                    <span className="truncate">{instance.displayName}</span>
                    {instance.isLocal && <Badge tone="neutral">local</Badge>}
                    {instance.isGateway && <Badge tone="info">gateway</Badge>}
                  </span>
                </Link>
              }
              meta={
                <>
                  {/* The endpoint is what tells two Ollama servers apart, so it
                      stays on the row rather than being shed on a phone. It
                      breaks anywhere it has to — a URL with no spaces in it is
                      what pushes a list like this off the screen. */}
                  <span className="data-list__code">{instance.apiBase}</span>
                  {/* A badge with a word in it, not a bare coloured dot: colour
                      alone is the one encoding some readers do not receive. */}
                  <Badge
                    tone={instance.credentialsPresent ? 'success' : 'neutral'}
                  >
                    {instance.credentialsPresent ? 'key saved' : 'no key'}
                  </Badge>
                  <Badge tone={instance.enabled ? 'success' : 'neutral'}>
                    {instance.enabled ? 'Enabled' : 'Disabled'}
                  </Badge>
                </>
              }
              actions={
                <RowActions label={instance.displayName}>
                  <DropdownMenuItem
                    onSelect={() => {
                      void navigate({
                        to: '/settings/providers/$instanceId',
                        params: { instanceId: instance.id },
                      });
                    }}
                  >
                    <Pencil />
                    Edit
                  </DropdownMenuItem>
                  {/* Reversible where Delete is not, so it does not ask:
                        switching it back on is the same click. */}
                  <DropdownMenuItem
                    disabled={saving}
                    onSelect={() => {
                      save(
                        toProviderEnabledPatch(instance.id, !instance.enabled),
                      );
                    }}
                  >
                    {instance.enabled ? <PowerOff /> : <Power />}
                    {instance.enabled ? 'Disable' : 'Enable'}
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    className="menu__item--danger"
                    onSelect={() => {
                      setPendingDelete(instance);
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

      <Pagination
        pagination={pagination}
        total={matched.length}
        label={t('providers.title')}
      />

      <ConfirmDialog
        open={pendingDelete !== undefined}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(undefined);
        }}
        title={t('providers.deleteTitle')}
        description={t('providers.deleteHint', {
          name: pendingDelete?.displayName ?? '',
        })}
        confirmLabel="Delete"
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
    </Section>
  );
}
