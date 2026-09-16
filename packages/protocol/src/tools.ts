/**
 * Tool wire shapes and the permission model.
 *
 * **A permission is per tool, and it is the only thing that decides.** An agent
 * carries a `name -> allow | ask | deny` map, and a name the map does not
 * mention is not enabled at all — so "which tools does this agent have" and
 * "what happens when it calls one" are one question with one answer, rather
 * than two mechanisms that can disagree.
 *
 * `risk` survives as **metadata**. It is declared by the tool, it rides on the
 * `tool.call` and `tool.approvalRequest` events so a card can badge itself, and
 * it seeds the permission a newly created agent starts a tool at. It decides
 * nothing at call time: a band-to-policy table as the gate would make `search`
 * and a port scanner in the same container one setting, and would leave an
 * agent's tool list a separate allow/deny pair that could admit a tool the
 * table then refused.
 */

import { z } from 'zod';

/**
 * What a tool can do, worst case. Bands rather than a boolean because the
 * useful default differs per band: reads are fine unattended, writes are
 * usually fine inside the workspace jail, exec and network are the two that a
 * self-hosted agent operator actually wants to see before they happen.
 *
 * Advisory since permissions became per tool — see the module header.
 */
export const ToolRiskSchema = z.enum(['safe', 'write', 'exec', 'network']);
export type ToolRisk = z.infer<typeof ToolRiskSchema>;

/**
 * What an agent may do with one tool.
 *
 * `deny` and *absent* mean the same thing to the runtime — the tool is not in
 * the definitions the model is sent. Both spellings exist because a UI needs
 * somewhere to put the off switch: an operator switching a tool off writes
 * `deny`, and that survives a round trip, where deleting the key would make the
 * row disappear from the editor entirely.
 */
export const ToolPermissionSchema = z.enum(['allow', 'ask', 'deny']);
export type ToolPermission = z.infer<typeof ToolPermissionSchema>;

/**
 * Tool name → permission.
 *
 * A record rather than a list of pairs so a patch that mentions one tool is one
 * key, and so the whole map replaces cleanly — removing a tool has to be
 * expressible, and a deep merge of two lists cannot express it.
 */
export const ToolPermissionsSchema = z.record(z.string(), ToolPermissionSchema);
export type ToolPermissions = z.infer<typeof ToolPermissionsSchema>;

/**
 * The tools that ship in the box.
 *
 * Here rather than in `ghostai-tools`, which is where they are actually
 * defined, because two packages downstream need the *names* without the
 * implementations: `ghostai-security` refuses a container that shadows one, and
 * `DEFAULT_AGENT_TOOLS` in `config.ts` seeds a new agent with them. Both sit
 * below `tools` in the layer graph. `packages/tools` owns a test that this list
 * still matches `BUILTIN_TOOLS`, which is the only place both are visible.
 */
export const BUILTIN_TOOL_NAMES: readonly string[] = [
  'read_file',
  'write_file',
  'edit_file',
  'list_dir',
  'exec',
  'automation',
  'memory',
  'skill',
];

/** Where a registered tool came from, so `unregisterBySource` can be exact. */
export const ToolSourceSchema = z.enum(['builtin', 'mcp', 'extension']);
export type ToolSource = z.infer<typeof ToolSourceSchema>;

/**
 * Hints about a tool's effects, mirroring MCP's annotation vocabulary so
 * built-in tools and tools proxied from an MCP server describe themselves the
 * same way.
 */
export const ToolAnnotationsSchema = z.object({
  title: z.string().optional(),
  readOnlyHint: z.boolean().optional(),
  destructiveHint: z.boolean().optional(),
  idempotentHint: z.boolean().optional(),
  openWorldHint: z.boolean().optional(),
});
export type ToolAnnotations = z.infer<typeof ToolAnnotationsSchema>;

/**
 * A tool as advertised to a model or listed over REST.
 *
 * `parameters` is an already-computed JSON Schema object (from
 * `z.toJSONSchema`), kept opaque here: validating a JSON Schema *document*
 * against Zod would buy nothing, and the value is produced by this repo rather
 * than accepted from a client.
 */
export const ToolDefinitionSchema = z.object({
  name: z.string().min(1),
  description: z.string(),
  parameters: z.record(z.string(), z.unknown()),
  risk: ToolRiskSchema.default('safe'),
  source: ToolSourceSchema.default('builtin'),
  annotations: ToolAnnotationsSchema.optional(),
});
export type ToolDefinition = z.infer<typeof ToolDefinitionSchema>;

/**
 * How long an approval decision holds.
 *
 * `session` is what makes the prompt tolerable in practice — approving `exec`
 * once per session rather than once per call.
 */
export const ApprovalScopeSchema = z.enum(['once', 'session', 'always']);
export type ApprovalScope = z.infer<typeof ApprovalScopeSchema>;

// Rewriting what a tool says about itself

