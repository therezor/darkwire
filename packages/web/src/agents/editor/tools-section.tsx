import { Wrench } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { NoticeBlock } from '@/components/ui/notice.js';
import { Section, SwitchRow } from '@/components/form/controls.js';
import type { AgentEditor } from '@/agents/use-agent-editor.js';

import { ToolList, ToolSearchRow } from './tool-list.js';

/**
 * The tools this agent may call.
 *
 * One row per tool, one control on it. Enabling a tool and choosing what
 * happens when it runs are the same act (`deny` is the off position), so
 * there is no switch beside the select and no mode toggle above the list.
 */
export function ToolsSection({
  editor,
}: {
  readonly editor: AgentEditor;
}): JSX.Element {
  const { t } = useTranslation();
  const { tools, toolsOff, switches, actionToolNames, mcpToolNames } = editor;

  return (
    <Section
      title={t('agents.toolsSection')}
      description={t('agents.toolsDesc')}
    >
      {tools.isPending && (
        <p className="page__note">{t('agents.loadingTools')}</p>
      )}
      {/* Above the list rather than in place of it. The rows say what this
          agent is configured to do, and that is still true and still saved.
          What has changed is only that this model is not being told about any
          of it. Emptying the list would read as the config being gone, which
          is the one thing the switch must not do. */}
      {toolsOff && (
        <NoticeBlock
          tone="warning"
          icon={Wrench}
          title={t('agents.toolsOffTitle')}
          message={t('agents.toolsOffNote')}
        />
      )}
      {/* The door first, because it changes what every row below means: on,
          each row gains a pin, and the unpinned ones are what the model has
          to search for. The row for `tool_search` itself appears only while
          the switch is on, which is exactly when it is sent. */}
      <SwitchRow
        label={t('agents.lazyDiscovery')}
        hint={t('agents.lazyDiscoveryHint')}
        checked={switches.lazyDiscovery.checked}
        onCheckedChange={switches.lazyDiscovery.set}
      />
      {switches.lazyDiscovery.checked && <ToolSearchRow editor={editor} />}

      {actionToolNames.length > 0 && (
        <ToolList editor={editor} names={actionToolNames} />
      )}

      {/* Directly under the action list, because these are action tools:
          calls the model makes during a turn, under the same permissions.
          What separates them is where they came from, which is the thing an
          operator is scanning for when a row is missing or unfamiliar. Not
          split per server: `localeCompare` already clusters `mcp_github_*`
          ahead of `mcp_linear_*`, so the list reads by server for free. */}
      {mcpToolNames.length > 0 && (
        <>
          <h3 className="agent-editor__tool-group">
            {t('agents.mcpToolsGroup')}
          </h3>
          <p className="page__note">{t('agents.mcpToolsNote')}</p>
          <ToolList editor={editor} names={mcpToolNames} />
        </>
      )}
    </Section>
  );
}
