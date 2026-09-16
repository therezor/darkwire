/**
 * What may name a directory DarkWire creates from user input.
 *
 * Two things are named this way — a workspace and an agent — and both arrive
 * over HTTP. A workspace id becomes a path; an agent id no longer does, but it
 * keeps the identical rules, because the reasons that made them identical have
 * not changed and two sets that agree today are two sets that drift apart in
 * exactly the case nobody tested.
 *
 * The rules, and why each is a rule rather than a preference:
 *
 *  - **One segment, `[a-z0-9-]`, no leading or trailing hyphen, 1–40 chars.**
 *    `..`, `/`, `\`, `:`, NUL and a leading `~` are all unrepresentable, so a
 *    crafted id cannot become a path outside the tree it belongs to. The jail
 *    would catch it anyway; this catches it a layer earlier, where the error
 *    can say something useful.
 *  - **Lowercase only, and that is a security rule.** APFS and NTFS fold case,
 *    so `Work` and `work` would be two rows sharing one directory — two things
 *    that believe they are isolated and are not.
 *  - **The Windows device names are reserved**, because `mkdir con` fails on
 *    exactly one platform, and something that cannot be created on Windows is a
 *    bug report from a user who did nothing wrong.
 *
 * What is *not* here is which ids a particular kind reserves beyond those, or
 * what a name with nothing usable in it falls back to. Those differ between
 * workspaces and agents, so callers pass them in.
 *
 * It lives in `@darkwire/protocol` rather than in `darkwire-core` because both
 * sides need it: the server turns an id into a path, and the browser *mints*
 * one when an operator creates an agent. Two implementations of a rule whose
 * whole job is that two things cannot collide is not a rule.
 */

/** 1–40 chars, lowercase alphanumerics and hyphens, no leading or trailing hyphen. */
export const SLUG_ID_PATTERN: RegExp = /^[a-z0-9](?:[a-z0-9-]{0,38}[a-z0-9])?$/;

export const MAX_SLUG_ID_LENGTH = 40;

/** Reserved on Windows whatever the id names. */
export const RESERVED_DEVICE_NAMES: ReadonlySet<string> = new Set([
  'con',
  'prn',
  'aux',
  'nul',
  ...Array.from({ length: 9 }, (unused, index) => `com${String(index + 1)}`),
  ...Array.from({ length: 9 }, (unused, index) => `lpt${String(index + 1)}`),
]);

/** Whether a string may be resolved to a directory name. */
export function isSlugId(value: string): boolean {
  return SLUG_ID_PATTERN.test(value);
}

/**
 * A display name reduced to a legal id.
 *
 * Lossy on purpose — the name is stored separately and is what the UI shows, so
 * this only has to produce something legal, stable and recognisable. A name
 * with nothing usable in it falls back rather than failing: the caller then
 * disambiguates against the rows that already exist.
 */
export function slugify(
  name: string,
  options: {
    readonly reserved: ReadonlySet<string>;
    readonly fallback: string;
  },
): string {
  const slug = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, MAX_SLUG_ID_LENGTH)
    .replace(/-+$/, '');

  return slug === '' || options.reserved.has(slug) ? options.fallback : slug;
}

// Workspaces

/**
 * What may name a workspace.
 *
 * A workspace id is also a directory name — `<root>/workspace/<id>` — and it
 * arrives over HTTP, so this is the only thing standing between a request body
 * and a path. The character rules and their rationale are at the top of this
 * file.
 *
 * What is specific to a workspace: **`default` is reserved**, because it names
 * the parent of every other workspace rather than a folder beside them.
 *
 * These moved here from `darkwire-core` for the reason stated at the top of
 * this file, which now applies to both kinds: the browser mints one. The create
 * form asks for the folder as its own field, so it has to be able to say —
 * before the request — that `Client Acme` proposes `client-acme` and that `con`
 * is not available.
 */

/** The workspace every install has, and the one that cannot be deleted. */
export const DEFAULT_WORKSPACE_ID = 'default';

/** 1–40 chars, lowercase alphanumerics and hyphens, no leading or trailing hyphen. */
export const WORKSPACE_ID_PATTERN: RegExp = SLUG_ID_PATTERN;