/**
 * An operator's replacement for one tool's prose.
 *
 * A tool's description is the sentence that decides whether the model reaches
 * for it, and until now it was a string literal next to the handler — the one
 * part of the payload an operator could see in the context inspector and not
 * change. This is the same argument that made the whole system prompt editable,
 * one layer down.
 *
 * **Prose only, and that boundary is load-bearing.** `type`, `required`, `enum`
 * and the rest of the schema stay generated from the tool's Zod object, which is
 * also what `parseArgs` validates against. Letting an operator supply a schema
 * would let the advertised shape drift from the accepted one, and the failure
 * mode is a model dutifully passing a field that then fails validation on every
 * call — an agent that looks broken for a reason nothing reports.
 */
export const ToolPromptOverrideSchema = z.strictObject({
  /**
   * Replaces the tool's description. Empty means the built-in.
   *
   * A single space advertises the tool with no description at all, which is the
   * same "empty inherits, whitespace deletes" rule the prompt templates use.
   * Rarely what anyone wants — a nameless verb is a tool the model guesses at —
   * but it is the only way to say it, so it is available.
   */
  description: z.string().default(''),
  /**
   * Top-level parameter name → its description.
   *
   * Top-level only. A path syntax reaching `argv.items.description` would be a
   * second mini-language to specify and to validate, for the sake of a field
   * whose parent description can say the same thing in a sentence.
   *
   * A name that is not in the schema is reported and dropped rather than added:
   * inventing a property would advertise an argument the model then passes and
   * `parseArgs` then rejects.
   */
  fields: z.record(z.string(), z.string()).default({}),
});
export type ToolPromptOverride = z.infer<typeof ToolPromptOverrideSchema>;

/** Tool name → its prose overrides. Keyed the way permissions are, for the same reason. */
export const ToolPromptOverridesSchema = z.record(
  z.string(),
  ToolPromptOverrideSchema,
);
export type ToolPromptOverrides = z.infer<typeof ToolPromptOverridesSchema>;

/** What `applyToolPrompts` could not apply, for the warning sink and the editor. */
interface ToolPromptMisses {
  /** Override keys naming no advertised tool. */
  readonly unknownTools: readonly string[];
  /** `<tool>.<field>` pairs naming no property in that tool's schema. */
  readonly unknownFields: readonly string[];
}

interface AppliedToolPrompts extends ToolPromptMisses {
  readonly definitions: readonly ToolDefinition[];
}

/** The `properties` map of a JSON Schema object, when it has one. */
function propertiesOf(
  parameters: Record<string, unknown>,
): Record<string, unknown> | undefined {
  const properties = parameters.properties;
  if (
    typeof properties !== 'object' ||
    properties === null ||
    Array.isArray(properties)
  ) {
    return undefined;
  }
  return properties as Record<string, unknown>;
}

/**
 * The definitions, with each operator's wording in place of the compiled one.
 *
 * Pure, and the only place a definition's prose is rewritten — so "what does the
 * model actually see" has one answer and the context inspector shows it without
 * reassembling anything.
 *
 * **Nothing here mutates.** `parameters` is frozen at definition time and shared
 * by every turn on every agent through the registry's memoised list; writing a
 * description into it in place would give one agent's wording to all of them.
 * Each level is cloned on the way down, and only where an override actually
 * lands — a definition nobody overrode is passed through by reference.
 */
export function applyToolPrompts(
  definitions: readonly ToolDefinition[],
  overrides: ToolPromptOverrides,
): AppliedToolPrompts {
  const names = Object.keys(overrides);
  if (names.length === 0) {
    return { definitions, unknownTools: [], unknownFields: [] };
  }

  const seen = new Set<string>();
  const unknownFields: string[] = [];

  const applied = definitions.map((definition) => {
    const override = overrides[definition.name];
    if (override === undefined) return definition;
    seen.add(definition.name);

    const fields = Object.entries(override.fields);
    const properties =
      fields.length === 0 ? undefined : propertiesOf(definition.parameters);

    let parameters = definition.parameters;
    if (properties !== undefined) {
      const next: Record<string, unknown> = { ...properties };
      let changed = false;
      for (const [field, description] of fields) {
        const property = next[field];
        if (
          typeof property !== 'object' ||
          property === null ||
          Array.isArray(property)
        ) {
          unknownFields.push(`${definition.name}.${field}`);
          continue;
        }
        next[field] = { ...(property as Record<string, unknown>), description };
        changed = true;
      }
      if (changed) parameters = { ...definition.parameters, properties: next };
    } else if (fields.length > 0) {
      // A tool whose schema has no `properties` at all — `z.strictObject({})`.
      // Every named field is unknown, and saying so once per field matches what
      // the operator wrote.
      for (const [field] of fields) {
        unknownFields.push(`${definition.name}.${field}`);
      }
    }

    return {
      ...definition,
      ...(override.description === ''
        ? {}
        : { description: override.description.trim() }),
      parameters,
    };
  });

  return {
    definitions: applied,
    unknownTools: names.filter((name) => !seen.has(name)),
    unknownFields,
  };
}
