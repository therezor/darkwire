/**
 * The Extensions panel: third-party code this install has been given, and
 * whether the operator has said yes to it.
 *
 * Shaped like MCP servers next door, because it is the same kind of screen — a
 * list joined to live state, a `RowActions` kebab, a badge with a word in it.
 * Two things are deliberately different, and both come from the fact that this
 * list is about *code*:
 *
 *  - **There is no editor route.** An extension is a directory an operator put
 *    on the box; nothing about it is editable from here. The panel's whole job
 *    is the decision — approve, withdraw, disable — and a form would imply this
 *    screen could change what the extension does.
 *  - **Approve asks, and says what it is asking about.** Every other reversible
 *    action in Settings just happens. This one grants the code the server's own
 *    access, so it goes through `ConfirmDialog` with the sentence that says so.
 *    The panel that made this a one-click toggle would be the panel that made
 *    the digest gate decorative.
 *
 * The row's `contributes` is the line to read before the button: it is what the
 * extension declared, and the host drops anything it registers beyond it. It is
 * disclosure rather than a boundary — an extension process reaches the
 * filesystem regardless — and `docs/security.md` says so rather than implying
 * otherwise.
 */

import { Blocks, Check, Power, PowerOff, ShieldOff } from 'lucide-react';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';
import type {
  Config,
  ExtensionStatus,
  ExtensionState,
} from '@darkwire/protocol';

import { Badge } from '@/components/ui/badge.js';
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
import { useApproveExtension, useExtensions } from './use-extensions.js';
import { useSaveSettings } from './use-settings.js';

type SortKey = 'name' | 'status';

/** Both are text, and text reads from A. See `sortBy`. */
const ASCENDING_FIRST: readonly SortKey[] = ['name', 'status'];

/**
 * How loudly a row's state should read.
 *
 * `warning` for `unapproved`, not `danger`: an extension nobody has approved is
 * the *correct* state for one that was just installed, and the loudest thing on
 * this screen should be something that went wrong. `drifted` is the same tone
 * for the same reason — files changed, which is what editing an extension is
 * supposed to do.
 */
const TONES: Readonly<
  Record<ExtensionState, 'success' | 'warning' | 'neutral' | 'danger'>
> = {
  ready: 'success',
  unapproved: 'warning',
  drifted: 'warning',
  disabled: 'neutral',
  failed: 'danger',
};

const STATE_LABELS: Readonly<Record<ExtensionState, WebKey>> = {
  ready: 'settings.extensions.state.ready',
  unapproved: 'settings.extensions.state.unapproved',
  drifted: 'settings.extensions.state.drifted',
  disabled: 'settings.extensions.state.disabled',
  failed: 'settings.extensions.state.failed',
};

/** Ranked for the sort, worst first: "what needs me" is one press away. */
const STATE_ORDER: Readonly<Record<ExtensionState, number>> = {
  failed: 0,
  drifted: 1,
  unapproved: 2,
  disabled: 3,
  ready: 4,
};

const COMPARE: Comparators<ExtensionStatus, SortKey> = {
  name: (a, b) => a.id.localeCompare(b.id),
  status: (a, b) => STATE_ORDER[a.state] - STATE_ORDER[b.state],
};

/** The patch that turns one extension off, or back on. */
function toEnabledPatch(
  config: Config,
  id: string,
  enabled: boolean,
): { readonly extensions: { readonly disabled: string[] } } {
  const disabled = config.extensions.disabled.filter((one) => one !== id);
  // Arrays replace, so the whole list is sent — which is also the only way to
  // express a removal. See `docs/configuration.md#patching`.
  return {
    extensions: { disabled: enabled ? disabled : [...disabled, id] },
  };
}

