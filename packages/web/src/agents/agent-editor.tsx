/**
 * Editing one agent.
 *
 * A route of its own rather than a panel beside a list, because an agent has
 * several sections' worth of settings and a master/detail that put both on one
 * screen made the list a narrow column of nothing and the form a scroll. The
 * list picks; this edits; the back link returns.
 *
 * **The default agent is edited here too, and it is the reason Settings has no
 * "Agent" panel.** It is an entry in `agents.list` like any other, so a second
 * screen for the same fields would be two doors into one room.
 *
 * **It is the same screen, field for field**, with no branch anywhere for which
 * agent is being edited: there is one subtree, so every control writes to the
 * same place.
 *
 * **Nothing on this screen inherits.** Every box holds this agent's own value,
 * and one opened on an agent that stored none is filled from the defaults so
 * that saving writes them down — see the note at the top of `agents-form.ts`.
 * There is no "Inherit — …" option and no empty box that means "ask the other
 * screen": a setting you cannot read off the screen it is on is a setting an
 * operator has to go and derive.
 *
 * **The system prompt is last.** It is the tallest thing on the screen by a
 * wide margin — a full editor holding a page of text — and in the middle it
 * split the short settings into a group above and a group below, so reaching
 * the tools meant scrolling past a document. Everything that fits on a line
 * comes first; the one thing that does not comes after, where it can be as tall
 * as it likes without pushing anything.
 */

import { Link, useParams } from '@tanstack/react-router';
import { ArrowLeft, Trash2 } from 'lucide-react';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import { AgentEntrySchema, DEFAULT_AGENT_ID } from '@darkwire/protocol';

import { PageTitle } from '@/app/page-title.js';
import { Badge } from '@/components/ui/badge.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { RowActions } from '@/components/crud/row-actions.js';
import { SaveBar } from '@/components/form/controls.js';
import { useSettings } from '@/settings/use-settings.js';
import { EnvironmentSection } from './editor/environment-section.js';
import { FeatureToolsSection } from './editor/feature-tools-section.js';
import { IdentitySection } from './editor/identity-section.js';
import { LimitsSection } from './editor/limits-section.js';
import { ModelSection } from './editor/model-section.js';
import { PromptSection } from './editor/prompt-section.js';
import { SubagentsSection } from './editor/subagents-section.js';
import { ToolsSection } from './editor/tools-section.js';
import { useAgentEditor, type AgentEditorProps } from './use-agent-editor.js';

/**
 * Creating an agent, on the page that edits one.
 *
 * The same `Editor`, seeded from the default agent's entry — which is exactly
 * what the dialog it replaced used as its template, only now the operator sees
 * it and can change it *before* anything is written. Nothing reaches the
 * settings tree until Save, so an abandoned create leaves no agent behind.
 */
export function AgentCreateRoute(): JSX.Element {
  const { t } = useTranslation();
  const settings = useSettings();

  if (settings.isPending) {
    return <p className="page__note">{t('agents.loading')}</p>;
  }
  if (settings.isError) {
    return (
      <p role="alert" className="page__error">
        {t('agents.loadDefaultsError', { message: settings.error.message })}
      </p>
    );
  }

  const { config } = settings.data;
  // The template a new agent is stamped from, and the same one `toNewAgentPatch`
  // used: the default agent as it actually stands, not the schema's defaults.
  const template =
    config.agents.list[DEFAULT_AGENT_ID] ?? AgentEntrySchema.parse({});

  return (
    <Editor
      mode="create"
      agentId=""
      entry={template}
      list={config.agents.list}
    />
  );
}

