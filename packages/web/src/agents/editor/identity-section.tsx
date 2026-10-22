import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import {
  FieldGrid,
  Section,
  SwitchRow,
  TextField,
} from '@/components/form/controls.js';
import type { AgentEditor } from '@/agents/use-agent-editor.js';

export function IdentitySection({
  editor,
}: {
  readonly editor: AgentEditor;
}): JSX.Element {
  const { t } = useTranslation();
  const {
    agentId,
    creating,
    isDefault,
    form,
    errors,
    update,
    idDraft,
    updateIdDraft,
    proposedId,
    renaming,
    idError,
  } = editor;

  return (
    <Section
      title={t('agents.identity')}
      description={t('agents.identityDesc')}
    >
      <FieldGrid>
        <TextField
          label={t('common.name')}
          value={form.label}
          placeholder={agentId}
          onValueChange={(value) => {
            update('label', value);
          }}
          hint={t('agents.labelHint', { token: '{{name}}' })}
        />
        {!isDefault && (
          <TextField
            label={t('agents.idLabel')}
            value={idDraft}
            {...(creating ? { placeholder: proposedId } : {})}
            onValueChange={updateIdDraft}
            {...(errors.agentId === undefined || creating
              ? {}
              : { error: errors.agentId })}
            hint={
              // Creating shows what the id *will be* as it is typed, and says
              // so inline rather than as an error. A name that collides is a
              // thing to fix, not a failure that has happened.
              creating
                ? (idError ?? t('agents.idCreatePreview', { id: proposedId }))
                : renaming
                  ? t('agents.idPreview', { id: proposedId })
                  : t('agents.idHint')
            }
          />
        )}
        {!isDefault && (
          <SwitchRow
            label={t('agents.enabled')}
            hint={t('agents.enabledHint')}
            checked={form.enabled}
            onCheckedChange={(checked) => {
              update('enabled', checked);
            }}
          />
        )}
      </FieldGrid>
    </Section>
  );
}
