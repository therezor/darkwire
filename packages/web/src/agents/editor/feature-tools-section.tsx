import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { FieldGrid, Section, SwitchRow } from '@/components/form/controls.js';
import type { AgentEditor } from '@/agents/use-agent-editor.js';

import { ToolList } from './tool-list.js';

/**
 * `memory` and `skill`, as switches and as rows.
 *
 * Its own section, with the two switches directly above the two rows they
 * are: the switches write the same `allow`/`deny` the rows show, and the rows
 * add "ask first" and the wording. Denying either also removes a section from
 * every prompt on the workspace, which is why they read as feature switches
 * rather than as one denied call.
 */
export function FeatureToolsSection({
  editor,
}: {
  readonly editor: AgentEditor;
}): JSX.Element {
  const { t } = useTranslation();
  const { memoryOff, skillsOff, featureToolNames, setToolPermission } = editor;

  return (
    <Section
      title={t('agents.featureToolsGroup')}
      description={t('agents.featureToolsDesc')}
    >
      <FieldGrid>
        <SwitchRow
          label={t('agents.memoryEnabled')}
          hint={t('agents.memoryEnabledHint')}
          checked={!memoryOff}
          onCheckedChange={(next) => {
            setToolPermission('memory', next ? 'allow' : 'deny');
          }}
        />
        <SwitchRow
          label={t('agents.skillsEnabled')}
          hint={t('agents.skillsEnabledHint')}
          checked={!skillsOff}
          onCheckedChange={(next) => {
            setToolPermission('skill', next ? 'allow' : 'deny');
          }}
        />
      </FieldGrid>
      {featureToolNames.length > 0 && (
        <ToolList editor={editor} names={featureToolNames} />
      )}
    </Section>
  );
}
