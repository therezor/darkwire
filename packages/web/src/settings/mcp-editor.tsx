/**
 * Adding and editing one MCP server.
 *
 * A route of its own for the reason the provider editor is one: a server has
 * four sections' worth of settings — how to reach it, what to expose, whether it
 * needs authorizing, and whether it is on — and a modal holding all of them is a
 * scroll inside a box with a Save that cannot use `SaveBar`.
 *
 * **The transport is a control here, unlike a provider's type.** A provider's
 * type is fixed because the vault entry is keyed to the instance and a type that
 * could change would be a key handed to a stranger. A server that moved from a
 * local command to a URL is the same server offering the same tools, so this one
 * moves — and the half of the form that does not apply is *cleared* on save
 * rather than omitted, because `resolveSpec` refuses an entry naming both a
 * command and a url.
 *
 * **The id is fixed once created**, and that is the one thing here that behaves
 * like a provider's type. Every tool this server contributes is named
 * `mcp_<id>_<tool>`, and those names are the keys of every agent's permission
 * map — so renaming a server would silently revoke it from every agent that had
 * been granted one of its tools.
 */

import { Link, useNavigate, useParams } from '@tanstack/react-router';
import { ArrowLeft, ExternalLink, Trash2 } from 'lucide-react';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';
import type { McpServerStatus, McpTransport } from '@ghostwire/protocol';

import { Badge } from '@/components/ui/badge.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { RowActions } from '@/components/crud/row-actions.js';
import {
  FieldGrid,
  SaveBar,
  Section,
  SelectField,
  SwitchRow,
  TextField,
  TextareaField,
} from '@/components/form/controls.js';
import {
  EMPTY_MCP_FORM,
  proposeServerId,
  toMcpForm,
  toMcpPatch,
  type McpForm,
} from './mcp-form.js';
import { useMcpServers, useRemoveMcpServer } from './use-mcp.js';
import { useSaveSettings, useSettings } from './use-settings.js';

const TRANSPORTS: readonly McpTransport[] = ['stdio', 'streamableHttp', 'sse'];

function BackLink(): JSX.Element {
  const { t } = useTranslation();
  return (
    <Link to="/settings" search={{ panel: 'mcp' }} className="page__back">
      <ArrowLeft aria-hidden="true" />
      {t('settings.mcp.backToExtensions')}
    </Link>
  );
}

export function McpCreateRoute(): JSX.Element {
  const { t } = useTranslation();
  const settings = useSettings();
  const [serverId, setServerId] = useState('');

  if (settings.isPending) {
    return <p className="page__note">{t('settings.mcp.loadingOne')}</p>;
  }
  if (settings.isError) {
    return (
      <p role="alert" className="page__error">
        {t('settings.mcp.loadError', { message: settings.error.message })}
      </p>
    );
  }

  const taken = Object.keys(settings.data.config.tools.mcpServers);
  const trimmed = serverId.trim();

  return (
    <Editor
      mode="create"
      serverId={trimmed === '' ? '' : proposeServerId(trimmed, taken)}
      form={EMPTY_MCP_FORM}
      status={undefined}
      idField={
        <TextField
          label={t('settings.mcp.serverId')}
          value={serverId}
          onValueChange={setServerId}
          hint={t('settings.mcp.serverIdHint')}
        />
      }
    />
  );
}

export function McpEditorRoute(): JSX.Element {
  const { t } = useTranslation();
  const { serverId } = useParams({ from: '/settings/mcp/$serverId' });
  const settings = useSettings();
  const live = useMcpServers();

  if (settings.isPending) {
    return <p className="page__note">{t('settings.mcp.loadingOne')}</p>;
  }
  if (settings.isError) {
    return (
      <p role="alert" className="page__error">
        {t('settings.mcp.loadError', { message: settings.error.message })}
      </p>
    );
  }

  const stored = settings.data.config.tools.mcpServers[serverId];

  // A stale link — a bookmark to a server that was deleted, or a hand-typed id.
  // Saying so beats an empty form that silently creates one on first save.
  if (stored === undefined) {
    return (
      <div className="stack page page--wide">
        <p role="alert" className="page__error">
          {t('settings.mcp.noSuchServer', { id: serverId })}
        </p>
        <BackLink />
      </div>
    );
  }

  return (
    // Remounts on a change of server, so one server's edits cannot survive into
    // the next one's boxes.
    <Editor
      key={serverId}
      mode="edit"
      serverId={serverId}
      form={toMcpForm(stored)}
      status={live.data?.servers.find((server) => server.id === serverId)}
    />
  );
}

