/**
 * The `/` completion, at the level the popover cannot reach.
 *
 * Two levels are under test and the second one is the reason the file exists:
 * with no listing surface in the browser, `/agent ` completing to the agent ids
 * *is* the picker. A `commandAtCaret` that stopped at the command word would
 * leave `/agent` with no way to answer at all.
 */

import type { TFunction } from 'i18next';
import { describe, expect, it } from 'vitest';

import type { AgentSummary, ModelInfo } from '@ghostwire/protocol';

import {
  applyCommand,
  commandAtCaret,
  commandSuggestions,
  type CompleteDeps,
} from '@/chat/command-complete.js';

const agents: readonly AgentSummary[] = [
  { id: 'default', label: 'Default', model: 'test-model', provider: 'ollama' },
  {
    id: 'researcher',
    label: 'Researcher',
    model: 'pinned',
    provider: 'lmstudio',
  },
];

const models: readonly ModelInfo[] = [
  { id: 'test-model', providerId: 'ollama' },
];

/**
 * The key back, unchanged.
 *
 * What is under test is *which* key each row carries, not what English it
 * renders as — and an identity `t` says that in the assertion rather than
 * hiding it behind a sentence that will be reworded.
 */
const t = ((key: string): string => key) as unknown as TFunction;
const deps: CompleteDeps = {
  agents,
  models,
  effort: undefined,
  t,
};

describe('the command under the caret', () => {
  it('opens on a bare slash', () => {
    expect(commandAtCaret('/', 1)).toEqual({
      start: 0,
      end: 1,
      name: undefined,
      query: '',
    });
  });

  it('narrows as the name is typed', () => {
    expect(commandAtCaret('/re', 3)?.query).toBe('re');
  });

  it('is nothing at all in prose', () => {
    expect(commandAtCaret('hello /clear', 12)).toBeUndefined();
  });

  it('moves to the argument once the word is closed', () => {
    // The replacement starts *after* the space, so accepting an id does not
    // swallow the command that takes it.
    expect(commandAtCaret('/agent de', 9)).toEqual({
      start: 7,
      end: 9,
      name: 'agent',
      query: 'de',
    });
  });

  it('opens the argument list the moment the space is typed', () => {
    expect(commandAtCaret('/agent ', 7)).toEqual({
      start: 7,
      end: 7,
      name: 'agent',
      query: '',
    });
  });

  it('stops after the first argument, because nothing takes two', () => {
    expect(commandAtCaret('/agent default more', 19)).toBeUndefined();
  });

  it('answers for the caret rather than for the end of the line', () => {
    // The caret sits inside the word, so what is offered is what is behind it.
    expect(commandAtCaret('/rename', 3)?.query).toBe('re');
  });
});

describe('what completes it', () => {
  it('lists every command on a bare slash', () => {
    const rows = commandSuggestions(commandAtCaret('/', 1)!, deps);
    expect(rows.map((row) => row.label)).toEqual([
      '/new',
      '/clear',
      '/rename <title>',
      '/stop',
      '/branch',
      '/agent <id>',
      '/model <id>',
      '/effort <level>',
      '/temperature <0-2>',
    ]);
  });

  it('filters by prefix, and inserts a trailing space', () => {
    const rows = commandSuggestions(commandAtCaret('/re', 3)!, deps);
    expect(rows).toEqual([
      {
        insert: '/rename ',
        label: '/rename <title>',
        hint: 'chat.commands.rename',
      },
    ]);
  });

  it('offers the agents once the command is closed', () => {
    const rows = commandSuggestions(commandAtCaret('/agent r', 8)!, deps);
    expect(rows).toEqual([
      { insert: 'researcher ', label: 'researcher', hint: 'Researcher' },
    ]);
  });

  it('offers nothing for a command whose argument is free text', () => {
    // Not "no results" — a menu saying that for `/rename` would read as broken
    // where an absent one reads as absent.
    expect(commandSuggestions(commandAtCaret('/rename x', 9)!, deps)).toEqual(
      [],
    );
  });

  it('closes once a command that needs nothing is typed in full', () => {
    // What makes `/stop⏎` one keypress rather than two. There is nothing left
    // to complete, so the list gets out of the way and Enter submits.
    expect(commandSuggestions(commandAtCaret('/stop', 5)!, deps)).toEqual([]);
    expect(commandSuggestions(commandAtCaret('/new', 4)!, deps)).toEqual([]);
  });

  it('keeps offering a command that still needs an argument', () => {
    // The rule the terminal's palette states as "a row that needs an argument
    // is typed, not run": Enter here leaves `/rename ` with the cursor after
    // it, rather than earning a usage error.
    expect(
      commandSuggestions(commandAtCaret('/rename', 7)!, deps),
    ).toHaveLength(1);
    // And for `/agent` that is what opens the id list.
    expect(commandSuggestions(commandAtCaret('/agent', 6)!, deps)).toHaveLength(
      1,
    );
  });

  it('offers nothing while the list it needs is still in flight', () => {
    const rows = commandSuggestions(commandAtCaret('/model ', 7)!, {
      ...deps,
      models: [],
    });
    expect(rows).toEqual([]);
  });
});

describe('accepting one', () => {
  it('replaces the query and leaves the caret after it', () => {
    const query = commandAtCaret('/re', 3);
    expect(query).toBeDefined();
    const [suggestion] = commandSuggestions(query!, deps);
    expect(suggestion).toBeDefined();
    expect(applyCommand('/re', query!, suggestion!)).toEqual({
      text: '/rename ',
      caret: 8,
    });
  });

  it('keeps whatever was to the right of the caret', () => {
    const query = commandAtCaret('/ag', 3);
    expect(
      applyCommand('/agX', query!, {
        insert: '/agent ',
        label: '/agent <id>',
        hint: '',
      }),
    ).toEqual({ text: '/agent X', caret: 7 });
  });
});
