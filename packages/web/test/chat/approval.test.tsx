/**
 * The approval prompt's three states, driven directly.
 *
 * All three are transient in a running app — the gate answers, the loop moves
 * on and the prompt is replaced — which is exactly why they are asserted here
 * rather than end-to-end. `approvals.spec.ts` used to check the resolved line
 * from the browser and raced a scripted provider that answers inside a frame;
 * it now asserts the durable card status, and the wording the operator reads
 * between pressing a button and the tool result arriving is covered here, where
 * the state is held still.
 */

import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ToolApprovalState } from '@/state/transcript.js';
import { renderWithProviders } from '@testkit/render.js';

import { ApprovalPrompt } from '@/chat/approval.js';

/** A deadline far enough out that the countdown is not what is under test. */
const OPEN: ToolApprovalState = { expiresAtMs: 0, answered: undefined };

// Through the real provider stack, because the scope buttons are wrapped in
// `Tooltip` and a bare `render` throws on the missing provider rather than
// telling you anything about the prompt.
const promptWith = (
  approval: Partial<ToolApprovalState>,
  onAnswer = vi.fn(),
) => {
  renderWithProviders(
    <ApprovalPrompt
      toolName="exec"
      approval={{ ...OPEN, expiresAtMs: Date.now() + 60_000, ...approval }}
      onAnswer={onAnswer}
    />,
  );
  return onAnswer;
};

describe('an unanswered prompt', () => {
  it('names the tool and offers the three scopes plus a denial', async () => {
    const onAnswer = promptWith({});

    expect(screen.getByText('exec')).toBeInTheDocument();
    expect(screen.getByText(/needs approval to run/)).toBeInTheDocument();

    // Each scope is a different promise, so each button has to send its own —
    // a prompt where every button means "once" looks identical on screen.
    await userEvent.click(screen.getByRole('button', { name: 'Once' }));
    expect(onAnswer).toHaveBeenCalledWith(true, 'once');

    await userEvent.click(screen.getByRole('button', { name: 'This session' }));
    expect(onAnswer).toHaveBeenCalledWith(true, 'session');

    await userEvent.click(screen.getByRole('button', { name: 'Always' }));
    expect(onAnswer).toHaveBeenCalledWith(true, 'always');

    // A denial is scoped to the call whatever the operator pressed before it.
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(onAnswer).toHaveBeenCalledWith(false, 'once');
  });
});

describe('a prompt this tab answered', () => {
  it.each([
    ['approved' as const, 'Approved — waiting for the agent.'],
    ['denied' as const, 'Denied — waiting for the agent.'],
  ])('reports %s and stops offering buttons', (answered, line) => {
    promptWith({ answered });

    expect(screen.getByText(line)).toBeInTheDocument();
    // The point of the state: the decision is gone the moment it is made, so a
    // second press cannot answer a gate that already has its answer.
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });
});

describe('a prompt nobody answered in time', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('says the call was refused rather than leaving buttons that do nothing', () => {
    vi.setSystemTime(new Date('2026-07-28T12:00:00Z'));
    promptWith({ expiresAtMs: Date.now() - 1 });

    expect(
      screen.getByText(/The approval window closed\. The call was refused\./),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button')).not.toBeInTheDocument();
  });
});
