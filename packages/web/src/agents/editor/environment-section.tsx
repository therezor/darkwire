import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import {
  FieldGrid,
  Section,
  SelectField,
  SwitchRow,
  TextField,
} from '@/components/form/controls.js';
import type { AgentEditor } from '@/agents/use-agent-editor.js';

import { HOST_ENVIRONMENT } from './constants.js';

export function EnvironmentSection({
  editor,
}: {
  readonly editor: AgentEditor;
}): JSX.Element {
  const { t } = useTranslation();
  const {
    form,
    update,
    switches,
    environments,
    environmentOptions,
    networkOptions,
    chosenEnvironment,
  } = editor;

  return (
    <Section
      title={t('agents.environmentSection')}
      description={t('agents.environmentDesc')}
    >
      {environments.isPending && (
        <p className="page__note">{t('agents.environmentLoading')}</p>
      )}
      {environments.data?.environments.length === 0 && (
        <p className="page__note">{t('agents.environmentNoProfiles')}</p>
      )}
      <FieldGrid>
        <SelectField
          label={t('agents.environmentProfile')}
          value={
            form.environmentName === ''
              ? HOST_ENVIRONMENT
              : form.environmentName
          }
          onValueChange={(value) => {
            update('environmentName', value === HOST_ENVIRONMENT ? '' : value);
          }}
          options={environmentOptions}
        />
        {/* Only with an environment. Egress is enforced by its
            gateway, so offering the control without one would be offering a
            setting the save refuses. */}
        {chosenEnvironment !== undefined && (
          <SelectField
            label={t('agents.environmentNetwork')}
            value={form.environmentNetworkMode}
            onValueChange={(value) => {
              update('environmentNetworkMode', value);
            }}
            options={networkOptions}
          />
        )}
      </FieldGrid>

      {/* Under the picker it qualifies, because "where does this agent run"
          and "does that hold when something delegates to it" are one
          question asked twice. */}
      <SwitchRow
        label={t('agents.environmentAlwaysUseOwn')}
        hint={
          form.environmentAlwaysUseOwn
            ? form.environmentName === ''
              ? t('agents.environmentAlwaysUseOwnHostHint')
              : t('agents.environmentAlwaysUseOwnHint', {
                  environment: form.environmentName,
                })
            : t('agents.environmentAlwaysUseOwnOffHint')
        }
        checked={switches.environmentAlwaysUseOwn.checked}
        onCheckedChange={switches.environmentAlwaysUseOwn.set}
      />

      {/* Only the states an operator has to act on. A manifest that parses
          needs no line of its own. */}
      {chosenEnvironment !== undefined && (
        <p className="page__note">
          {chosenEnvironment.definition?.shared
            ? t('agents.environmentShared', { name: chosenEnvironment.name })
            : t('agents.environmentPrivate', {
                name: chosenEnvironment.name,
              })}
        </p>
      )}
      {chosenEnvironment?.problem !== undefined && (
        <p className="page__note">{chosenEnvironment.problem}</p>
      )}
      {chosenEnvironment !== undefined &&
        chosenEnvironment.weakened.length > 0 && (
          <p className="page__note">
            {t('agents.environmentWeakened', {
              what: chosenEnvironment.weakened.join(', '),
            })}
          </p>
        )}
      {/* Resolved by the server from the definition's own uid, privileges and
          capabilities, so an operator learns a restricted allow-list is
          impossible here while they are still choosing rather than on save. */}
      {chosenEnvironment?.gatewayProblem !== undefined &&
        form.environmentNetworkMode === 'allowlist' && (
          <p className="page__note">
            {t('agents.environmentGatewayProblem', {
              why: chosenEnvironment.gatewayProblem,
            })}
          </p>
        )}
      {chosenEnvironment !== undefined &&
        form.environmentNetworkMode === 'allowlist' && (
          <>
            <TextField
              label={t('agents.environmentAllow')}
              value={form.environmentAllow}
              onValueChange={(value) => {
                update('environmentAllow', value);
              }}
              hint={t('agents.environmentAllowHint')}
            />
          </>
        )}
    </Section>
  );
}
