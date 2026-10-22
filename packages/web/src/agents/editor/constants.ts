/**
 * Names the agent editor's state and its sections both read.
 */

/**
 * The tool pinned to the top of the list.
 *
 * A name rather than a risk band: `exec` is the specific tool that runs a
 * program, and pinning the whole `exec` band would float every MCP tool that
 * shares it, which is the second list, in its own group, where source order is
 * the useful order.
 */
export const EXEC_TOOL = 'exec';

/** The door to the tools lazy discovery hides. Never itself pinned. */
export const TOOL_SEARCH = 'tool_search';

/**
 * The two tools that are the write half of a whole feature.
 *
 * Grouped away from the action tools because denying one of these does more than
 * refuse a call: `runtime.ts` gates the skills catalogue and the memory index on
 * exactly these permissions, so an operator flipping one here also changes what
 * every prompt on the workspace carries. That is a different size of decision
 * from "may this agent write a file", and a row in the same alphabetical list
 * does not say so.
 *
 * Kept as names rather than derived from a risk band: both are `write`, and so
 * is `write`, which is squarely an action tool.
 */
export const FEATURE_TOOLS: ReadonlySet<string> = new Set(['memory', 'skill']);

/**
 * The prefix `flattenToolName` puts on every tool an MCP server contributes.
 *
 * A fallback for the rows the registry cannot describe. `ToolDefinition.source`
 * is the answer whenever the tool is registered, and it is the one used first.
 * But `toolNames` deliberately includes tools this agent has an opinion about
 * whose server is down, and those have no definition at all. Without this they
 * would sit in the action list wearing a "not installed" badge, which is the
 * mixing this grouping exists to undo, on the rows most likely to be looked at.
 *
 * A prefix test and nothing more. `mcp_{server}_{tool}` cannot be parsed back
 * (`sanitise` collapses characters and a long name is truncated onto a digest),
 * so this asks only whether a name came from a server, never which one.
 */
export const MCP_TOOL_PREFIX = 'mcp_';

/**
 * The dropdown's value for the host.
 *
 * **The host is an environment, not the absence of one.** It is where commands
 * run when no definition is named, and the prompt says so in its own words. The
 * config spells it as an empty name because there is no definition file to
 * point at, but nothing above that layer should read it as "none chosen".
 *
 * A `SelectItem` may not carry an empty value. Radix reserves it for "nothing
 * chosen", which is exactly the meaning to avoid here, so this option needs a
 * value of its own. It begins with `-`, which cannot collide with an installed
 * environment name.
 */
export const HOST_ENVIRONMENT = '-host-';
