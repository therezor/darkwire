/**
 * An agent preset: an installable agent definition.
 *
 * A preset is a JSON file — beside a toolbox manifest, or bundled with the CLI
 * — that `ghostai agent install` turns into an entry in `agents.list`. After
 * install it is ordinary agent config: the operator edits it in the UI, and
 * nothing remembers where it came from. That is the whole design — a preset is
 * a starting point, not a subscription, which is why there is no version field
 * to reconcile and no update command to run.
 *
 * The shape is a strict subset of `AgentEntry`, and what is *absent* is the
 * point:
 *
 *  - **No `model`, `provider`, `temperature` or token caps.** Those describe an
 *    install — which endpoints exist, what the hardware can hold — and a preset
 *    describes a role. A preset shipping a model id would break on every
 *    machine that lacks it, so the installer copies the default agent's in.
 *  - **No `exec` patch and no `enabled` flag.** Install sets `enabled: true`,
 *    because installing a disabled agent is a contradiction; the exec
 *    allow-list is the operator's to tighten afterwards.
 *  - **`toolbox` is `AgentToolboxSchema`** — a name and a network *request*.
 *    Everything that could widen the boundary (image, caps, limits) lives in
 *    the toolbox manifest, approved by hash, and has no representation here to
 *    reach. A preset can therefore express nothing a settings save could not.
 *
 * `toolsEnabled` is the one settings knob a preset may set, because one
 * preset exists specifically to switch it off: a no-tools agent whose every
 * request is the short cheap kind. Optional, so an ordinary preset says nothing
 * and takes the schema's `true`.
 *
 * `skills` is the one field that is *not* an `AgentEntry` field at all. It is an
 * install instruction — which sheets to copy out of the catalogue and into the
 * workspace — so `presetToAgentEntry` drops it rather than carrying it into
 * config. Nothing remembers afterwards that a sheet arrived with a preset, which
 * is the same "a preset is a starting point, not a subscription" rule the rest of
 * this file keeps.
 */

import { z } from 'zod';

import {
  AgentEntrySchema,
  AgentToolboxSchema,
  DEFAULT_AGENT_TOOLS,
  PromptModeSchema,
  SubagentRefSchema,
} from './config.js';
import type { AgentEntry } from './config.js';
import { SLUG_ID_PATTERN } from './ids.js';
import { ToolPermissionsSchema } from './tools.js';

export const AgentPresetSchema = z.object({
  /** Bumped only for a breaking preset change; refused when unrecognised. */
  schema: z.literal('ghostai.agent-preset/1'),
  /**
   * Becomes the `agents.list` key. The install command holds it to the same
   * rules the UI does — `isAgentId`, nothing in `RESERVED_AGENT_IDS` — since a
   * preset arrives from disk, not from the form that enforces them.
   */
  id: z.string().min(1).max(40),
  /** Shown in the UI. Empty falls back to the id. */
  label: z.string().default(''),

  // The eight prompt templates, with `AgentEntry`'s three-state semantics:
  // empty inherits the built-in, a single space deletes the section, anything
  // else replaces it. A preset's `systemPrompt` is where a toolbox's tool
  // documentation lives — it replaces the default template wholesale, so it
  // carries its own heading and workspace section.
  systemPrompt: z.string().default(''),
  livePrompt: z.string().default(''),
  wrapUpPrompt: z.string().default(''),
  platformPrompt: z.string().default(''),
  toolboxPrompt: z.string().default(''),
  toolPolicyPrompt: z.string().default(''),
  memoryPrompt: z.string().default(''),
  skillsPrompt: z.string().default(''),
  promptMode: PromptModeSchema.default('template'),

  /** The one settings knob a preset may set. Unset takes the schema's `true`. */
  toolsEnabled: z.boolean().optional(),
  /** Replaces, never merges — the same rule as `AgentEntry.tools`. */
  tools: ToolPermissionsSchema.default({ ...DEFAULT_AGENT_TOOLS }),
  toolbox: AgentToolboxSchema.prefault({}),
  subagents: z.array(SubagentRefSchema).default([]),

  /**
   * Skill directories to copy out of the catalogue's `skills/` and into the
   * workspace's, named by directory. An install instruction rather than agent
   * config, so `presetToAgentEntry` drops it.
   *
   * `SLUG_ID_PATTERN` because each name becomes a path segment, and this one
   * arrived over the network: `..`, `/` and a leading `~` are unrepresentable,
   * so the copier never has to judge a traversal — a preset carrying one fails
   * to parse. Deliberately stricter than what a *workspace* may hold, where a
   * sheet directory is whatever a person named it and `readSkills` reads it
   * happily. The bound is a floor under a bad publish, not a policy.
   */
  skills: z.array(z.string().regex(SLUG_ID_PATTERN)).max(32).default([]),
});
export type AgentPreset = z.infer<typeof AgentPresetSchema>;

/**
 * The `agents.list` entry a preset installs as.
 *
 * Built through `AgentEntrySchema.parse` rather than a spread into a literal,
 * so the entry's own defaults are applied by the schema that owns them — a
 * field added to `AgentEntry` later gets its default here without this
 * function knowing it exists. `toolsEnabled` is spread only when the preset
 * set it: an absent key takes the schema's default, and `undefined` written
 * into the patch would be a claim, not an absence.
 *
 * `skills` is named in the destructure rather than left to be stripped. Zod
 * would drop it either way — `AgentEntrySchema` is not strict — but the spread
 * above exists so that a *new `AgentEntry` field* picks up its default here
 * without this function knowing about it, and a key that must never reach the
 * entry is the opposite case. Naming it is what says so.
 */
export function presetToAgentEntry(preset: AgentPreset): AgentEntry {
  const { schema, id, toolsEnabled, skills, ...fields } = preset;
  return AgentEntrySchema.parse({
    ...fields,
    ...(toolsEnabled === undefined ? {} : { toolsEnabled }),
    enabled: true,
  });
}
