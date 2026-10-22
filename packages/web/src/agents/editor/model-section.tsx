import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import {
  FieldGrid,
  Section,
  SelectField,
  SwitchRow,
} from '@/components/form/controls.js';
import { REASONING_EFFORTS } from '@/agents/agents-form.js';
import type { AgentEditor } from '@/agents/use-agent-editor.js';

import { BoundField, OptionalSelect } from './bound-fields.js';

export function ModelSection({
  editor,
}: {
  readonly editor: AgentEditor;
}): JSX.Element {
  const { t } = useTranslation();
  const {
    isDefault,
    errors,
    fields,
    switches,
    models,
    providerOptions,
    modelChoices,
    onProviderChange,
  } = editor;

  return (
    <Section
      title={t('agents.modelSection')}
      description={
        isDefault
          ? 'What this agent runs on, and what a new agent is created holding.'
          : 'What this agent runs on.'
      }
    >
      <FieldGrid>
        <SelectField
          label={t('agents.provider')}
          value={fields.provider.value}
          options={providerOptions}
          onValueChange={onProviderChange}
          error={errors.provider}
        />
        {/* No "resolved automatically" option, because there is no such
            resolution: an empty model is `noModelError` and a `null` provider
            in the runtime, so offering it here dressed an unconfigured
            install up as a choice. Blank is a placeholder now, a question the
            form asks, and saving without answering it is refused. */}
        <SelectField
          label={t('agents.model')}
          value={fields.model.value}
          placeholder={
            modelChoices.length === 0
              ? 'No models to choose from'
              : 'Choose a model'
          }
          options={modelChoices.map((model) => ({
            value: model,
            label: model,
          }))}
          onValueChange={fields.model.set}
          error={errors.model}
          hint={
            modelChoices.length === 0
              ? 'Nothing could be listed. Add an endpoint and its credentials in Settings → Providers.'
              : models.isError
                ? 'The model lists could not be fetched. Anything already pinned is still offered.'
                : 'Endpoints that list their own models are enumerated live.'
          }
        />
        <BoundField
          label={t('agents.temperature')}
          bound={fields.temperature}
          inputMode="decimal"
          error={errors.temperature}
          placeholder={t('agents.providerDefault')}
          hint={t('agents.temperatureHint')}
        />
        {/* The same sentence as the temperature placeholder beside it, and
            the same one `/effort` shows: a blank here means this agent sends
            no reasoning parameter, so the provider applies its own. There is
            nothing above an agent for it to mean anything else. */}
        <OptionalSelect
          label={t('agents.reasoningEffort')}
          bound={fields.reasoningEffort}
          options={REASONING_EFFORTS}
          unsetLabel={t('agents.providerDefault')}
        />
        {/* Below the model rather than beside the toolset, because that is
            what they are about: what this model can be asked to do, not what
            this agent is allowed to do. Both default on, so an operator only
            comes here once they have met a model that cannot keep up. */}
        <SwitchRow
          label={t('agents.vision')}
          hint={t('agents.visionHint')}
          checked={switches.visionEnabled.checked}
          onCheckedChange={switches.visionEnabled.set}
        />
        <SwitchRow
          label={t('agents.toolsEnabled')}
          hint={t('agents.toolsEnabledHint')}
          checked={switches.toolsEnabled.checked}
          onCheckedChange={switches.toolsEnabled.set}
        />
      </FieldGrid>
    </Section>
  );
}
