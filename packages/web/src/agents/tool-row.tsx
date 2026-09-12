/**
 * One row of the agent editor's tool list, and the dialog behind it.
 *
 * Lifted out of `agent-editor.tsx` whole, along with the reading of a tool's
 * advertised JSON Schema that decides which override boxes the dialog shows.
 * Nothing here is shared with the editor beyond its props.
 */

import { RotateCcw, SquarePen } from 'lucide-react';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type {
  ToolPermission,
  ToolPromptOverride,
  ToolRisk,
} from '@ghostwire/protocol';

import { Badge } from '@/components/ui/badge.js';
import { Button } from '@/components/ui/button.js';
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogHeading,
  DialogSubheading,
} from '@/components/ui/dialog.js';
import { SelectField, TextareaField } from '@/components/form/controls.js';

import { TOOL_PERMISSIONS, isToolPermission } from './agents-form.js';
import { PERMISSION_LABELS } from './tool-permissions.js';

/** Shared so an unopened row does not allocate one per render. */
const EMPTY_OVERRIDE: ToolPromptOverride = { description: '', fields: {} };

/** One advertised argument, and what the tool itself says about it. */
interface ToolField {
  readonly name: string;
  /** The built-in description, or empty when the schema gives none. */
  readonly description: string;
}

/**
 * The top-level arguments in a tool's advertised JSON Schema, with their own
 * descriptions — which are what the boxes below show as placeholders.
 *
 * A generic "the built-in description" told an operator nothing: the whole
 * question they are answering is whether the built-in wording is good enough,
 * and they cannot answer it without reading it. Showing the real one means an
 * empty box is a legible statement about what the model is being told.
 *
 * Top level only, which is the same bound `applyToolPrompts` enforces: an
 * override reaching `argv.items` would need a path syntax to specify and to
 * validate, for a field whose parent can say the same thing in a sentence.
 */
export function parameterFields(
  parameters: Readonly<Record<string, unknown>>,
): ToolField[] {
  const properties = parameters.properties;
  if (
    typeof properties !== 'object' ||
    properties === null ||
    Array.isArray(properties)
  ) {
    return [];
  }

  return Object.entries(properties as Record<string, unknown>).map(
    ([name, schema]) => {
      const described =
        typeof schema === 'object' && schema !== null && !Array.isArray(schema)
          ? (schema as Record<string, unknown>).description
          : undefined;
      return {
        name,
        description: typeof described === 'string' ? described : '',
      };
    },
  );
}

/**
 * One tool, and the single control that decides everything about it.
 *
 * There is no on/off switch beside the select, because there is nothing for one
 * to say: `Disabled` *is* the off position, and a switch would let an operator
 * express "off, but ask" — a state the config cannot hold and the runtime would
 * silently read as off.
 *
 * The label is the tool's name in mono, which is what it is: an identifier the
 * model types, not prose. The risk badge is advisory — it is what seeded this
 * permission when the agent was created, and it decides nothing now.
 */
