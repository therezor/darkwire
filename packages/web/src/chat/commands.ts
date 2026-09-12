/**
 * The composer's slash commands.
 *
 * The third table of its kind — `packages/cli/src/commands.ts` and
 * `packages/channels/src/telegram/commands.ts` are the other two — and the third
 * rather than a shared one for the reason the Telegram file gives about
 * `resolveSeq`: the surfaces agree on a vocabulary, not on an implementation,
 * and a common core would have to be the union of three sets of constraints to
 * serve any of them. What is shared is the discipline, and there are two halves
 * to it.
 *
 * **Nothing here translates anything.** A command answers with a key and its
 * values; the caller renders it. That is what keeps this file free of React and
 * of `t`, so the whole table can be exercised against a plain object.
 *
 * **Nothing here reaches for a singleton.** Everything a command can touch is a
 * member of `CommandContext`, assembled once in `use-commands.ts`. The nine
 * verbs on it are close to one per command, which looks like ceremony until you
 * try to test `/branch` — at which point it is the difference between a fake and
 * a mocked module.
 *
 * Three commands differ from their terminal spelling on purpose, and each says
 * why at its own definition: `/new`, `/branch` and `/model`.
 */

import {
  ReasoningEffortSchema,
  type AgentSummary,
  type ExtensionCommand,
  type ModelInfo,
  type ReasoningEffort,
} from '@ghostwire/protocol';

import type { WebKey } from '@/i18n/keys.js';

/** Interpolation for the sentence a command answers with. */
type Values = Readonly<Record<string, string | number>>;

/**
 * What a command answers with, in words the caller renders.
 *
 * Two shapes, and the difference is where the words came from. A built-in
 * answers with a **key**, because this file translates nothing and its copy
 * ships in the locale bundle. An extension's command answers with **text**,
 * because its copy ships with the extension and the translation layer has never
 * seen it — the same rule a toolbox's `notes` follows. Neither can be
 * substituted for the other, so both are in the union rather than one being
 * squeezed into the other's shape.
 */
export type CommandOutcome =
  | { readonly kind: 'note'; readonly key: WebKey; readonly values?: Values }
  | { readonly kind: 'error'; readonly key: WebKey; readonly values?: Values }
  | { readonly kind: 'note'; readonly text: string }
  | { readonly kind: 'error'; readonly text: string };

/** Everything a command may reach. Assembled once, in `use-commands.ts`. */
export interface CommandContext {
  /** The conversation the socket is attached to, before anything is stored. */
  readonly sessionKey: string | undefined;
  /** Where a *new* conversation lands. */
  readonly workspaceId: string;
  /** Which agent a *new* conversation runs on. */
  readonly agentId: string | undefined;
  /** True between `turn.start` and `turn.end` — only `/stop` reads it. */
  readonly busy: boolean;
  /**
   * Whether this conversation has a stored row yet.
   *
   * Three commands need it, because `PATCH`, `DELETE` and `POST …/branch` all
   * 404 against a conversation nobody has spoken in. Refusing here says why;
   * letting the request go says "Not Found".
   */
  readonly stored: boolean;
  /** The last thing the user said, for `/branch`. */
  readonly lastUserSeq: number | undefined;
  /** Every configured agent, so `/agent x` can refuse an id that is not one. */
  readonly agents: readonly AgentSummary[];

