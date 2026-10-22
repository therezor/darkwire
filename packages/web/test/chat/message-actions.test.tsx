/**
 * The message action bar.
 *
 * Two things are worth asserting here and are not obvious from the markup: the
 * action *set* differs by side, which is the design rather than an oversight;
 * and every one of the buttons is an icon, so every one of them needs a name a
 * screen reader can read. `a11y.test.tsx` sweeps the sources for that rule; this
 * checks the rendered result, which is the half a source sweep cannot see.
 */

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { MessageActions } from '@/chat/message-actions.js';
import { STYLES } from '@testkit/paths.js';
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

  it('disables what would start a turn while one is running, and nothing else', () => {
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
    // A branch is a new session, and the server refuses a point the running
    // turn has not finished, so a running turn is no reason to refuse it.
    expect(
      screen.getByRole('button', { name: 'Branch from here' }),
    ).toBeEnabled();
    // Copy asks the server nothing, so a running turn is no reason to refuse it.
    expect(screen.getByRole('button', { name: 'Copy message' })).toBeEnabled();
  });

  it('names Copy and Branch after what they act on, where asked to', () => {
    renderWithProviders(
      <MessageActions
        text="hello"
        busy={false}
        copyLabel="Copy step 2"
        branchLabel="Branch from step 2"
        onBranch={() => undefined}
      />,
    );

    expect(
      screen.getByRole('button', { name: 'Copy step 2' }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: 'Branch from step 2' }),
    ).toBeInTheDocument();
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

/**
 * jsdom lays nothing out, so the height of a message's line is invisible to
 * every other test here. Buttons that set the line's height are a hole after
 * every message of a long turn, and only a reader would ever notice it.
 */
describe("a message's line in the stylesheet", () => {
  const css = readFileSync(join(STYLES, 'screens/chat.css'), 'utf8');
  const rule = (selector: string): string =>
    new RegExp(`\\n  ${selector.replaceAll('.', '\\.')} \\{([^}]*)\\}`).exec(
      css,
    )?.[1] ?? '';

  it('keeps the buttons from making the line of figures taller', () => {
    expect(rule('.turn__line-actions')).toContain(
      'margin-block: calc((var(--leading-sm) - var(--size-control-md)) / 2);',
    );
  });

  it('keeps one set of controls per pointer', () => {
    expect(css).toMatch(
      /@media \(hover: hover\) \{[^@]*\.turn__line-more \{\s*display: none;/,
    );
    expect(css).toMatch(
      /@media \(hover: none\) \{[^@]*\.message-actions--line \{\s*display: none;/,
    );
  });
});
