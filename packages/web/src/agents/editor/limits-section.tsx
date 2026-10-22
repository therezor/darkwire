import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { FieldGrid, Section } from '@/components/form/controls.js';
import type { AgentEditor } from '@/agents/use-agent-editor.js';

import { BoundField } from './bound-fields.js';

export function LimitsSection({
  editor,
}: {
  readonly editor: AgentEditor;
}): JSX.Element {
  const { t } = useTranslation();
  const { fields, errors } = editor;

  return (
    <Section title={t('agents.limits')} description={t('agents.limitsDesc')}>
      <FieldGrid>
        <BoundField
          label={t('agents.maxOutputTokens')}
          bound={fields.maxTokens}
          inputMode="numeric"
          error={errors.maxTokens}
        />
        <BoundField
          label={t('agents.contextWindow')}
          bound={fields.contextWindowTokens}
          inputMode="numeric"
          error={errors.contextWindowTokens}
        />
        <BoundField
          label={t('agents.toolTimeout')}
          bound={fields.toolTimeoutSeconds}
          inputMode="numeric"
          error={errors.toolTimeoutSeconds}
          hint={t('agents.zeroDisablesHint')}
        />
        {/* Per agent, like everything beside them. These two were the
            default agent's alone while they lived in a subtree of their own;
            a tool budget is as much a property of one agent as its model. */}
        <BoundField
          label={t('agents.maxToolIterations')}
          bound={fields.maxToolIterations}
          inputMode="numeric"
          error={errors.maxToolIterations}
        />
        <BoundField
          label={t('agents.turnTimeout')}
          bound={fields.loopWallTimeoutSeconds}
          inputMode="numeric"
          error={errors.loopWallTimeoutSeconds}
          hint={t('agents.zeroDisablesHint')}
        />
        {/* The tool layer's numbers, beside the caps they resemble. They were
            install-wide and sat in Settings; a result budget is as much a
            property of one agent as its iteration cap is. */}
        <BoundField
          label={t('agents.approvalTimeout')}
          bound={fields.approvalTimeoutSeconds}
          inputMode="decimal"
          error={errors.approvalTimeoutSeconds}
          hint={t('agents.approvalTimeoutHint')}
        />
        <BoundField
          label={t('agents.maxOutputChars')}
          bound={fields.maxOutputChars}
          inputMode="numeric"
          error={errors.maxOutputChars}
          hint={t('agents.maxOutputCharsHint')}
        />
        <BoundField
          label={t('agents.execTimeout')}
          bound={fields.execTimeoutSeconds}
          inputMode="decimal"
          error={errors.execTimeoutSeconds}
          hint={t('agents.zeroDisablesHint')}
        />
        <BoundField
          label={t('agents.execMaxOutputKb')}
          bound={fields.execMaxOutputKb}
          inputMode="decimal"
          error={errors.execMaxOutputKb}
        />
      </FieldGrid>
    </Section>
  );
}
