/**
 * Editing one environment definition.
 *
 * A route of its own rather than a row that expands, because a definition has
 * five sections' worth of settings and every other CRUD screen in this package
 * already works this way: the list picks, this edits, the back link returns.
 *
 * **The whole definition, hardening included.** A UI that edited the resource
 * budget and left the capability set to a text editor would be two doors into
 * one room, and the door people found would be the one that could not express
 * what they needed. Nothing here can widen what the file already refuses: the
 * server validates a save exactly as it validates a file, so an image that is
 * not digest-pinned and a capability that is never grantable come back as the
 * same sentence they would from `darkwire environment list`.
 *
 * **The warnings are the last saved state, not the form's.** `weakened` and
 * `gatewayProblem` are resolved on the server so the terminal and the browser
 * cannot describe one definition differently; recomputing them here would be a
 * second implementation of the thing that exists to stop there being two. They
 * refresh on save, which is when they change.
 */

import { useState, type JSX } from 'react';
import { Link, useNavigate, useParams } from '@tanstack/react-router';
import { ArrowLeft, Trash2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';

import type { EnvironmentSummary } from '@darkwire/protocol';

import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { RowActions } from '@/components/crud/row-actions.js';
import {
  FieldGrid,
  SaveBar,
  Section,
  TextField,
} from '@/components/form/controls.js';
import {
  emptyEnvironmentForm,
  toEnvironmentDefinition,
  toEnvironmentForm,
  type EnvironmentErrors,
  type EnvironmentForm,
} from './environment-form.js';
import {
  useEnvironments,
  useRemoveEnvironment,
  useSaveEnvironment,
} from './use-environment.js';

function BackLink(): JSX.Element {
  const { t } = useTranslation();
  return (
    <Link
      to="/settings"
      search={{ panel: 'environments' }}
      className="page__back"
    >
      <ArrowLeft aria-hidden="true" />
      {t('settings.panels.environments.label')}
    </Link>
  );
}

/**
 * Creating one, on the form that edits one.
 *
 * Seeded from the schema's own defaults, which is the hardened shape: every
 * capability dropped, no new privileges, a read-only root and a non-root uid.
 * An operator weakening one of those is making a decision; an operator who
 * never opened the section has made none.
 */
export function EnvironmentCreateRoute(): JSX.Element {
  const installed = useEnvironments();
  return (
    <Editor
      mode="create"
      stored={emptyEnvironmentForm()}
      summary={undefined}
      taken={(installed.data?.environments ?? []).map((entry) => entry.name)}
    />
  );
}

export function EnvironmentEditorRoute(): JSX.Element {
  const { t } = useTranslation();
  const { name } = useParams({ from: '/settings/environments/$name' });
  const installed = useEnvironments();

  if (installed.isPending) {
    return <p className="page__note">{t('settings.environments.loading')}</p>;
  }
  if (installed.isError) {
    return (
      <p role="alert" className="page__error">
        {t('settings.environments.loadError', {
          message: installed.error.message,
        })}
      </p>
    );
  }

  const summary = installed.data.environments.find(
    (entry) => entry.name === name,
  );

  // A stale link, or a file that does not parse. Either way there is nothing to
  // load into the form, and saying so beats an empty one that would silently
  // write over whatever is on disk on the first save.
  if (summary?.definition === undefined) {
    return (
      <div className="stack page page--wide">
        <p role="alert" className="page__error">
          {summary?.problem ??
            t('settings.environments.noSuchEnvironment', { name })}
        </p>
        <BackLink />
      </div>
    );
  }

  return (
    // Remounts on a change of environment, so one definition's edits cannot
    // survive into the next one's boxes.
    <Editor
      key={summary.name}
      mode="edit"
      stored={toEnvironmentForm(summary.definition)}
      summary={summary}
      taken={[]}
    />
  );
}

function Editor({
  mode,
  stored,
  summary,
  taken,
}: {
  /** `create` lets the name be typed and navigates on save; `edit` does not. */
  readonly mode: 'create' | 'edit';
  readonly stored: EnvironmentForm;
  /** The server's verdict on the saved definition, absent while creating. */
  readonly summary: EnvironmentSummary | undefined;
  /** Names already installed, so a create cannot land on one. */
  readonly taken: readonly string[];
}): JSX.Element {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const creating = mode === 'create';
  const [form, setForm] = useState(stored);
  const [dirty, setDirty] = useState(creating);
  const [errors, setErrors] = useState<EnvironmentErrors>({});
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const { save, saving, error: saveError } = useSaveEnvironment();
  const { remove, removing } = useRemoveEnvironment();

  const update = <K extends keyof EnvironmentForm>(
    field: K,
    value: EnvironmentForm[K],
  ): void => {
    setForm((current) => ({ ...current, [field]: value }));
    setDirty(true);
  };

  const collision = creating && taken.includes(form.name);
  // Named rather than linked: the browser cannot open it, and the point is to
  // say where the rest of the definition is edited.
  const filePath = `policy/environments/${form.name || 'name'}.yaml`;

  const onSave = (): void => {
    const result = toEnvironmentDefinition(form, t);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    save(result.definition, {
      // On success, not on the press: the editor this lands on reads the
      // environments cache, and arriving before the write does is the "no such
      // environment" path.
      onSuccess: () => {
        setDirty(false);
        if (creating) {
          void navigate({
            to: '/settings/environments/$name',
            params: { name: result.definition.name },
          });
        }
      },
    });
  };

  return (
    <div className="stack page page--wide">
      <div className="editor__head">
        <BackLink />

        <div className="cluster editor__title">
          <h1 className="page__title">
            {form.name === ''
              ? t('settings.environments.newEnvironment')
              : form.name}
          </h1>
          <span className="spacer" />
          {/* Not a button at the bottom of the form: a destructive action does
              not belong in the reading order of the settings it would destroy.
              Absent while creating, since there is nothing yet to remove. */}
          {!creating && (
            <RowActions label={form.name}>
              <DropdownMenuItem
                className="menu__item--danger"
                onSelect={() => {
                  setConfirmingDelete(true);
                }}
              >
                <Trash2 />
                {t('settings.environments.deleteEnvironment')}
              </DropdownMenuItem>
            </RowActions>
          )}
        </div>

        {/* The last saved verdict. Warnings rather than refusals: a weakened
            definition is usable, and an operator who chose the weakening
            deserves to be reminded rather than blocked. */}
        {summary !== undefined && summary.weakened.length > 0 && (
          <p role="status" className="page__note">
            {t('settings.environments.weakened', {
              what: summary.weakened.join(', '),
            })}
          </p>
        )}
        {summary?.gatewayProblem !== undefined && (
          <p role="status" className="page__note">
            {t('settings.environments.gatewayProblem', {
              why: summary.gatewayProblem,
            })}
          </p>
        )}
      </div>

      <Section
        title={t('settings.environments.identity')}
        description={t('settings.environments.identityDesc')}
      >
        <FieldGrid>
          {/* Shown either way, disabled after the first save. The name is the
              filename, so renaming one is creating another; showing it read-only
              is what tells an operator which definition they have open. */}
          <TextField
            label={t('common.name')}
            value={form.name}
            disabled={!creating}
            error={
              errors.name ??
              (collision ? t('settings.environments.nameTaken') : undefined)
            }
            hint={t('settings.environments.nameHint')}
            onValueChange={(value) => {
              update('name', value);
            }}
          />
          <TextField
            label={t('settings.environments.image')}
            value={form.image}
            placeholder="sha256:…"
            hint={t('settings.environments.imageHint')}
            onValueChange={(value) => {
              update('image', value);
            }}
          />
        </FieldGrid>
      </Section>

      <Section
        title={t('settings.environments.resources')}
        description={t('settings.environments.resourcesDesc')}
      >
        <FieldGrid>
          <TextField
            label={t('settings.environments.memoryMb')}
            value={form.memoryMb}
            inputMode="numeric"
            error={errors.memoryMb}
            onValueChange={(value) => {
              update('memoryMb', value);
            }}
          />
          <TextField
            label={t('settings.environments.cpus')}
            value={form.cpus}
            inputMode="decimal"
            error={errors.cpus}
            onValueChange={(value) => {
              update('cpus', value);
            }}
          />
        </FieldGrid>
      </Section>

      {/* Read-only, and behind a disclosure.
          
          These are the fields a catalogue definition ships correctly and nobody
          hand-edits: the runtime, the uid, the hardening, the device list. They
          are shown rather than hidden because "what is this container actually
          doing" is a question this screen should answer, and named with their
          file because that is where they are changed.
          
          The form still carries them and sends them back untouched, so editing
          the memory on a hand-written definition does not quietly replace its
          tmpfs with a default. */}
      <details className="stack">
        <summary>{t('settings.environments.advanced')}</summary>
        <p className="page__note">
          {t('settings.environments.advancedHint', { path: filePath })}
        </p>
        <dl className="settings-readout">
          <dt>{t('settings.environments.runtime')}</dt>
          <dd>{form.runtime}</dd>
          <dt>{t('settings.environments.user')}</dt>
          <dd>{form.user}</dd>
          <dt>{t('settings.environments.workdir')}</dt>
          <dd>{form.workdir}</dd>
          <dt>{t('settings.environments.pidsMax')}</dt>
          <dd>{form.pidsMax}</dd>
          <dt>{t('settings.environments.shmSizeMb')}</dt>
          <dd>{form.shmSizeMb}</dd>
          <dt>{t('settings.environments.hardening')}</dt>
          <dd>
            {t('settings.environments.hardeningLine', {
              privileges: form.noNewPrivileges
                ? t('settings.environments.noNewPrivileges')
                : t('settings.environments.privilegesAllowed'),
              root: form.readOnlyRoot
                ? t('settings.environments.readOnlyRoot')
                : t('settings.environments.writableRoot'),
              seccomp: form.seccomp,
            })}
          </dd>
          <dt>{t('settings.environments.capsDrop')}</dt>
          <dd>{form.capsDrop || t('settings.environments.none')}</dd>
          <dt>{t('settings.environments.capsAdd')}</dt>
          <dd>{form.capsAdd || t('settings.environments.none')}</dd>
          <dt>{t('settings.environments.tmpfs')}</dt>
          <dd>{form.tmpfs || t('settings.environments.none')}</dd>
          <dt>{t('settings.environments.devices')}</dt>
          <dd>{form.devices || t('settings.environments.none')}</dd>
          <dt>{t('settings.environments.env')}</dt>
          <dd>{form.env || t('settings.environments.none')}</dd>
        </dl>
      </details>

      {/* Beside the button that produced it, as well as in the toast. A
          refusal here is a sentence about the definition on screen — an image
          that is not digest-pinned, an agent that still needs this one — and
          reading it should not depend on catching a toast before it goes. */}
      {saveError !== null && (
        <p role="alert" className="page__error">
          {saveError.message}
        </p>
      )}

      <SaveBar
        dirty={dirty && !collision}
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
        title={t('settings.environments.deleteTitle')}
        description={t('settings.environments.deleteHint', { name: form.name })}
        confirmLabel={t('common.delete')}
        pending={removing}
        onConfirm={() => {
          // Leaving on success rather than on the press: a delete the server
          // refuses because an agent still names it should leave the question
          // on screen with that sentence.
          remove(form.name, {
            onSuccess: () => {
              void navigate({
                to: '/settings',
                search: { panel: 'environments' },
              });
            },
          });
        }}
      />
    </div>
  );
}
