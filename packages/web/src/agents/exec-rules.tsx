/**
 * The `exec` tool's command rules, behind a button on its row.
 *
 * A dialog for the reason the wording editor is one: the tool list is a
 * scrolling box of one-line rows, and a table opened inside it would push every
 * row below it down while giving itself the width of a column.
 *
 * The row's own select stays the fallback for a command no rule covers, so it
 * is not repeated here. What is here is the rules, and the one setting that
 * caps them: how far a shell call may go.
 */

import { ListChecks, Plus, Trash2 } from 'lucide-react';
import { useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import type { ToolPermission } from '@darkwire/protocol';

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
import { SelectField, TextField } from '@/components/form/controls.js';

import {
  TOOL_PERMISSIONS,
  isToolPermission,
  type ExecRuleRow,
} from './agents-form.js';

export function ExecRulesButton({
  rules,
  shell,
  errors,
  disabled,
  onRulesChange,
  onShellChange,
}: {
  readonly rules: readonly ExecRuleRow[];
  readonly shell: ToolPermission;
  /** Keyed `execRules.<index>`, from the last save. */
  readonly errors: Readonly<Record<string, string>>;
  readonly disabled: boolean;
  readonly onRulesChange: (next: readonly ExecRuleRow[]) => void;
  readonly onShellChange: (next: ToolPermission) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);

  const actionOptions = TOOL_PERMISSIONS.map((option) => ({
    value: option,
    label: t(`agents.execRules.action.${option}`),
  }));

  const change = (index: number, next: ExecRuleRow): void => {
    onRulesChange(rules.map((rule, at) => (at === index ? next : rule)));
  };

  return (
    <>
      <Button
        variant="ghost"
        size="sm"
        className="agent-editor__tool-settings"
        disabled={disabled}
        aria-label={t('agents.execRules.open', { rules: rules.length })}
        onClick={() => {
          setOpen(true);
        }}
      >
        <ListChecks aria-hidden="true" />
        {rules.length > 0 && <span>{rules.length}</span>}
      </Button>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="agent-editor__rules-dialog">
          <DialogHeader>
            <DialogHeading>{t('agents.execRules.title')}</DialogHeading>
            <DialogSubheading>{t('agents.execRules.desc')}</DialogSubheading>
          </DialogHeader>

          <div className="stack agent-editor__rules-body">
            {rules.length === 0 ? (
              <p className="agent-editor__hint">
                {t('agents.execRules.empty')}
              </p>
            ) : (
              <ul className="stack agent-editor__rules">
                {rules.map((rule, index) => {
                  const position = index + 1;
                  return (
                    // Positional, because a rule has no id and two rows may
                    // hold the same text while one is being edited.
                    <li key={index} className="row agent-editor__rule">
                      <div className="agent-editor__rule-action">
                        <SelectField
                          label={
                            <span className="sr-only">
                              {t('agents.execRules.actionFor', { position })}
                            </span>
                          }
                          value={rule.action}
                          options={actionOptions}
                          onValueChange={(next) => {
                            if (isToolPermission(next)) {
                              change(index, { ...rule, action: next });
                            }
                          }}
                        />
                      </div>
                      <div className="agent-editor__rule-pattern">
                        <TextField
                          label={
                            <span className="sr-only">
                              {t('agents.execRules.patternFor', { position })}
                            </span>
                          }
                          value={rule.pattern}
                          placeholder={t('agents.execRules.patternPlaceholder')}
                          error={errors[`execRules.${String(index)}`]}
                          spellCheck={false}
                          autoComplete="off"
                          onValueChange={(next) => {
                            change(index, { ...rule, pattern: next });
                          }}
                        />
                      </div>
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label={t('agents.execRules.remove', { position })}
                        onClick={() => {
                          onRulesChange(
                            rules.filter((other, at) => at !== index),
                          );
                        }}
                      >
                        <Trash2 aria-hidden="true" />
                      </Button>
                    </li>
                  );
                })}
              </ul>
            )}
            <div>
              <Button
                variant="secondary"
                size="sm"
                onClick={() => {
                  onRulesChange([...rules, { action: 'allow', pattern: '' }]);
                }}
              >
                <Plus aria-hidden="true" />
                {t('agents.execRules.add')}
              </Button>
            </div>
            <p className="agent-editor__hint">{t('agents.execRules.hint')}</p>

            <SelectField
              label={t('agents.execRules.shell')}
              hint={t('agents.execRules.shellHint')}
              value={shell}
              options={actionOptions}
              onValueChange={(next) => {
                if (isToolPermission(next)) onShellChange(next);
              }}
            />
          </div>

          <DialogFooter>
            <DialogClose asChild>
              <Button>{t('common.done')}</Button>
            </DialogClose>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
