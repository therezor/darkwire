/**
 * The `exec` command rules dialog, driven directly.
 */

import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { renderWithProviders } from '@testkit/render.js';

import { ExecRulesButton } from '@/agents/exec-rules.js';
import type { ExecRuleRow } from '@/agents/agents-form.js';

function dialogWith(
  rules: readonly ExecRuleRow[],
  errors: Readonly<Record<string, string>> = {},
) {
  const onRulesChange = vi.fn();
  const onShellChange = vi.fn();
  renderWithProviders(
    <ExecRulesButton
      rules={rules}
      shell="ask"
      errors={errors}
      disabled={false}
      onRulesChange={onRulesChange}
      onShellChange={onShellChange}
    />,
  );
  return { onRulesChange, onShellChange };
}

describe('the command rules dialog', () => {
  it('opens from a button that counts the rules', async () => {
    dialogWith([{ action: 'allow', pattern: 'git *' }]);
    await userEvent.click(
      screen.getByRole('button', { name: 'Command rules (1)' }),
    );

    const dialog = screen.getByRole('dialog');
    expect(
      within(dialog).getByLabelText('Command pattern for rule 1'),
    ).toHaveValue('git *');
  });

  it('says there are none, and adds an empty allow row', async () => {
    const { onRulesChange } = dialogWith([]);
    await userEvent.click(
      screen.getByRole('button', { name: 'Command rules (0)' }),
    );
    expect(
      screen.getByText(
        'No rules yet. Every command gets the tool’s own permission.',
      ),
    ).toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Add rule' }));
    expect(onRulesChange).toHaveBeenCalledWith([
      { action: 'allow', pattern: '' },
    ]);
  });

  it('edits and removes a rule in place', async () => {
    const { onRulesChange } = dialogWith([
      { action: 'allow', pattern: 'git' },
      { action: 'deny', pattern: 'rm *' },
    ]);
    await userEvent.click(
      screen.getByRole('button', { name: 'Command rules (2)' }),
    );

    await userEvent.type(
      screen.getByLabelText('Command pattern for rule 1'),
      '!',
    );
    expect(onRulesChange).toHaveBeenLastCalledWith([
      { action: 'allow', pattern: 'git!' },
      { action: 'deny', pattern: 'rm *' },
    ]);

    await userEvent.click(
      screen.getByRole('button', { name: 'Remove rule 1' }),
    );
    expect(onRulesChange).toHaveBeenLastCalledWith([
      { action: 'deny', pattern: 'rm *' },
    ]);
  });

  it('shows the error from the last save under its rule', async () => {
    dialogWith([{ action: 'allow', pattern: 'x "y' }], {
      'execRules.0': 'A quote is not closed.',
    });
    await userEvent.click(
      screen.getByRole('button', { name: 'Command rules (1)' }),
    );
    expect(screen.getByText('A quote is not closed.')).toBeInTheDocument();
  });
});