  /**
   * Every reachable model, for the same refusal — and a request, not a field.
   *
   * The listing is only fetched when something wants it, and `/model gpt-4o`
   * typed straight past the completion list is exactly the case where nothing
   * has. A field would be empty there, and the command would refuse a model
   * that exists.
   */
  models(): Promise<readonly ModelInfo[]>;
  /** Mints a key and points the socket at it. Persists nothing. */
  newSession(): string;
  /** Takes the browser to a conversation. */
  openSession(key: string): void;
  rename(title: string): Promise<void>;
  clear(): Promise<void>;
  branch(seq: number): Promise<string>;
  stop(): void;
  chooseAgent(id: string): Promise<void>;
  /** The label of the agent this conversation runs on, for a note to name. */
  readonly agentLabel: string;
  /**
   * The whole model, not its id.
   *
   * A model and the endpoint that serves it are one setting: writing an agent's
   * `model` alone on a two-provider install leaves `provider` naming an
   * instance that has never heard of it. The agent editor has always saved the
   * pair for this reason, and `modelOptions` in `components/form/fields.ts` is
   * the same rule read the other way round.
   */
  setModel(model: ModelInfo): Promise<void>;
  /**
   * `null` clears, and clearing is not the same as `off`.
   *
   * Unset means the request carries no reasoning parameter at all and the
   * provider applies its own — the only thing that works against an endpoint
   * that rejects the field. `off` sends one asking for none. Both are offered
   * because on a model that thinks unless told otherwise they are different
   * turns. `temperature` is the same shape, with `0` as its own trap: it is a
   * value, not an absence.
   */
  setEffort(effort: ReasoningEffort | null): Promise<void>;
  setTemperature(temperature: number | null): Promise<void>;
  /**
   * The ids extensions currently contribute, fetched rather than compiled in.
   *
   * A list of names rather than a table of runnable things: this file has no
   * `api` and never will, so what it can do is *recognise* one and hand it
   * back. `use-commands.ts` supplies both halves, as it does for everything
   * else here.
   */
  readonly extensionCommands: readonly string[];
  runExtensionCommand(
    id: string,
    args: string,
  ): Promise<{ readonly message: string; readonly ok: boolean }>;
}

/** What the parser hands a command. */
export interface CommandInput {
  /** Whitespace-split arguments, without the command word. */
  readonly args: readonly string[];
  /** Everything after the command word, untouched. `/rename` wants this. */
  readonly tail: string;
  readonly ctx: CommandContext;
}

export interface WebCommand {
  /** Typed by the user, so never translated. */
  readonly name: string;
  /** Extra words the parser reads. `<a>` is required, `[a]` is not. */
  readonly usage?: string;
  /** Prose, so always a key. */
  readonly description: WebKey;
  /**
   * What completes this command's argument, if anything does.
   *
   * The browser's answer to Telegram's picker keyboard and the terminal's menu:
   * with no listing surface, a command that takes an id has to be able to offer
   * the ids. Absent means the argument is free text, or there is none.
   */
  readonly values?: (ctx: CommandValues) => readonly ValueSuggestion[];
  run(input: CommandInput): Promise<CommandOutcome> | CommandOutcome;
}

/** What the completion list knows. A subset of the context, and read-only. */
export interface CommandValues {
  readonly agents: readonly AgentSummary[];
  readonly models: readonly ModelInfo[];
  /**
   * The effort this session's agent sends, absent when it states none.
   *
   * Here so `/effort` can mark the row in force, which is the whole reason the
   * list is worth opening: the six levels are guessable and which one is
   * running is not. Already inherited — it comes off `AgentSummary` — so absent
   * means "this agent sends no reasoning parameter", not "unknown".
   */
  readonly effort: ReasoningEffort | undefined;
  /**
   * The two sentences `/effort`'s rows need, already prose.
   *
   * Supplied rather than written here, like every other string in this file:
   * nothing in this table translates anything.
   */
  readonly effortHints: { readonly default: string; readonly off: string };
}

export interface ValueSuggestion {
  /** What is typed after the command word. */
  readonly value: string;
  /** The line beside it, already prose. */
  readonly hint: string;
  /**
   * The row the list should open on.
   *
   * At most one, and only where "in force" means anything — `/agent` and
   * `/model` write what a turn already uses, so their lists open at the top as
   * they always have. It is the composer that acts on this; the table only
   * says which row it is.
   */
  readonly current?: boolean;
}

/**
 * The word for "send nothing and let the provider decide".
 *
 * The terminal spells it the same way; see `packages/cli/src/pickers/effort.ts`,
 * which argues why the word is available at all.
 *
 * Not translated, for the reason a command name is not: it is typed, so it is
 * what the parser matches on. A localised `default` is a word the parser does
 * not know.
 */
const DEFAULT_LEVEL = 'default';

