/**
 * One name for one thing.
 *
 * The product called a single object three things at once. The sidebar heading
 * said "Latest sessions"; the list directly under it was labelled
 * "Conversations"; the workspace card two clicks away counted "12 chats".
 * Nobody chose that. It accumulated one string at a time, because no two of the
 * three are ever read side by side while you are writing the third.
 *
 * "Session" won, and it won because everything underneath already said it:
 * `SessionStore`, the `sessions` table, `/api/sessions`, `sessionKey`, and the
 * terminal's `/sessions` and `--session`. A word that reaches from the button
 * to the column name is one nobody has to translate while reading a bug report.
 *
 * So "conversation" is gone, and "chat" is gone as a noun for one of these —
 * which is the harder half, because "chat" is a real word here in two other
 * senses: `ghostai chat` is a command, and a Telegram chat is a destination. The
 * allowlist below is where those live, with their reasons attached.
 *
 * **"Session" is not banned anywhere.** That is the whole point of the choice,
 * and it is why this file has no boundary to maintain: there is no rule about
 * which "session" is the auth one, because the auth copy stopped using the word
 * (`server.sessionInvalid` reads "You have been signed out", `account.desc`
 * says "every other device"). A gate that had to tell those apart would be a
 * regex nobody could review.
 *
 * The bundles rather than the source, because the bundles *are* the copy —
 * `packages/web/test/i18n/untranslated.test.ts` is what proves no English sits
 * outside them. Key paths are checked as well as values, deliberately: a key is
 * what the next person greps for, so a surviving `openConversation` teaches the
 * old word back even while its value reads correctly. That also makes this the
 * only thing watching for extractor residue — `keepRemoved: true` means a
 * renamed key leaves its old entry behind, so a stale block survives
 * `pnpm i18n:extract` and shows up nowhere but here.
 */

import { describe, expect, it } from 'vitest';

import { EN } from '#src/resources.js';

/** Never a name for one of these, in any sense, anywhere. */
const CONVERSATION = /\bconversations?\b/iu;

/**
 * Banned as a noun for one of these, allowed for the two other things it
 * genuinely names — see `ALLOWED`.
 */
const CHAT = /\bchats?\b/iu;

/**
 * Keys that may say "chat", each with the reason it is not a session.
 *
 * Keyed by dotted path alone rather than by path-plus-word. A key that earned
 * an exemption for one banned word has not earned one for the other, and a key
 * holding both is a key that needs rewriting rather than a narrower exemption.
 */
const ALLOWED: ReadonlyArray<{ readonly key: string; readonly why: string }> = [
  {
    key: 'web.settings.appearance.languageDesc',
    why: '`ghostai chat` is the name of a command, quoted here as the thing the language setting also applies to. The subcommand is not being renamed, and a sentence that named it anything else would name a command that does not exist.',
  },
];

const ALLOWED_KEYS = new Set(ALLOWED.map((entry) => entry.key));

/**
 * Namespaces whose *paths* legitimately contain "chat".
 *
 * `web.chat.*` is the transcript surface — the composer, the approval card, the
 * message list — and `cli.chat.*` belongs to the `ghostai chat` subcommand.
 * Neither names a session, and neither is being renamed, so the path rule skips
 * them. Their values are still checked; only the key path is exempt.
 */
const CHAT_NAMESPACES = ['web.chat.', 'cli.chat.'];

interface Leaf {
  readonly path: string;
  readonly value: string;
}

/**
 * Every leaf string in a bundle, under the dotted path `t()` would be handed.
 *
 * Local rather than shared: `keys.ts` is types-only, and a flattener exported
 * from this package would invite a second caller and then drift from the one
 * rule it exists to serve.
 */
function leaves(node: unknown, prefix: string): Leaf[] {
  if (typeof node === 'string') return [{ path: prefix, value: node }];
  if (typeof node !== 'object' || node === null) return [];

  return Object.entries(node as Record<string, unknown>).flatMap(
    ([key, child]) => leaves(child, prefix === '' ? key : `${prefix}.${key}`),
  );
}

const LEAVES = Object.entries(EN).flatMap(([namespace, bundle]) =>
  leaves(bundle, namespace),
);

/**
 * `openConversation` and `forThisChat` hold a banned word with no word boundary
 * in front of it, so a path is split at its humps before the same rule is
 * applied. Values are prose and already have their spaces.
 */
function spaced(path: string): string {
  return path.replace(/([a-z0-9])([A-Z])/gu, '$1 $2');
}

function offence(leaf: Leaf): string | undefined {
  const inNamespace = CHAT_NAMESPACES.some((prefix) =>
    leaf.path.startsWith(prefix),
  );
  const path = spaced(leaf.path);

  if (CONVERSATION.test(path)) return 'key says "conversation"';
  if (CONVERSATION.test(leaf.value)) return 'says "conversation"';
  if (ALLOWED_KEYS.has(leaf.path)) return undefined;
  if (!inNamespace && CHAT.test(path)) return 'key says "chat"';
  if (CHAT.test(leaf.value)) return 'says "chat"';
  return undefined;
}

describe('the product vocabulary', () => {
  it('calls a session a session, in every bundle and in every key', () => {
    const offenders = LEAVES.flatMap((leaf) => {
      const why = offence(leaf);
      return why === undefined ? [] : [`${leaf.path} ${why}: ${leaf.value}`];
    });

    expect(offenders).toEqual([]);
  });

  it('keeps the allowlist honest', () => {
    // Both halves matter. An entry naming a key that no longer exists is an
    // exemption nobody is watching, and the next person reads a stale line as
    // permission. An entry whose string was since reworded should lose the
    // exemption rather than sit there implying the word is still needed —
    // which is how a list of two becomes a list of twenty.
    for (const { key } of ALLOWED) {
      const leaf = LEAVES.find((entry) => entry.path === key);

      expect(leaf, `${key} is not a key in any bundle`).toBeDefined();
      expect(
        leaf !== undefined &&
          (CHAT.test(leaf.value) || CONVERSATION.test(leaf.value)),
        `${key} no longer says "chat" or "conversation" — drop it from the allowlist`,
      ).toBe(true);
    }
  });
});
