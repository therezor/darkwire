/**
 * The `/` autocomplete, over the command table beside it.
 *
 * `parseCommand` in `commands.ts` answers "is this finished line a command",
 * which is the question the send path asks. The box needs the other one — "is
 * the caret inside a command that is still being typed, and what would complete
 * it" — which no parser of finished text can answer. So this file owns the
 * partial case and defers to the table for everything else: the rows come from
 * `commandRows()`, so a command added there appears here without an edit.
 *
 * The arrow points one way, exactly as it does in the terminal between
 * `cli/src/commands.ts` and `cli/src/pickers/palette.ts`: this file imports the
 * table, and the table does not import this file.
 *
 * **It completes two levels, and the second one is the point.** A command
 * reports in a toast, which has room for a sentence and not for a list, so
 * `/agent` and `/model` have nowhere to print their ids — they have to be
 * offerable while the argument is being typed instead. This is the browser's
 * answer to Telegram's picker keyboard and the terminal's arrow-key menu.
 */

import type { TFunction } from 'i18next';

import type {
  AgentSummary,
  ModelInfo,
  ReasoningEffort,
} from '@ghostwire/protocol';

import { commandRows, findCommand } from './commands.js';

/** A command being typed at the caret. */
export interface CommandQuery {
  /** Index in the source where an accepted suggestion starts replacing. */
  readonly start: number;
  /** The caret. Everything between `start` and here is what has been typed. */
  readonly end: number;
  /** The command whose argument is being typed. Absent while naming one. */
  readonly name: string | undefined;
  /** What has been typed for whichever of the two is being completed. */
  readonly query: string;
}

export interface CommandSuggestion {
  /** What replaces the query. Carries the trailing space that closes it. */
  readonly insert: string;
  readonly label: string;
  readonly hint: string;
  /**
   * The row the list should open on, when one of them is already in force.
   *
   * Never set for a command row — there is no "current" command — and set by at
   * most one value row. The composer reads it; nothing else does.
   */
  readonly current?: boolean;
}

/** What the value rows are drawn from, and the words for the command rows. */
export interface CompleteDeps {
  readonly agents: readonly AgentSummary[];
  readonly models: readonly ModelInfo[];
  /** What this session's agent sends now, so `/effort` can mark its row. */
  readonly effort: ReasoningEffort | undefined;
  readonly t: TFunction;
}

/**
 * The command under the caret, if there is one.
 *
 * A command starts at column 0 — that is the whole of the grammar, and it is
 * what makes this so much shorter than the mention parser it replaced. There is
 * no word-boundary search backwards because there is no word to find: either the
 * message opens with a slash or it is prose.
 *
 * Two shapes are recognised, and nothing else is. The caret is inside the
 * command word, or it is inside the *first* argument. Past that there is nothing
 * left to offer: no command here takes two.
 */
export function commandAtCaret(
  text: string,
  caret: number,
): CommandQuery | undefined {
  if (!text.startsWith('/')) return undefined;
  const before = text.slice(0, caret);

  const naming = /^\/([a-z]*)$/u.exec(before);
  if (naming !== null) {
    return { start: 0, end: caret, name: undefined, query: naming[1] ?? '' };
  }

  const arguing = /^\/([a-z]+)\s+(\S*)$/u.exec(before);
  if (arguing === null) return undefined;
  const [, name = '', typed = ''] = arguing;
  return { start: caret - typed.length, end: caret, name, query: typed };
}

/**
 * What could complete this query.
 *
 * An empty list is what the caller renders as "no popover", not as "nothing
 * found" — a command that takes free text (`/rename`) or none at all reaches the
 * argument branch and correctly offers nothing, and a menu that said "no
 * results" there would read as broken rather than as absent.
 *
 * A value list that is still being fetched is an empty array, and the popover
 * simply does not open until it arrives. That is better than flashing "none" at
 * an install that has plenty.
 */
export function commandSuggestions(
  query: CommandQuery,
  deps: CompleteDeps,
): readonly CommandSuggestion[] {
  if (query.name === undefined) {
    const rows = commandRows().filter((command) =>
      command.name.startsWith(query.query),
    );

    // Nothing left to complete, so the list gets out of the way and Enter
    // *sends* rather than inserting a space nobody asked for. Without this
    // every command costs two Return presses, which is not what `/stop` means
    // anywhere else.
    //
    // Only when the name is typed in full, is the only match, and takes no
    // argument. A command that needs one keeps its row so accepting it leaves
    // the cursor after the space — the rule `cli/src/pickers/palette.ts` states
    // as "a row that needs an argument is typed, not run", and what makes
    // `/agent` open the id list instead of earning a usage error.
    const [only] = rows;
    if (
      rows.length === 1 &&
      only?.name === query.query &&
      only.usage === undefined
    ) {
      return [];
    }

    return rows.map((command) => ({
      // The trailing space closes the command, so the argument list — or the
      // next word — starts cleanly.
      insert: `/${command.name} `,
      label:
        command.usage === undefined
          ? `/${command.name}`
          : `/${command.name} ${command.usage}`,
      hint: deps.t(command.description),
    }));
  }

  const values = findCommand(query.name)?.values;
  if (values === undefined) return [];

  const typed = query.query.toLowerCase();
  return values({
    agents: deps.agents,
    models: deps.models,
    effort: deps.effort,
    // Translated here rather than in the table, which holds no prose, and
    // rather than in the composer, which would then be picking sentences for a
    // command it knows nothing else about. This file already resolves
    // `command.description` the same way.
    effortHints: {
      default: deps.t('chat.commands.efforts.default'),
      off: deps.t('chat.commands.efforts.off'),
    },
  })
    .filter((one) => one.value.toLowerCase().startsWith(typed))
    .map((one) => ({
      insert: `${one.value} `,
      label: one.value,
      hint: one.hint,
      ...(one.current === true ? { current: true } : {}),
    }));
}

/** The text and caret after accepting a suggestion. */
export function applyCommand(
  text: string,
  query: CommandQuery,
  suggestion: CommandSuggestion,
): { readonly text: string; readonly caret: number } {
  const next =
    text.slice(0, query.start) + suggestion.insert + text.slice(query.end);
  return { text: next, caret: query.start + suggestion.insert.length };
}