const COMMANDS: readonly WebCommand[] = [
  {
    // **No title, unlike the terminal's `/new [title]`.** The browser mints a
    // key and sends `session.new`; no row exists until the first message lands,
    // so there is nothing to title yet. `POST /api/sessions` would create one
    // eagerly and is the thing `newSession` documents not doing — a sidebar
    // filling with conversations nobody had. `/rename` afterwards.
    name: 'new',
    description: 'chat.commands.new',
    run: ({ ctx }) => {
      ctx.openSession(ctx.newSession());
      return { kind: 'note', key: 'chat.commands.notes.started' };
    },
  },

  {
    name: 'clear',
    description: 'chat.commands.clear',
    run: async ({ ctx }) => {
      const refusal = requireStored(ctx);
      if (refusal !== undefined) return refusal;
      await ctx.clear();
      // No transcript work here. The server answers the clear with a
      // `session.reset` frame, and every tab — this one included — empties from
      // that rather than from the promise.
      return { kind: 'note', key: 'chat.commands.notes.cleared' };
    },
  },

  {
    name: 'rename',
    usage: '<title>',
    description: 'chat.commands.rename',
    run: async ({ tail, ctx }) => {
      if (tail === '') {
        return { kind: 'error', key: 'chat.commands.errors.usageRename' };
      }
      const refusal = requireStored(ctx);
      if (refusal !== undefined) return refusal;
      await ctx.rename(tail);
      return {
        kind: 'note',
        key: 'chat.commands.notes.renamed',
        values: { title: tail },
      };
    },
  },

  {
    name: 'stop',
    description: 'chat.commands.stop',
    run: ({ ctx }) => {
      if (!ctx.busy) {
        return { kind: 'error', key: 'chat.commands.errors.notRunning' };
      }
      ctx.stop();
      return { kind: 'note', key: 'chat.commands.notes.stopping' };
    },
  },

  {
    // **No ref, unlike the terminal's `/branch [ref]`.** There is no
    // `resolveSeq` in the browser and no reason to invent one: every message on
    // screen carries its own Branch action, so the only point a *typed* command
    // can usefully name is the last thing said. The seq goes to the route
    // unchanged — terminal parity, where `/branch` forks inclusively at the
    // resolved seq. The transcript's own "Branch from here" passes `seq - 1`
    // instead, because forking *before* a message is what lets you re-ask it.
    name: 'branch',
    description: 'chat.commands.branch',
    run: async ({ ctx }) => {
      const refusal = requireStored(ctx);
      if (refusal !== undefined) return refusal;
      const seq = ctx.lastUserSeq;
      if (seq === undefined) {
        return { kind: 'error', key: 'chat.commands.errors.nothingToBranch' };
      }
      ctx.openSession(await ctx.branch(seq));
      return {
        kind: 'note',
        key: 'chat.commands.notes.branched',
        values: { seq },
      };
    },
  },

  {
    name: 'agent',
    usage: '<id>',
    description: 'chat.commands.agent',
    values: (ctx) =>
      ctx.agents.map((agent) => ({ value: agent.id, hint: agent.label })),
    run: async ({ args, ctx }) => {
      const id = args[0];
      if (id === undefined) {
        return { kind: 'error', key: 'chat.commands.errors.usageAgent' };
      }
      if (!ctx.agents.some((agent) => agent.id === id)) {
        return {
          kind: 'error',
          key: 'chat.commands.errors.noAgent',
          values: { id },
        };
      }
      await ctx.chooseAgent(id);
      return {
        kind: 'note',
        key: 'chat.commands.notes.agentSet',
        values: { id },
      };
    },
  },

  {
    // **It moves the agent this conversation runs on, and it saves.**
    //
    // An agent is the only place a model lives, so the agent the conversation is
    // bound to is the one that moves. The rule about `agents.list.*` replacing
    // wholesale rather than merging lives in `agentSettingsPatch`;
    // `use-commands.ts` calls it. The terminal's `/model` does the same thing;
    // the bot's is process-scoped, see `packages/cli/src/telegram.ts`.
    name: 'model',
    usage: '<id>',
    description: 'chat.commands.model',
    values: (ctx) =>
      ctx.models.map((model) => ({
        value: model.id,
        hint: model.providerId,
      })),
    run: async ({ args, ctx }) => {
      const id = args[0];
      if (id === undefined) {
        return { kind: 'error', key: 'chat.commands.errors.usageModel' };
      }
      const models = await ctx.models();
      const chosen = models.find((model) => model.id === id);
      if (chosen === undefined) {
        return {
          kind: 'error',
          key: 'chat.commands.errors.noModel',
          values: { id },
        };
      }
      await ctx.setModel(chosen);
      return {
        kind: 'note',
        key: 'chat.commands.notes.modelSet',
        values: { id, agent: ctx.agentLabel },
      };
    },
  },

  {
    // `default` rather than an empty argument, because "send no reasoning
    // parameter" is a real answer and needs a way to be said — see
    // `CommandContext.setEffort`.
    //
    // The rows say which level is running and what unset would do, which is the
    // only reason to open a list of six words you could have typed. Ordered as
    // the ramp the levels are, never with the current one hoisted to the top:
    // scrambling `minimal → xhigh` to put a marker first costs more than the
    // marker is worth.
    name: 'effort',
    usage: '<level>',
    description: 'chat.commands.effort',
    values: (ctx) => [
      {
        value: DEFAULT_LEVEL,
        hint: ctx.effortHints.default,
        current: ctx.effort === undefined,
      },
      ...ReasoningEffortSchema.options.map((level) => ({
        value: level,
        hint: level === 'off' ? ctx.effortHints.off : '',
        current: ctx.effort === level,
      })),
    ],
    run: async ({ args, ctx }) => {
      const level = args[0];
      if (level === undefined) {
        return {
          kind: 'error',
          key: 'chat.commands.errors.usageEffort',
          values: { levels: ReasoningEffortSchema.options.join(', ') },
        };
      }
      if (level === DEFAULT_LEVEL) {
        await ctx.setEffort(null);
        return {
          kind: 'note',
          key: 'chat.commands.notes.effortDefault',
          values: { agent: ctx.agentLabel },
        };
      }
      const parsed = ReasoningEffortSchema.safeParse(level);
      if (!parsed.success) {
        return {
          kind: 'error',
          key: 'chat.commands.errors.usageEffort',
          values: { levels: ReasoningEffortSchema.options.join(', ') },
        };
      }
      await ctx.setEffort(parsed.data);
      return {
        kind: 'note',
        key: 'chat.commands.notes.effortSet',
        values: { level: parsed.data, agent: ctx.agentLabel },
      };
    },
  },

  {
    name: 'temperature',
    usage: '<0-2>',
    description: 'chat.commands.temperature',
    run: async ({ args, ctx }) => {
      const raw = args[0];
      if (raw === undefined) {
        return { kind: 'error', key: 'chat.commands.errors.usageTemperature' };
      }
      if (raw === DEFAULT_LEVEL) {
        await ctx.setTemperature(null);
        return {
          kind: 'note',
          key: 'chat.commands.notes.temperatureDefault',
          values: { agent: ctx.agentLabel },
        };
      }
      // `Number` rather than `parseFloat`, which reads `0.5abc` as `0.5`. A
      // temperature that was half a typo is a refusal, not a guess.
      const value = Number(raw);
      if (!Number.isFinite(value) || value < 0 || value > 2) {
        return { kind: 'error', key: 'chat.commands.errors.usageTemperature' };
      }
      await ctx.setTemperature(value);
      return {
        kind: 'note',
        key: 'chat.commands.notes.temperatureSet',
        values: { value, agent: ctx.agentLabel },
      };
    },
  },
];

