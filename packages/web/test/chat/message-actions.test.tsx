/**
 * The message action bar.
 *
 * Two things are worth asserting here and are not obvious from the markup: the
 * action *set* differs by side, which is the design rather than an oversight;
 * and every one of the buttons is an icon, so every one of them needs a name a
 * screen reader can read. `a11y.test.tsx` sweeps the sources for that rule; this
 * checks the rendered result, which is the half a source sweep cannot see.
 */

import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { MessageActions } from '@/chat/message-actions.js';
import { renderWithProviders } from '@testkit/render.js';

/**
 * jsdom has no clipboard and `src/test/setup.ts` stubs none, but
 * `userEvent.setup()` installs one — and it wins over anything a `beforeEach`
 * defines, because it is installed later. So the assertion reads from that,
 * rather than fighting it with a spy that would never be called.
 */

describe('the message action bar', () => {
  it('offers copy alone when nothing else is possible', () => {
    renderWithProviders(<MessageActions text="hello" busy={false} />);

    expect(
      screen.getByRole('button', { name: 'Copy message' }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Edit this message' }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: 'Regenerate the answer' }),
    ).not.toBeInTheDocument();
  });

  it('names every icon button, because none of them has visible text', () => {
    renderWithProviders(
      <MessageActions
        text="hello"
        busy={false}
        onEdit={() => undefined}
        onRegenerate={() => undefined}
        onBranch={() => undefined}
      />,
    );

    for (const name of [
      'Copy message',
      'Edit this message',
      'Regenerate the answer',
      'Branch from here',
    ]) {
      expect(screen.getByRole('button', { name })).toBeInTheDocument();
    }
  });

  it('copies the message and confirms on the button rather than in a toast', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MessageActions text="the answer" busy={false} />);

    await user.click(screen.getByRole('button', { name: 'Copy message' }));

    expect(await navigator.clipboard.readText()).toBe('the answer');
    // The check mark is where the eye already is; a toast would be a
    // notification about something the user just did and is looking at.
    expect(
      await screen.findByRole('button', { name: 'Copied' }),
    ).toBeInTheDocument();
  });

  it('disables everything that would start a turn while one is running', () => {
    renderWithProviders(
      <MessageActions
        text="hello"
        busy
        onEdit={() => undefined}
        onRegenerate={() => undefined}
        onBranch={() => undefined}
      />,
    );

    expect(
      screen.getByRole('button', { name: 'Edit this message' }),
    ).toBeDisabled();
    expect(
      screen.getByRole('button', { name: 'Regenerate the answer' }),
    ).toBeDisabled();
    expect(
      screen.getByRole('button', { name: 'Branch from here' }),
    ).toBeDisabled();
    // Copy asks the server nothing, so a running turn is no reason to refuse it.
    expect(screen.getByRole('button', { name: 'Copy message' })).toBeEnabled();
  });

  it('reports each action to its caller', async () => {
    const user = userEvent.setup();
    const onEdit = vi.fn();
    const onRegenerate = vi.fn();
    const onBranch = vi.fn();

    renderWithProviders(
      <MessageActions
        text="hello"
        busy={false}
        onEdit={onEdit}
        onRegenerate={onRegenerate}
        onBranch={onBranch}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Edit this message' }));
    await user.click(
      screen.getByRole('button', { name: 'Regenerate the answer' }),
    );
    await user.click(screen.getByRole('button', { name: 'Branch from here' }));

    expect(onEdit).toHaveBeenCalledOnce();
    expect(onRegenerate).toHaveBeenCalledOnce();
    expect(onBranch).toHaveBeenCalledOnce();
  });
});