export function ToolRow({
  name,
  detail,
  risk,
  permission,
  fields,
  override,
  disabled,
  onChange,
  onOverrideChange,
}: {
  readonly name: string;
  /** The tool's own description. The row shows the override instead when there is one. */
  readonly detail: string;
  readonly risk: ToolRisk | undefined;
  readonly permission: ToolPermission;
  /** Top-level arguments from the live schema. Empty when it is not registered. */
  readonly fields: readonly ToolField[];
  readonly override: ToolPromptOverride | undefined;
  /**
   * The row is shown but not editable, because this model is sent no tools.
   *
   * Not required, and that is the point: the stored permission and wording are
   * still rendered and still saved untouched, so switching the model back
   * restores the toolset exactly as it was. A list that emptied itself would
   * make the switch destructive.
   */
  readonly disabled: boolean;
  readonly onChange: (next: ToolPermission) => void;
  readonly onOverrideChange: (next: ToolPromptOverride) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const stored = override ?? EMPTY_OVERRIDE;
  const [editing, setEditing] = useState(false);
  const rewritten = stored.description !== '';

  const setField = (field: string, text: string): void => {
    onOverrideChange({
      ...stored,
      fields: { ...stored.fields, [field]: text },
    });
  };

  return (
    <li className="row agent-editor__tool">
      <div className="agent-editor__tool-text">
        <span className="agent-editor__tool-name truncate">{name}</span>
        {/* The row says what the model is told, not what the tool shipped with.
            Showing the built-in under a tool whose description an operator has
            replaced would make the list disagree with the payload. */}
        {(rewritten ? stored.description : detail) !== '' && (
          <span className="agent-editor__tool-detail truncate">
            {rewritten ? stored.description : detail}
          </span>
        )}
      </div>
      {risk === undefined ? (
        // Named in the config but not registered right now — an MCP server that
        // is down, or a tool that was removed. It stays on the list so a save
        // cannot drop it.
        <Badge tone="warning">{t('agents.toolNotInstalled')}</Badge>
      ) : (
        <Badge tone="neutral">{risk}</Badge>
      )}
      <Button
        variant="ghost"
        size="sm"
        className="agent-editor__tool-edit"
        disabled={disabled}
        // Named for the tool: a list of ten rows has ten of these, and an icon
        // called "Wording" tells a screen reader nothing about which.
        aria-label={t('agents.toolWordingFor', { name })}
        onClick={() => {
          setEditing(true);
        }}
      >
        <SquarePen aria-hidden="true" />
      </Button>
      <div className="agent-editor__tool-permission">
        <SelectField
          label={
            <span className="sr-only">
              {t('agents.toolPermissionFor', { name })}
            </span>
          }
          value={permission}
          disabled={disabled}
          options={TOOL_PERMISSIONS.map((option) => ({
            value: option,
            label: t(PERMISSION_LABELS[option]),
          }))}
          onValueChange={(next) => {
            if (isToolPermission(next)) onChange(next);
          }}
        />
      </div>

      {/* A dialog rather than an expander on the row. The list is a scrolling
          box of one-line rows, and pushing a form into the middle of it moved
          every row below the one being edited — while giving the form the width
          of a column sized for a permission select. */}
      <Dialog open={editing} onOpenChange={setEditing}>
        <DialogContent className="agent-editor__wording-dialog">
          <DialogHeader>
            <DialogHeading>
              {t('agents.toolWordingTitle', { name })}
            </DialogHeading>
            <DialogSubheading>{t('agents.toolWordingHint')}</DialogSubheading>
          </DialogHeader>

          <div className="stack agent-editor__wording-body">
            <TextareaField
              label={t('agents.toolWordingDesc')}
              value={stored.description}
              // The tool's real description, so an empty box is a legible
              // statement about what the model is being told rather than a
              // promise that something exists somewhere.
              placeholder={detail}
              rows={3}
              onValueChange={(next) => {
                onOverrideChange({ ...stored, description: next });
              }}
            />
            {fields.length > 0 && (
              <>
                <h4 className="agent-editor__wording-heading">
                  {t('agents.toolWordingArgs')}
                </h4>
                {fields.map((field) => (
                  <TextareaField
                    key={field.name}
                    label={field.name}
                    value={stored.fields[field.name] ?? ''}
                    placeholder={field.description}
                    rows={2}
                    onValueChange={(next) => {
                      setField(field.name, next);
                    }}
                  />
                ))}
              </>
            )}
            {/* The boundary that stops an override from breaking the tool: the
                model is told what these arguments mean, and the schema that
                accepts them is still generated from the tool's own definition. */}
            <p className="agent-editor__hint">{t('agents.toolWordingNote')}</p>
          </div>

          <DialogFooter>
            <Button
              variant="ghost"
              onClick={() => {
                onOverrideChange({ description: '', fields: {} });
              }}
            >
              <RotateCcw aria-hidden="true" />
              {t('agents.resetBuiltin')}
            </Button>
            <DialogClose asChild>
              <Button>{t('common.done')}</Button>
            </DialogClose>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </li>
  );
}