export function ExtensionsPanel({
  config,
}: {
  readonly config: Config;
}): JSX.Element {
  const { t } = useTranslation();
  const [pendingApprove, setPendingApprove] = useState<
    ExtensionStatus | undefined
  >(undefined);
  const { approve, revoke, pending } = useApproveExtension();
  const { save, saving } = useSaveSettings();
  const live = useExtensions();

  const all: readonly ExtensionStatus[] = live.data?.extensions ?? [];

  const { filter, setFilter, sort, setSort, matched, pagination, rows } =
    useListPage({
      rows: all,
      initialSort: { key: 'status', descending: false },
      // The label and the description are in the haystack because both are on
      // screen: a list that shows a value it will not match on reads as broken.
      haystack: (row) => `${row.id} ${row.label} ${row.description}`,
      comparators: COMPARE,
      tiebreak: (a, b) => a.id.localeCompare(b.id),
    });

  return (
    <div className="stack settings-panel">
      <Section
        title={t('settings.extensions.title')}
        description={t('settings.extensions.description')}
      >
        {all.length > 0 && (
          <div className="row list-toolbar">
            <SearchFilter
              value={filter}
              label={t('settings.extensions.filter')}
              onValueChange={setFilter}
            />
            <ListSort
              options={[
                { key: 'name', label: t('common.name') },
                { key: 'status', label: t('common.status') },
              ]}
              sort={sort}
              ascendingFirst={ASCENDING_FIRST}
              onChange={setSort}
            />
          </div>
        )}

        {all.length === 0 ? (
          // The empty state names the directory, because "install one" is not
          // an action this screen can offer — an extension arrives on the box
          // by some means the browser has no part in.
          <p className="page__note">{t('settings.extensions.none')}</p>
        ) : matched.length === 0 ? (
          <p className="page__note">
            {t('settings.extensions.noMatch', { filter })}
          </p>
        ) : (
          <DataList label={t('settings.extensions.title')}>
            {rows.map((row) => (
              <DataListRow
                key={row.id}
                primary={
                  <span className="data-list__open">
                    <Blocks />
                    <span className="row">
                      <span className="truncate">
                        {row.label === '' ? row.id : row.label}
                      </span>
                    </span>
                  </span>
                }
                meta={
                  <>
                    <span className="data-list__code">{row.id}</span>
                    {/* A badge with a word in it, not a bare coloured dot:
                        colour alone is the one encoding some readers do not
                        receive. */}
                    <Badge tone={TONES[row.state]}>
                      {t(STATE_LABELS[row.state])}
                    </Badge>
                    {row.version !== '' && (
                      <span className="data-list__code">{row.version}</span>
                    )}
                    {/* The line to read before pressing Approve. */}
                    <span>
                      {row.contributes.length === 0
                        ? t('settings.extensions.declaresNothing')
                        : t('settings.extensions.declares', {
                            kinds: row.contributes.join(', '),
                          })}
                    </span>
                    {row.description !== '' && (
                      <span className="data-list__detail truncate">
                        {row.description}
                      </span>
                    )}
                    {/* The sentence an operator came for. It is the only place
                        the reason exists — see `ExtensionStatus.lastError`. */}
                    {row.lastError !== undefined && (
                      <span className="data-list__detail truncate">
                        {firstLine(row.lastError)}
                      </span>
                    )}
                    {row.warnings.map((warning) => (
                      <span
                        key={warning}
                        className="data-list__detail truncate"
                      >
                        {warning}
                      </span>
                    ))}
                  </>
                }
                actions={
                  <RowActions label={row.id}>
                    {/* Approve is offered whenever the digest is not the one on
                        record — which is `unapproved` and `drifted` alike, and
                        `failed` too, because an extension that threw may have
                        been repaired since. */}
                    {row.state !== 'ready' && (
                      <DropdownMenuItem
                        disabled={pending}
                        onSelect={() => {
                          setPendingApprove(row);
                        }}
                      >
                        <Check />
                        {t('settings.extensions.approve')}
                      </DropdownMenuItem>
                    )}
                    {/* Withdrawing needs no confirmation: it takes access away,
                        and the files stay where they are. */}
                    <DropdownMenuItem
                      disabled={pending}
                      onSelect={() => {
                        revoke(row.id);
                      }}
                    >
                      <ShieldOff />
                      {t('settings.extensions.revoke')}
                    </DropdownMenuItem>
                    <DropdownMenuItem
                      disabled={saving}
                      onSelect={() => {
                        save(
                          toEnabledPatch(
                            config,
                            row.id,
                            row.state === 'disabled',
                          ),
                        );
                      }}
                    >
                      {row.state === 'disabled' ? <Power /> : <PowerOff />}
                      {row.state === 'disabled'
                        ? t('common.enable')
                        : t('common.disable')}
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
          label={t('settings.extensions.title')}
        />
      </Section>

      <ConfirmDialog
        open={pendingApprove !== undefined}
        onOpenChange={(open) => {
          if (!open) setPendingApprove(undefined);
        }}
        title={t('settings.extensions.approveTitle', {
          name: pendingApprove?.id ?? '',
        })}
        description={t('settings.extensions.approveHint')}
        confirmLabel={t('settings.extensions.approve')}
        pending={pending}
        onConfirm={() => {
          if (pendingApprove === undefined) return;
          approve(pendingApprove.id);
          setPendingApprove(undefined);
        }}
      />
    </div>
  );
}

/**
 * The first line of a multi-line refusal.
 *
 * `ExtensionStore` writes its messages for a terminal — a sentence, then two
 * indented lines naming the command that fixes it. The command is not what to
 * offer someone already looking at the button that does it, so the row shows
 * the sentence and `darkwire extension list` shows the rest.
 */
function firstLine(message: string): string {
  return message.split('\n')[0] ?? message;
}