export function AgentEditorRoute(): JSX.Element {
  const { t } = useTranslation();
  const { agentId } = useParams({ from: '/agents/$agentId' });
  const settings = useSettings();

  if (settings.isPending) {
    return <p className="page__note">{t('agents.loading')}</p>;
  }
  if (settings.isError) {
    return (
      <p role="alert" className="page__error">
        {t('agents.loadOneError', { message: settings.error.message })}
      </p>
    );
  }

  const { config } = settings.data;
  const isDefault = agentId === DEFAULT_AGENT_ID;
  const entry = config.agents.list[agentId];

  // A named agent that is not in the settings is a stale link — a bookmark to
  // one that was deleted, or a hand-typed id. Saying so beats an empty form
  // that silently creates it on the first save.
  if (entry === undefined && !isDefault) {
    return (
      <div className="stack page page--wide">
        <p role="alert" className="page__error">
          {t('agents.noSuchAgent', { id: agentId })}
        </p>
        <Link to="/agents" className="page__back">
          <ArrowLeft aria-hidden="true" />
          {t('agents.backToAgents')}
        </Link>
      </div>
    );
  }

  return (
    <Editor
      // Remounts on a change of agent, so one agent's edits cannot survive into
      // the next one's boxes.
      key={agentId}
      mode="edit"
      agentId={agentId}
      entry={entry ?? AgentEntrySchema.parse({})}
      // The whole list, not the `/api/agents` listing: that one omits the
      // disabled agents, and a rename onto a switched-off agent's id is still
      // the collision the server refuses with a 409.
      list={config.agents.list}
    />
  );
}

function Editor(props: AgentEditorProps): JSX.Element {
  const { t } = useTranslation();
  const editor = useAgentEditor(props);
  const {
    creating,
    isDefault,
    name,
    resolved,
    form,
    dirty,
    saving,
    onSave,
    onRevert,
    onDelete,
  } = editor;
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  return (
    <div className="stack page page--wide agent-editor">
      <div className="editor__head">
        <Link to="/agents" className="page__back">
          <ArrowLeft aria-hidden="true" />
          Agents
        </Link>

        <div className="cluster editor__title">
          <PageTitle title={name} />
          <h1 className="page__title">{name}</h1>
          {isDefault && <Badge>default</Badge>}
          <span className="spacer" />
          {/* Not a section at the bottom of the form any more. A destructive
              action does not belong in the reading order of the settings it
              would destroy, and it used to fire without asking. Absent while
              creating: there is nothing yet to delete. */}
          {!isDefault && !creating && (
            <RowActions label={name}>
              <DropdownMenuItem
                className="menu__item--danger"
                onSelect={() => {
                  setConfirmingDelete(true);
                }}
              >
                <Trash2 />
                {t('agents.deleteAgent')}
              </DropdownMenuItem>
            </RowActions>
          )}
        </div>

        <p className="page__note">
          {/* A switched-off agent is absent from `/api/agents` just as a
              deleted one is, so `resolved` being undefined cannot on its own
              mean "no model". Asked in this order, the disabled case answers
              for itself and the model line is only reached by an agent that
              could actually run. Otherwise a disabled agent was told it had
              no model, and choosing one would not have helped. */}
          {isDefault
            ? 'Every session that names no agent runs on this one, and a new agent starts as a copy of it.'
            : !form.enabled
              ? 'Switched off: it cannot take a turn, and it is hidden from the picker. Its settings and its sessions are kept.'
              : `Runs on ${resolved?.model === '' || resolved === undefined ? 'no model yet. It cannot take a turn until one is chosen' : resolved.model}.`}
        </p>
      </div>

      <IdentitySection editor={editor} />
      <ModelSection editor={editor} />
      <ToolsSection editor={editor} />
      <FeatureToolsSection editor={editor} />

      {/* After the tools, because delegating *is* a tool from the model's side
          (one per subagent, named after it), and an operator reading down the
          page has just decided what this agent may do on its own. */}
      <SubagentsSection editor={editor} />

      {/* After the tools, because an environment decides where exec runs. */}
      <EnvironmentSection editor={editor} />

      {/* Below the tools and above the prompt, in plain sight. It sat behind a
          "Show limits" press on the theory that nobody changes a budget twice a
          year. But the numbers are now this agent's own rather than inherited,
          and a setting an operator has to go looking for to find out what it
          says is not one they can be said to have chosen. */}
      <LimitsSection editor={editor} />

      <PromptSection editor={editor} />

      <SaveBar
        dirty={dirty}
        saving={saving}
        onSave={onSave}
        onRevert={onRevert}
      />

      <ConfirmDialog
        open={confirmingDelete}
        onOpenChange={setConfirmingDelete}
        title={t('agents.deleteTitle')}
        description={`${name} is removed from the settings. Its sessions keep their history and fall back to the default agent.`}
        confirmLabel="Delete"
        pending={saving}
        onConfirm={onDelete}
      />
    </div>
  );
}
