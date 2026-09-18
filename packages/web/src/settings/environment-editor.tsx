/**
 * Editing one environment definition.
 *
 * A route of its own rather than a row that expands, because a definition has
 * several sections' worth of settings and every other CRUD screen in this
 * package already works this way: the list picks, this edits, the back link
 * returns.
 *
 * **The whole definition, hardening included.** A UI that edited the resource
 * budget and left the capability set to a text editor would be two doors into
 * one room, and the door people found would be the one that could not express
 * what they needed. Nothing here can widen what the file already refuses: the
 * server validates a save exactly as it validates a file, so an image that is
 * not digest-pinned and a capability that is never grantable come back as the
 * same sentence they would from `darkwire environment list`. What the server
 * merely disapproves of comes back as the `weakened` line above, which is why
 * the hardening is editable at all: the check that matters is on the server,
 * and a read-only box here protected nothing that a `PUT` could not reach.
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

import type {
  EnvironmentSummary,
  ResolveImageResponse,
} from '@darkwire/protocol';

import { PageTitle } from '@/app/page-title.js';
import { Button } from '@/components/ui/button.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { api } from '@/lib/api.js';
import { RowActions } from '@/components/crud/row-actions.js';
import {
  FieldGrid,
  SaveBar,
  Section,
  SelectField,
  SwitchRow,
  TextareaField,
  TextField,
} from '@/components/form/controls.js';
import {
  CONTAINER_RUNTIMES,
  SECCOMP_PROFILES,
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
  /**
   * What the last resolve produced, or why it failed.
   *
   * Local state rather than a query: it is an action with a result, not a fact
   * about the server, and it is cleared the moment the box is typed in again so
   * a stale "resolved to…" cannot sit under a different reference.
   */
  const [resolving, setResolving] = useState(false);
  const [resolved, setResolved] = useState<ResolveImageResponse | undefined>();
  const [resolveError, setResolveError] = useState<string | undefined>();

  const onResolve = (): void => {
    setResolving(true);
    setResolved(undefined);
    setResolveError(undefined);
    api
      .resolveImage(form.image.trim())
      .then((answer) => {
        // Straight into the field. The digest is the value a definition
        // carries, and making an operator copy it out of a note beside the box
        // would leave the step this exists to remove.
        update('image', answer.image);
        setResolved(answer);
      })
      .catch((error: unknown) => {
        setResolveError(
          error instanceof Error
            ? error.message
            : t('settings.environments.resolveFailed'),
        );
      })
      .finally(() => {
        setResolving(false);
      });
  };

  const update = <K extends keyof EnvironmentForm>(
    field: K,
    value: EnvironmentForm[K],
  ): void => {
    setForm((current) => ({ ...current, [field]: value }));
    setDirty(true);
  };

  const collision = creating && taken.includes(form.name);

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
          <PageTitle
            title={
              form.name === ''
                ? t('settings.environments.newEnvironment')
                : form.name
            }
          />
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
          <div className="stack settings-environment__image">
            <TextField
              label={t('settings.environments.image')}
              value={form.image}
              placeholder="node:22"
              hint={t('settings.environments.imageHint')}
              error={resolveError}
              onValueChange={(value) => {
                update('image', value);
                setResolved(undefined);
                setResolveError(undefined);
              }}
            />
            {/* The step that made adding a container a research project.
                A definition must pin a digest, and finding one meant `docker
                pull` and `docker image inspect` in a terminal. The rule does
                not move: what lands in the box above is still a digest. */}
            <Button
              type="button"
              variant="secondary"
              disabled={resolving || form.image.trim() === ''}
              onClick={onResolve}
            >
              {resolving
                ? t('settings.environments.resolving')
                : t('settings.environments.resolve')}
            </Button>
            {resolved !== undefined && (
              <p className="page__note">
                {resolved.pulled
                  ? t('settings.environments.resolvedPulled', {
                      reference: resolved.reference,
                    })
                  : t('settings.environments.resolvedLocal', {
                      reference: resolved.reference,
                    })}
              </p>
            )}
          </div>
        </FieldGrid>
        {/* The one field here that reaches a model, and it carries the same
            label as the agent editor's box, because the two fill the same
            section of the same prompt. Every agent using this definition is
            told this unless it overrides it, so a toolchain is described once
            per image rather than once per agent. */}
        <TextareaField
          label={t('settings.environments.prompt')}
          value={form.prompt}
          placeholder={t('settings.environments.promptPlaceholder')}
          hint={t('settings.environments.promptHint')}
          error={
            form.prompt.trimStart().startsWith('#')
              ? t('settings.environments.promptHeading')
              : undefined
          }
          onValueChange={(value) => {
            update('prompt', value);
          }}
        />
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

      {/* Behind a disclosure, and editable.
          
          These are the fields a catalogue definition ships correctly and most
          operators never touch: the runtime, the uid, the hardening, the device
          list. Closed by default so the screen reads as "what image, how big",
          which is what nearly every visit is about.
          
          They were a read-only readout, and that was a door that looked locked
          without being one: the save route takes a whole definition, so the
          browser could already write every one of these. What refuses a bad one
          is the server, and it still does. What it merely disapproves of comes
          back as the weakened line at the top of this page. */}
      <details className="settings-section settings-advanced">
        <summary>{t('settings.environments.advanced')}</summary>
        <div className="stack settings-advanced__body">
          <p className="page__note">
            {t('settings.environments.advancedHint')}
          </p>
          <FieldGrid>
            <SelectField
              label={t('settings.environments.runtime')}
              value={form.runtime}
              options={CONTAINER_RUNTIMES.map((runtime) => ({
                value: runtime,
                label: runtime,
              }))}
              hint={t('settings.environments.runtimeHint')}
              onValueChange={(value) => {
                update('runtime', value as EnvironmentForm['runtime']);
              }}
            />
            <SelectField
              label={t('settings.environments.seccomp')}
              value={form.seccomp}
              options={SECCOMP_PROFILES.map((profile) => ({
                value: profile,
                label: profile,
              }))}
              hint={t('settings.environments.seccompHint')}
              onValueChange={(value) => {
                update('seccomp', value as EnvironmentForm['seccomp']);
              }}
            />
            <TextField
              label={t('settings.environments.user')}
              value={form.user}
              hint={t('settings.environments.userHint')}
              onValueChange={(value) => {
                update('user', value);
              }}
            />
            <TextField
              label={t('settings.environments.workdir')}
              value={form.workdir}
              hint={t('settings.environments.workdirHint')}
              onValueChange={(value) => {
                update('workdir', value);
              }}
            />
            <TextField
              label={t('settings.environments.pidsMax')}
              value={form.pidsMax}
              inputMode="numeric"
              error={errors.pidsMax}
              onValueChange={(value) => {
                update('pidsMax', value);
              }}
            />
            <TextField
              label={t('settings.environments.shmSizeMb')}
              value={form.shmSizeMb}
              inputMode="numeric"
              error={errors.shmSizeMb}
              onValueChange={(value) => {
                update('shmSizeMb', value);
              }}
            />
          </FieldGrid>
          <SwitchRow
            label={t('settings.environments.noNewPrivileges')}
            hint={t('settings.environments.noNewPrivilegesHint')}
            checked={form.noNewPrivileges}
            onCheckedChange={(checked) => {
              update('noNewPrivileges', checked);
            }}
          />
          <SwitchRow
            label={t('settings.environments.readOnlyRoot')}
            hint={t('settings.environments.readOnlyRootHint')}
            checked={form.readOnlyRoot}
            onCheckedChange={(checked) => {
              update('readOnlyRoot', checked);
            }}
          />
          {/* One entry per line, never comma-separated: a tmpfs spec has commas
              inside a single entry, so splitting on them would cut one mount
              into three broken ones. */}
          <TextareaField
            label={t('settings.environments.tmpfs')}
            value={form.tmpfs}
            hint={t('settings.environments.tmpfsHint')}
            onValueChange={(value) => {
              update('tmpfs', value);
            }}
          />
          <TextareaField
            label={t('settings.environments.devices')}
            value={form.devices}
            hint={t('settings.environments.devicesHint')}
            onValueChange={(value) => {
              update('devices', value);
            }}
          />
          <TextareaField
            label={t('settings.environments.capsDrop')}
            value={form.capsDrop}
            hint={t('settings.environments.capsDropHint')}
            onValueChange={(value) => {
              update('capsDrop', value);
            }}
          />
          <TextareaField
            label={t('settings.environments.capsAdd')}
            value={form.capsAdd}
            hint={t('settings.environments.capsAddHint')}
            onValueChange={(value) => {
              update('capsAdd', value);
            }}
          />
          <TextareaField
            label={t('settings.environments.env')}
            value={form.env}
            hint={t('settings.environments.envHint')}
            onValueChange={(value) => {
              update('env', value);
            }}
          />
        </div>
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