/** Reserved as *names to create*. `default` is still a legal id to resolve. */
export const RESERVED_WORKSPACE_IDS: ReadonlySet<string> = new Set([
  DEFAULT_WORKSPACE_ID,
  ...RESERVED_DEVICE_NAMES,
]);

/** Whether a string may be resolved to a workspace directory. */
export function isWorkspaceId(value: string): boolean {
  return isSlugId(value);
}

/** A display name reduced to a legal workspace id. See `slugify`. */
export function deriveWorkspaceId(name: string): string {
  return slugify(name, {
    reserved: RESERVED_WORKSPACE_IDS,
    fallback: 'workspace',
  });
}

// Agents

/**
 * What may name an agent.
 *
 * An agent id is a key in `agents.list`, a column in the session table and a
 * segment of a tool name, and it arrives over HTTP on a session frame — so it
 * gets the same treatment a workspace id gets even though, unlike a workspace
 * id, it never becomes a directory of its own. The character rules and their
 * rationale are at the top of this file.
 *
 * What is specific to an agent: **`default` is reserved**, because it names the
 * agent an install runs as before anyone has defined one — the settings under
 * the entry the schema prefaults into `agents.list`. It is a legal id
 * to *resolve*, and `agents.list.default` may be written to customise it; what
 * it is not is a name the UI lets an operator mint a second agent under.
 */

/** The agent every install has: `agents.list.default`, always present. */
export const DEFAULT_AGENT_ID = 'default';

/** 1–40 chars, lowercase alphanumerics and hyphens, no leading or trailing hyphen. */
export const AGENT_ID_PATTERN: RegExp = SLUG_ID_PATTERN;

/** Reserved as *names to create*. `default` is still a legal id to resolve. */
export const RESERVED_AGENT_IDS: ReadonlySet<string> = new Set([
  DEFAULT_AGENT_ID,
  ...RESERVED_DEVICE_NAMES,
]);

/** Whether a string may be resolved to an agent directory. */
export function isAgentId(value: string): boolean {
  return isSlugId(value);
}

/** A display label reduced to a legal agent id. See `slugify`. */
export function deriveAgentId(label: string): string {
  return slugify(label, { reserved: RESERVED_AGENT_IDS, fallback: 'agent' });
}

/** The prefix that keeps a subagent's tool name out of every other namespace. */
const SUBAGENT_TOOL_PREFIX = 'ask_';

/**
 * The tool name a model calls to hand work to a subagent.
 *
 * Derived rather than configured, so an operator cannot name two subagents the
 * same thing and cannot shadow a built-in: an agent id is 1–40 lowercase
 * alphanumerics and hyphens, so the prefixed form is 5–45 characters of
 * `[a-z0-9_]` — inside `TOOL_NAME_PATTERN`'s 64, and never equal to a built-in
 * name, none of which start with the prefix. The hyphens become underscores
 * because a leading `ask_` already reads as a prefix and `ask_code-review` reads
 * as two.
 *
 * It lives beside `deriveAgentId` because the browser needs it too: the editor
 * shows the operator the name their agent will be called by.
 */
export function subagentToolName(agentId: string): string {
  return `${SUBAGENT_TOOL_PREFIX}${agentId.replaceAll('-', '_')}`;
}

// Extensions

/**
 * What may name an extension.
 *
 * The same slug rules the other two get, and for a stronger version of the same
 * reason: an extension id names a directory under `<root>/extensions`, and it
 * is also the prefix every id that extension contributes must carry — a
 * channel, a provider, a command and, through the tool namespacer, a tool. One
 * character class for all four is what makes that rule checkable in one place.
 *
 * Nothing is reserved. There is no built-in extension for a name to collide
 * with, and shadowing between two installed extensions is a `conflict` the host
 * reports on the offending row rather than a name this pattern can prevent.
 */
export const EXTENSION_ID_PATTERN: RegExp = SLUG_ID_PATTERN;

/** Whether a string may be resolved to an extension directory. */
export function isExtensionId(value: string): boolean {
  return isSlugId(value);
}
