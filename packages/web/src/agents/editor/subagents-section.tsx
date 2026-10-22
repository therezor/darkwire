import { Plus } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { Button } from '@/components/ui/button.js';
import { Section } from '@/components/form/controls.js';
import { SubagentRow } from '@/agents/subagent-row.js';
import type { AgentEditor } from '@/agents/use-agent-editor.js';

export function SubagentsSection({
  editor,
}: {
  readonly editor: AgentEditor;
}): JSX.Element {
  const { t } = useTranslation();
  const { form, delegable, chosenSubagents, setSubagent, setSubagents } =
    editor;

  return (
    <Section
      title={t('agents.subagentsSection')}
      description={t('agents.subagentsDesc')}
    >
      {delegable.length === 0 ? (
        <p className="page__note">{t('agents.subagentsNoneAvailable')}</p>
      ) : (
        <>
          {form.subagents.length === 0 && (
            <p className="page__note">{t('agents.subagentsEmpty')}</p>
          )}

          <ul className="stack agent-editor__subagents">
            {form.subagents.map((ref, index) => (
              <SubagentRow
                // By position, not by id: a row the operator has not filled in
                // yet has no id, and two of them would collide on `''`, which
                // React resolves by reusing one input for both.
                key={index}
                subagentRef={ref}
                index={index}
                // The agent itself is never offered, and neither is one already
                // chosen. Both are refused at save, and a picker that offers
                // what the save refuses teaches the wrong thing.
                options={delegable.filter(
                  (agent) =>
                    agent.id === ref.id || !chosenSubagents.has(agent.id),
                )}
                // The form's value, not the saved one, so the hint follows
                // the environment picker further down the page as it moves.
                onChange={(next) => {
                  setSubagent(index, next);
                }}
                onRemove={() => {
                  setSubagents(
                    form.subagents.filter((unused, at) => at !== index),
                  );
                }}
              />
            ))}
          </ul>

          <Button
            variant="secondary"
            disabled={form.subagents.length >= delegable.length}
            onClick={() => {
              setSubagents([
                ...form.subagents,
                { id: '', prompt: '', permission: 'allow' },
              ]);
            }}
          >
            <Plus aria-hidden="true" />
            {t('agents.subagentAdd')}
          </Button>
        </>
      )}
    </Section>
  );
}