const BY_NAME = new Map(COMMANDS.map((command) => [command.name, command]));

/**
 * Every command, for the completion list.
 *
 * One table, two readers — the same discipline the other two surfaces hold, and
 * for the same reason: a second list beside this one eventually offers a
 * command that does not exist.
 */
export function commandRows(): readonly WebCommand[] {
  return COMMANDS;
}

/**
 * The rows above, plus whatever extensions contribute right now.
 *
 * A function of the fetched list rather than a constant, because the answer
 * changes while the page is open: approving an extension adds a command to a
 * composer nobody reloaded. The `description` is the extension's own text and
 * so is not a key — see `CommandOutcome`.
 */
export function commandRowsFor(
  extensions: readonly ExtensionCommand[],
): readonly CommandRow[] {
  return [
    ...COMMANDS.map((command) => ({
      name: command.name,
      ...(command.usage === undefined ? {} : { usage: command.usage }),
      description: command.description,
    })),
    ...extensions.map((command) => ({
      name: command.id,
      ...(command.argsHint === '' ? {} : { usage: command.argsHint }),
      text: command.description,
    })),
  ];
}

/**
 * One line of the `/` list.
 *
 * `description` is a key and `text` is words, for the reason `CommandOutcome`
 * carries both: one side's copy is in the locale bundle and the other's ships
 * with an extension.
 */