function Editor({
  mode,
  serverId,
  form: stored,
  status,
  idField,
}: {
  readonly mode: 'create' | 'edit';
  readonly serverId: string;
  readonly form: McpForm;
  /** Live state, for the badge and the authorize link. Absent while creating. */
  readonly status: McpServerStatus | undefined;
  /** The id box, rendered only while creating — it is fixed afterwards. */
  readonly idField?: JSX.Element;
}): JSX.Element {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const creating = mode === 'create';

  const [form, setForm] = useState<McpForm>(stored);
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>({});
  const [dirty, setDirty] = useState(false);
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const { save, saving } = useSaveSettings();
  const { remove, removing } = useRemoveMcpServer();

  const update = <K extends keyof McpForm>(key: K, value: McpForm[K]): void => {
    setForm((current) => ({ ...current, [key]: value }));
    setDirty(true);
  };

  const stdio = form.transport === 'stdio';

  const onSave = (): void => {
    if (serverId === '') {
      setErrors({ serverId: t('settings.fields.required') });
      return;
    }
    const result = toMcpPatch(serverId, form, t);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    save(result.patch, {
      onSuccess: () => {
        setDirty(false);
        // Back to the list on a create, because the row that just appeared is
        // the thing the operator wants to see connect. An edit stays put.
        if (creating) {
          void navigate({ to: '/settings', search: { panel: 'mcp' } });
        }
      },
    });
  };

  return (
    <div className="stack page page--wide">
      <div className="cluster page__header">
        <BackLink />
        <span className="spacer" />
        {!creating && (
          <RowActions label={serverId}>
            <DropdownMenuItem
              className="menu__item--danger"
              onSelect={() => {
                setConfirmingDelete(true);
              }}
            >
              <Trash2 />
              {t('common.delete')}
            </DropdownMenuItem>
          </RowActions>
        )}
      </div>

      <div className="row page__heading">
        <h2 className="page__title">
          {creating ? t('settings.mcp.newTitle') : serverId}
        </h2>
        {status !== undefined && (
          <Badge tone={status.state === 'ready' ? 'success' : 'neutral'}>
            {status.state === 'ready'
              ? t('settings.mcp.toolCount', { count: status.tools.length })
              : t('settings.mcp.state.disconnected')}
          </Badge>
        )}
      </div>

      {/* The one place an operator can act on `needs_authorization`. It is a
          link rather than a button because the destination is the
          authorization server's own page, not ours. */}
      {status?.authorizationUrl !== undefined && (
        <p role="alert" className="settings-load-error">
          <ExternalLink aria-hidden="true" />
          <a href={status.authorizationUrl} rel="noreferrer" target="_blank">
            {t('settings.mcp.authorizeLink', { name: serverId })}
          </a>
        </p>
      )}

      {/* The sentence an operator came for, when there is one. */}
      {status?.lastError !== undefined && (
        <p role="alert" className="settings-load-error">
          <span>{status.lastError}</span>
        </p>
      )}

      <Section
        title={t('settings.mcp.connectionTitle')}
        description={t('settings.mcp.connectionDesc')}
      >
        {idField}
        {errors.serverId !== undefined && (
          <p role="alert" className="page__error">
            {errors.serverId}
          </p>
        )}
        <SelectField
          label={t('settings.mcp.transport')}
          value={form.transport}
          options={TRANSPORTS.map((transport) => ({
            value: transport,
            label: t(`settings.mcp.transports.${transport}` as const),
          }))}
          onValueChange={(value) => {
            update('transport', value as McpTransport);
          }}
          hint={t('settings.mcp.transportHint')}
        />

        {stdio ? (
          <>
            <TextField
              label={t('settings.mcp.command')}
              value={form.command}
              error={errors.command}
              onValueChange={(value) => {
                update('command', value);
              }}
              hint={t('settings.mcp.commandHint')}
            />
            <TextareaField
              label={t('settings.mcp.args')}
              value={form.args}
              rows={3}
              onValueChange={(value) => {
                update('args', value);
              }}
              hint={t('settings.mcp.argsHint')}
            />
            <TextareaField
              label={t('settings.mcp.env')}
              value={form.env}
              rows={3}
              onValueChange={(value) => {
                update('env', value);
              }}
              hint={t('settings.mcp.envHint')}
            />
          </>
        ) : (
          <>
            <TextField
              label={t('settings.mcp.url')}
              value={form.url}
              error={errors.url}
              onValueChange={(value) => {
                update('url', value);
              }}
              hint={t('settings.mcp.urlHint')}
            />
            <TextareaField
              label={t('settings.mcp.headers')}
              value={form.headers}
              rows={3}
              onValueChange={(value) => {
                update('headers', value);
              }}
              hint={t('settings.mcp.headersHint')}
            />
          </>
        )}
      </Section>

      <Section
        title={t('settings.mcp.exposureTitle')}
        description={t('settings.mcp.exposureDesc')}
      >
        <TextareaField
          label={t('settings.mcp.enabledTools')}
          value={form.enabledTools}
          rows={3}
          onValueChange={(value) => {
            update('enabledTools', value);
          }}
          hint={t('settings.mcp.enabledToolsHint')}
        />
        {/* What the server offers and this entry filtered out. Only ever shown
            for a connected server, because it is a fact about a live list. */}
        {status !== undefined && status.filteredTools.length > 0 && (
          <p className="page__note">
            {t('settings.mcp.filteredTools', {
              names: status.filteredTools.join(', '),
            })}
          </p>
        )}
        <FieldGrid>
          <TextField
            label={t('settings.mcp.toolTimeout')}
            inputMode="decimal"
            value={form.toolTimeoutSeconds}
            error={errors.toolTimeoutSeconds}
            onValueChange={(value) => {
              update('toolTimeoutSeconds', value);
            }}
            hint={t('settings.mcp.toolTimeoutHint')}
          />
        </FieldGrid>
        <SwitchRow
          label={t('settings.mcp.enabled')}
          hint={t('settings.mcp.enabledHint')}
          checked={form.enabled}
          onCheckedChange={(checked) => {
            update('enabled', checked);
          }}
        />
      </Section>

      {/* Only for a transport that can carry it. A stdio server is a child
          process on this machine and has nobody to authorize with. */}
      {!stdio && (
        <Section
          title={t('settings.mcp.authTitle')}
          description={t('settings.mcp.authDesc')}
        >
          <SwitchRow
            label={t('settings.mcp.usesOAuth')}
            hint={t('settings.mcp.usesOAuthHint')}
            checked={form.usesOAuth}
            onCheckedChange={(checked) => {
              update('usesOAuth', checked);
            }}
          />
          {form.usesOAuth && (
            <>
              <FieldGrid>
                <TextField
                  label={t('settings.mcp.authUrl')}
                  value={form.authUrl}
                  error={errors.authUrl}
                  onValueChange={(value) => {
                    update('authUrl', value);
                  }}
                />
                <TextField
                  label={t('settings.mcp.tokenUrl')}
                  value={form.tokenUrl}
                  error={errors.tokenUrl}
                  onValueChange={(value) => {
                    update('tokenUrl', value);
                  }}
                />
              </FieldGrid>
              <TextField
                label={t('settings.mcp.clientId')}
                value={form.clientId}
                error={errors.clientId}
                onValueChange={(value) => {
                  update('clientId', value);
                }}
                hint={t('settings.mcp.clientIdHint')}
              />
              <TextareaField
                label={t('settings.mcp.scopes')}
                value={form.scopes}
                rows={2}
                onValueChange={(value) => {
                  update('scopes', value);
                }}
                hint={t('settings.mcp.scopesHint')}
              />
            </>
          )}
        </Section>
      )}

      {/* Warnings a server raised about itself: a tool whose schema could not
          be advertised, an `enabledTools` entry matching nothing. They belong
          here rather than on the list row, because every one of them is
          addressed by editing a field on this page. */}
      {status !== undefined && status.warnings.length > 0 && (
        <Section
          title={t('settings.mcp.warningsTitle')}
          description={t('settings.mcp.warningsDesc')}
        >
          <ul className="settings-divided-list">
            {status.warnings.map((warning) => (
              <li key={warning}>
                <span className="settings-divided-list__detail">{warning}</span>
              </li>
            ))}
          </ul>
        </Section>
      )}

      <SaveBar
        dirty={dirty || creating}
        saving={saving}
        onSave={onSave}
        onRevert={() => {
          setForm(stored);
          setErrors({});
          setDirty(false);
        }}
      />

      <ConfirmDialog
        open={confirmingDelete}
        onOpenChange={setConfirmingDelete}
        title={t('settings.mcp.deleteTitle')}
        description={t('settings.mcp.deleteHint', { name: serverId })}
        confirmLabel={t('common.delete')}
        pending={removing}
        onConfirm={() => {
          remove(serverId, {
            onSuccess: () => {
              setConfirmingDelete(false);
              void navigate({
                to: '/settings',
                search: { panel: 'mcp' },
              });
            },
          });
        }}
      />
    </div>
  );
}
