/**
 * The agent editor's tool rows, in the list markup every group shares.
 */

import type { JSX } from 'react';

import { cn } from '@/lib/cn.js';
import { ExecRulesButton } from '@/agents/exec-rules.js';
import { ToolRow, parameterFields } from '@/agents/tool-row.js';
import type { AgentEditor } from '@/agents/use-agent-editor.js';

import { EXEC_TOOL, TOOL_SEARCH } from './constants.js';

/**
 * One list, so the groups that use it cannot drift apart.
 *
 * They render the same control over the same state and differ only in which
 * heading they sit under. That is exactly the case where two copies of the JSX
 * end up with two sets of props.
 */
export function ToolList({
  editor,
  names,
}: {
  readonly editor: AgentEditor;
  readonly names: readonly string[];
}): JSX.Element {
  const {
    form,
    errors,
    registered,
    toolsOff,
    switches,
    update,
    setPinned,
    setToolPermission,
    setToolPrompt,
  } = editor;

  return (
    <ul
      className={cn(
        'stack agent-editor__tools',
        toolsOff && 'agent-editor__tools--off',
      )}
    >
      {names.map((name) => {
        const tool = registered.get(name);
        return (
          <ToolRow
            key={name}
            name={name}
            detail={tool?.description ?? ''}
            risk={tool?.risk}
            permission={form.tools[name] ?? 'deny'}
            fields={tool === undefined ? [] : parameterFields(tool.parameters)}
            override={form.toolPrompts[name]}
            disabled={toolsOff}
            // Only while the short list is on, and never on the door itself:
            // `tool_search` is what the pins sit beside.
            pin={
              switches.lazyDiscovery.checked && name !== TOOL_SEARCH
                ? {
                    pinned: form.pinnedTools.includes(name),
                    onChange: (pinned) => {
                      setPinned(name, pinned);
                    },
                  }
                : undefined
            }
            settings={
              name === EXEC_TOOL ? (
                <ExecRulesButton
                  rules={form.execRules}
                  shell={form.execShell}
                  errors={errors}
                  disabled={toolsOff || (form.tools[name] ?? 'deny') === 'deny'}
                  onRulesChange={(next) => {
                    update('execRules', next);
                  }}
                  onShellChange={(next) => {
                    update('execShell', next);
                  }}
                />
              ) : undefined
            }
            onChange={(next) => {
              setToolPermission(name, next);
            }}
            onOverrideChange={(next) => {
              setToolPrompt(name, next);
            }}
          />
        );
      })}
    </ul>
  );
}

/**
 * `tool_search`, in its own group under the switch that makes it exist.
 *
 * No permission select and no pin: the scope always admits it and it is
 * what the pins sit beside. Its wording is still the operator's to rewrite,
 * which is why it is a `ToolRow` rather than a line of text.
 */
export function ToolSearchRow({
  editor,
}: {
  readonly editor: AgentEditor;
}): JSX.Element | null {
  const { form, registered, toolsOff, setToolPrompt } = editor;
  const tool = registered.get(TOOL_SEARCH);
  if (tool === undefined) return null;
  return (
    <ul
      className={cn(
        'stack agent-editor__tools',
        toolsOff && 'agent-editor__tools--off',
      )}
    >
      <ToolRow
        name={TOOL_SEARCH}
        detail={tool.description}
        risk={tool.risk}
        permission="allow"
        alwaysOn
        fields={parameterFields(tool.parameters)}
        override={form.toolPrompts[TOOL_SEARCH]}
        disabled={toolsOff}
        onChange={() => {
          // Unreachable: an always-on row renders no permission select.
        }}
        onOverrideChange={(next) => {
          setToolPrompt(TOOL_SEARCH, next);
        }}
      />
    </ul>
  );
}