export interface CommandRow {
  readonly name: string;
  readonly usage?: string | undefined;
  readonly description?: WebKey | undefined;
  readonly text?: string | undefined;
}

export function findCommand(name: string): WebCommand | undefined {
  return BY_NAME.get(name);
}

export interface ParsedCommand {
  readonly name: string;
  readonly args: readonly string[];
  readonly tail: string;
}

/**
 * Reads a command out of what was typed, or decides it is prose.
 *
 * Telegram gets this for free: it matches on a `bot_command` entity at offset 0
 * rather than on a character, so a message that merely *mentions* `/clear` is
 * not one. A browser has no entities, so the rule has to be written down, and it
 * is load-bearing — `/usr/bin/env is on the path` must reach the model as the
 * sentence it is.
 *
 * A command is a leading slash followed by a slug — a lowercase letter, then
 * lowercase letters, digits and hyphens. That one shape is what makes every
 * path prose: `/usr/bin/env` and `/etc/hosts` carry a second slash,
 * `/Users/rezor` carries a capital, and none of them is ever a command however
 * this table grows.
 *
 * The hyphens and digits are for extensions. Every id an extension contributes
 * is `<extensionId>` or `<extensionId>-<suffix>` — see
 * `packages/extension-host/src/registration.ts` — so a table that could not
 * spell `slack-post` could not offer it. It widens what counts as a *typo*
 * rather than what counts as a command: `/foo-bar` matching nothing is still
 * reported as a mistake instead of being sent to the model, which is the answer
 * all three surfaces already give.
 *
 * A name that fits the shape and matches nothing is a typo rather than prose,
 * and is reported as one — the same answer both other surfaces give, because a
 * surface that silently sends a mistyped `/rname` to the model looks broken.
 */
export function parseCommand(text: string): ParsedCommand | undefined {
  const trimmed = text.trim();
  const [word = '', ...args] = trimmed.split(/\s+/u);
  if (!/^\/[a-z][a-z0-9-]*$/u.test(word)) return undefined;

  return {
    name: word.slice(1),
    args,
    tail: trimmed.slice(word.length).trim(),
  };
}

/**
 * Runs one command.
 *
 * Never throws for anything a person typed: a guard answers with
 * `{kind: 'error'}` and the box comes back, which is the rule
 * `runSlashCommand` and Telegram's `runCommand` both hold. A rejected request
 * is the caller's to report — it has the message, and this file has no words.
 *
 * Whether what was typed *is* a command is `parseCommand`'s question, and it is
 * answered synchronously. This one is only about what happens next, so the
 * message box never waits on it.
 */
export async function runCommand(
  parsed: ParsedCommand,
  ctx: CommandContext,
): Promise<CommandOutcome> {
  const command = BY_NAME.get(parsed.name);
  if (command === undefined) {
    // An extension's, if anything. The table above stays hand-written for the
    // reason its header gives — these are the commands this surface implements
    // — and an extension's is one it merely forwards, so it reaches the server
    // rather than this file.
    if (ctx.extensionCommands.includes(parsed.name)) {
      const answer = await ctx.runExtensionCommand(parsed.name, parsed.tail);
      return answer.ok
        ? { kind: 'note', text: answer.message }
        : { kind: 'error', text: answer.message };
    }
    return {
      kind: 'error',
      key: 'chat.commands.errors.unknown',
      values: { name: parsed.name },
    };
  }
  return await command.run({ args: parsed.args, tail: parsed.tail, ctx });
}

/**
 * The refusal three commands share.
 *
 * `PATCH`, `DELETE` and `POST …/branch` all answer 404 for a conversation with
 * no stored row, and "Not Found" is a worse sentence than the one this returns.
 */
function requireStored(ctx: CommandContext): CommandOutcome | undefined {
  if (ctx.sessionKey !== undefined && ctx.stored) return undefined;
  return { kind: 'error', key: 'chat.commands.errors.nothingYet' };
}
