/**
 * One row of the agent editor's delegation list.
 *
 * Lifted out of `agent-editor.tsx` whole. Shares no state with the editor.
 */

import { Trash2 } from 'lucide-react';
import type { JSX } from 'react';
import { useTranslation } from 'react-i18next';

import {
  defaultSubagentPrompt,
  subagentToolName,
  type SubagentRef,
} from '@ghostwire/protocol';

import { Button } from '@/components/ui/button.js';
import {
  SelectField,
  SwitchRow,
  TextareaField,
} from '@/components/form/controls.js';

import { TOOL_PERMISSIONS, isToolPermission } from './agents-form.js';
import { PERMISSION_LABELS } from './tool-permissions.js';

/**
 * One agent this one may hand a task to.
 *
 * Four controls, in the order the decision is made: *who*, *when to use them*,
 * *whether to ask first*, and *where it runs*. The guidance box is the largest
 * because it is the one that matters most: it becomes the tool description the
 * model reads, and it is the only part of this feature that decides when a
 * delegation actually fires.
 *
 * The tool name is shown rather than asked for. It is derived from the agent id
 * so two subagents can never collide and none can shadow a built-in; showing it
 * is what makes an operator's prompt ("use `ask_researcher` when…") match what
 * the model is really offered.
 *
 * The placement switch is last because it is the only control here that is
 * about the delegation rather than about the agent, and it names the caller's
 * environment rather than describing a mechanism. Where it runs used to be
 * implied by the target naming no environment of its own, which an operator
 * reading this row had no way to see at all.
 */
export function SubagentRow({
  subagentRef,
  index,
  options,
  callerEnvironment,
  onChange,
  onRemove,
}: {
  readonly subagentRef: SubagentRef;
  readonly index: number;
  readonly options: ReadonlyArray<{
    readonly id: string;
    readonly label: string;
  }>;
  /** The environment the editing agent runs in. Empty is the host. */
  readonly callerEnvironment: string;
  readonly onChange: (next: SubagentRef) => void;
  readonly onRemove: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const position = index + 1;
  // The target's label, which is what the generated description names. Read off
  // the option list rather than stored on the reference, for the same reason
  // `labelOf` reads it off the target's entry: one place to rename an agent.
  const targetLabel =
    options.find((agent) => agent.id === subagentRef.id)?.label ?? '';

  return (
    <li className="stack agent-editor__subagent">
      <div className="row agent-editor__subagent-head">
        {/* Wrapped, like the permission select beside it, so the stylesheet has
            something to place. The two are the same kind of thing and the phone
            layout has to address them both. */}
        <div className="agent-editor__subagent-agent">
          <SelectField
            label={
              <span className="sr-only">
                {t('agents.subagentAgentFor', { position })}
              </span>
            }
            value={subagentRef.id}
            placeholder={t('agents.subagentChoose')}
            options={options.map((agent) => ({
              value: agent.id,
              label: agent.label === '' ? agent.id : agent.label,
            }))}
            onValueChange={(next) => {
              onChange({ ...subagentRef, id: next });
            }}
          />
        </div>

        {subagentRef.id !== '' && (
          <code className="agent-editor__subagent-tool">
            {subagentToolName(subagentRef.id)}
          </code>
        )}

        <div className="agent-editor__subagent-permission">
          <SelectField
            label={
              <span className="sr-only">
                {t('agents.subagentPermissionFor', { position })}
              </span>
            }
            value={subagentRef.permission}
            options={TOOL_PERMISSIONS.map((option) => ({
              value: option,
              label: t(PERMISSION_LABELS[option]),
            }))}
            onValueChange={(next) => {
              if (isToolPermission(next)) {
                onChange({ ...subagentRef, permission: next });
              }
            }}
          />
        </div>

        <Button
          variant="ghost"
          aria-label={t('agents.subagentRemove', { position })}
          onClick={onRemove}
        >
          <Trash2 aria-hidden="true" />
        </Button>
      </div>

      {/* The one box on this page that is not a single line, and it earns it by
          what it has to *show* rather than by what gets typed into it. The
          placeholder is the description the model reads when this is left
          empty — a couple of sentences — and in an `<input>` an operator saw
          about forty characters of it, which left the default as invisible as
          having no placeholder at all. It grows with its content, so a one-word
          answer still costs one line. */}
      <TextareaField
        label={
          <span className="sr-only">
            {t('agents.subagentPromptFor', { position })}
          </span>
        }
        value={subagentRef.prompt}
        // The sentence the model would actually read, not an invented example
        // of one an operator might write — which would leave "leave it empty for
        // a generic one" as the only clue about a default nobody can see.
        //
        // A placeholder rather than a prefilled value on purpose: it is built
        // from the *target's* current label, so it follows a rename. Written
        // into the reference it would freeze the name the agent had on the day
        // the delegation was added, and nothing would ever correct it.
        placeholder={
          subagentRef.id === ''
            ? ''
            : defaultSubagentPrompt(
                targetLabel === '' ? subagentRef.id : targetLabel,
              )
        }
        hint={t('agents.subagentPromptHint')}
        rows={2}
        onValueChange={(next) => {
          onChange({ ...subagentRef, prompt: next });
        }}
      />

      {/* Names the place rather than the rule. "Runs where this agent runs" is
          a mechanism an operator has to hold in their head; "Runs in node-20"
          is the answer they were looking for. */}
      <SwitchRow
        label={t('agents.subagentInheritFor', { position })}
        hint={
          subagentRef.inheritEnvironment
            ? callerEnvironment === ''
              ? t('agents.subagentInheritHostHint')
              : t('agents.subagentInheritHint', {
                  environment: callerEnvironment,
                })
            : t('agents.subagentInheritOffHint')
        }
        checked={subagentRef.inheritEnvironment}
        onCheckedChange={(next) => {
          onChange({ ...subagentRef, inheritEnvironment: next });
        }}
      />
    </li>
  );
}
