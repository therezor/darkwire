/**
 * The agent editor's form, as pure functions.
 *
 * Validation is the part of a settings screen worth testing, and a component
 * test can only reach it through seven keystrokes and a button press — so it
 * lives here, in functions that take strings and return a `ConfigPatch` or the
 * errors that stopped one.
 *
 * **Every agent holds its own settings, and nothing inherits.** An agent is its
 * `agents.list` entry; a field the entry does not name is filled by the schema's
 * own default, not by another agent's answer. So a box on this screen reads what
 * that agent actually runs on, which is the whole point of the screen.
 *
 * There is one form type because there is one subtree. `AgentEntryForm` edits
 * `agents.list.<id>` and the default agent is edited through it like any other.
 *
 * The patch this builds *is* the agent: `agents.list.*` is in the merge's
 * `REPLACE_WHOLESALE` list, so a field left out of it is a field cleared. That
 * is why `toAgentEntryPatch` takes the stored entry as well as the form — the
 * settings this screen does not render (the memory scope and the exec
 * allow-list) have to be carried through by hand, or saving the prompt would
 * quietly delete them.
 *
 * One asymmetry is a property of the patch format rather than of this file.
 * `reasoningEffort` and `temperature` are optional in the config, and the two
 * are genuinely unset rather than defaulted: unset means the request carries no
 * such parameter and the provider applies its own, which is the only thing that
 * works for a model that rejects it. Wholesale replacement is what lets the form
 * express that — omitting the key *is* clearing it.
 */

import type { TFunction } from 'i18next';

import {
  type AgentEntry,
  type ConfigPatch,
  type PromptMode,
  type ReasoningEffort,
  type SubagentRef,
  type ToolPermission,
  type ToolPromptOverride,
} from '@darkwire/protocol';

import {
  msToSeconds,
  parseNumber,
  secondsToMs,
  type PatchResult,
} from '@/components/form/fields.js';

export type { PatchResult };

/**
 * The value the "leave this unset" option carries.
 *
 * Not the empty string, which is what the form holds: an empty `value` means
 * *no* value to a Radix select, so the option would select nothing and the
 * trigger would render blank — a control that looks broken while working.
 */
export const UNSET_VALUE = '__unset__';

/**
 * Why a model is required rather than merely encouraged.
 *
 * The schema's "empty means resolve from whichever provider has credentials"
 * reads as though the screen could offer "Resolved automatically". That
 * sentence is about the **provider**: `resolve_instance` uses the model as a
 * hint when picking an endpoint. The model itself is never invented —
 * `WireRuntime` turns an empty one into `no_model_error`, hands the loop no
 * provider, and the agent cannot run a turn at all.
 *
 * Such an option would offer an unconfigured install as though it were a
 * setting. Blank is a state the form refuses rather than one it saves.
 */
export const MODEL_REQUIRED =
  'Choose a model. An agent with none cannot run a turn.';

export const REASONING_EFFORTS: readonly ReasoningEffort[] = [
  'off',
  'minimal',
  'low',
  'medium',
  'high',
  'xhigh',
];

export interface AgentEntryForm {
  readonly label: string;
  readonly systemPrompt: string;
  /**
   * The seven other templates an agent owns, and the mode that decides whether
   * any of them are placed.
   *
   * Held raw, unlike almost everything else on this form: `''` and `' '` mean
   * different things — inherit the built-in, and delete the section — so a
   * trim anywhere on the way through would make deleting one impossible to
   * express. `systemPrompt` above has always been raw for the same reason.
   *
   * `memoryPrompt` and `skillsPrompt` are the odd ones out in *where they are
   * placed* rather than in how they are held: the other five fill sections the
   * prompt builder writes, while those two are contributors'. They are edited
   * beside the rest because an operator editing their prompt does not care which
   * of the two wrote a paragraph.
   */
  readonly livePrompt: string;
  readonly wrapUpPrompt: string;
  readonly platformPrompt: string;
  readonly toolPolicyPrompt: string;
  readonly memoryPrompt: string;
  readonly skillsPrompt: string;
  readonly promptMode: string;
  /**
   * Tool name → the operator's replacement for what it tells the model.
   *
   * The stored shape, for the reason `tools` below is: every value is free text
   * behind a labelled box, and there is nothing for a parse step to do.
   */
  readonly toolPrompts: Readonly<Record<string, ToolPromptOverride>>;
  readonly enabled: boolean;
  readonly provider: string;
  /** Empty asks the registry to resolve one — a value, not an absence. */
  readonly model: string;
  readonly maxTokens: string;
  readonly contextWindowTokens: string;
  /** Empty sends no temperature at all, so the provider applies its own. */
  readonly temperature: string;
  readonly reasoningEffort: string;
  /**
   * The two capability switches, per agent for the reason `model` above is: an
   * agent that pins its own model needs its own answer about what that model
   * can do.
   *
   * `toolsEnabled` is not `tools` below and does not overlap it. That one is
   * which tools this agent may use; this one is whether the model is told about
   * any of them. Switching it off leaves the permission map exactly as it is,
   * so moving the agent back to a capable model restores its toolset intact.
   */
  readonly visionEnabled: boolean;
  readonly toolsEnabled: boolean;
  readonly toolTimeoutSeconds: string;
  /**
   * Per agent, like everything else here. These two were the default agent's
   * alone while they lived in a separate subtree; there is one subtree now, and
   * a tool budget is as much a property of one agent as its model is.
   */
  readonly maxToolIterations: string;
  readonly loopWallTimeoutSeconds: string;
  /**
   * The tool layer's numbers, this agent's own. They were install-wide once
   * and sat in Settings; an agent on a small model wants a different result
   * budget from one on a large model, so they moved here beside the other caps.
   */
  readonly approvalTimeoutSeconds: string;
  readonly maxOutputChars: string;
  readonly execTimeoutSeconds: string;
  readonly execMaxOutputBytes: string;
  /**
   * The short tool list: `tool_search` plus the pins, with the rest found by
   * name. Per agent, because the agent on a small local model wants it and the
   * agent on a large hosted one may not.
   */
  readonly lazyDiscovery: boolean;
  /** Sent whole on every save, so unpinning is expressible. */
  readonly pinnedTools: readonly string[];
  /**
   * What this agent's memory index may cost in the prompt.
   *
   * Per agent for the reason the budget above is: an agent on a small window
   * cannot afford to be told about as many memories as one on a large one.
   * Whether it remembers *at all* is not here — that is the `memory` tool's
   * permission in `tools` below, and a second switch beside it is how the two
   * come to disagree.
   */
  /**
   * Tool name → permission. A name absent from the map is not enabled.
   *
   * Held as the stored shape rather than as text, unlike the CIDR list below:
   * this is a set of choices from a fixed vocabulary, and the editor renders one
   * control per entry. There is nothing to parse and nothing a typo can express.
   */
  readonly tools: Readonly<Record<string, ToolPermission>>;
  /**
   * The agents this one may delegate to, in the operator's order.
   *
   * The stored shape rather than a form-shaped copy of it, for the reason
   * `tools` above is: an id and a permission both come from a fixed vocabulary
   * behind a picker, and the guidance is free text that needs no parsing. A row
   * whose `id` is still empty is one the operator added and has not filled in;
   * `ownFields` drops those rather than sending an id nothing resolves.
   */
  readonly subagents: readonly SubagentRef[];
  /** An installed environment name, or empty to run on the host. */
  readonly environmentName: string;
  readonly environmentAlwaysUseOwn: boolean;
  readonly environmentNetworkMode: string;
  /** Comma-separated CIDR blocks. Only read when the mode is `allowlist`. */
  readonly environmentAllow: string;
  /** Comma-separated DNS names. Only read when the mode is `allowlist`. */
  readonly environmentHosts: string;
  /** Comma-separated resolver addresses. Only read when the mode is `allowlist`. */
  readonly environmentDns: string;
}

/**
 * The three permissions, in the order the select offers them.
 *
 * Widest first, because that is the order the consequences run in and a picker
 * that reads `deny, allow, ask` makes the operator work out the ordering
 * themselves every time they open it.
 */
export const TOOL_PERMISSIONS: readonly ToolPermission[] = [
  'allow',
  'ask',
  'deny',
];

/**
 * One agent's stored settings, as boxes.
 *
 * A straight read, with no fallback anywhere: the entry is complete by the time
 * it gets here, because `AgentEntrySchema` filled whatever the file did not name.
 * The two boxes that can legitimately be empty are the two the config leaves
 * genuinely unset — `temperature` and `reasoningEffort` — and empty is what
 * "send no such parameter, let the provider decide" looks like.
 */
export function toAgentEntryForm(entry: AgentEntry): AgentEntryForm {
  const temperature = entry.temperature;
  const toolTimeoutMs = entry.toolTimeoutMs;

  return {
    label: entry.label,
    systemPrompt: entry.systemPrompt,
    // Empty is a meaningful stored value here — it is what "follow the built-in
    // and keep receiving improvements to it" is spelled as — which is why none
    // of the eight is ever substituted for on the way in or out.
    livePrompt: entry.livePrompt,
    wrapUpPrompt: entry.wrapUpPrompt,
    platformPrompt: entry.platformPrompt,
    toolPolicyPrompt: entry.toolPolicyPrompt,
    memoryPrompt: entry.memoryPrompt,
    skillsPrompt: entry.skillsPrompt,
    promptMode: entry.promptMode,
    toolPrompts: { ...entry.toolPrompts },
    enabled: entry.enabled,
    provider: entry.provider,
    model: entry.model,
    maxTokens: String(entry.maxTokens),
    contextWindowTokens: String(entry.contextWindowTokens),
    // Not `?? ''` on the whole expression: `0` is a temperature, and a falsy
    // check here would render it as "the provider's own".
    temperature: temperature === undefined ? '' : String(temperature),
    reasoningEffort: entry.reasoningEffort ?? '',
    visionEnabled: entry.visionEnabled,
    toolsEnabled: entry.toolsEnabled,
    toolTimeoutSeconds: msToSeconds(toolTimeoutMs),
    maxToolIterations: String(entry.maxToolIterations),
    loopWallTimeoutSeconds: msToSeconds(entry.loopWallTimeoutMs),
    approvalTimeoutSeconds: msToSeconds(entry.approvalTimeoutMs),
    maxOutputChars: String(entry.maxOutputChars),
    execTimeoutSeconds: msToSeconds(entry.exec.timeoutMs),
    execMaxOutputBytes: String(entry.exec.maxOutputBytes),
    lazyDiscovery: entry.lazyDiscovery,
    pinnedTools: [...entry.pinnedTools],
    tools: { ...entry.tools },
    subagents: entry.subagents.map((ref) => ({ ...ref })),
    environmentName: entry.environment.name,
    environmentAlwaysUseOwn: entry.environment.alwaysUseOwn,
    environmentNetworkMode: entry.environment.network.mode,
    environmentAllow: entry.environment.network.allow.join(', '),
    environmentHosts: entry.environment.network.hosts.join(', '),
    environmentDns: entry.environment.network.dns.join(', '),
  };
}

type AgentPatchResult =
  | { readonly ok: true; readonly patch: ConfigPatch }
  | { readonly ok: false; readonly errors: Readonly<Record<string, string>> };

/** `a, b , ,c` → `['a','b','c']`. Empty entries dropped, order kept. */
export function parseList(value: string): string[] {
  return value
    .split(',')
    .map((name) => name.trim())
    .filter((name) => name !== '');
}

function isReasoningEffort(value: string): value is ReasoningEffort {
  return (REASONING_EFFORTS as readonly string[]).includes(value);
}

export function isToolPermission(value: string): value is ToolPermission {
  return (TOOL_PERMISSIONS as readonly string[]).includes(value);
}

/** The two prompt modes, in the order the toggle offers them. */
const PROMPT_MODES: readonly PromptMode[] = ['template', 'raw'];

function isPromptMode(value: string): value is PromptMode {
  return (PROMPT_MODES as readonly string[]).includes(value);
}

/**
 * The overrides that actually say something.
 *
 * The editor holds a row for every tool an operator has opened, so most of them
 * are empty most of the time. Storing those would fill `config.yaml` with blank
 * descriptions that are indistinguishable, on the way back in, from a deliberate
 * one — and `''` is precisely the value that means "inherit the built-in", so a
 * stored blank is not merely noise but a lie about what was chosen.
 */
export function pruneToolPrompts(
  overrides: Readonly<Record<string, ToolPromptOverride>>,
): Record<string, ToolPromptOverride> {
  const kept: Record<string, ToolPromptOverride> = {};
  for (const [name, override] of Object.entries(overrides)) {
    const fields = Object.fromEntries(
      Object.entries(override.fields).filter(([, text]) => text !== ''),
    );
    if (override.description === '' && Object.keys(fields).length === 0) {
      continue;
    }
    kept[name] = { description: override.description, fields };
  }
  return kept;
}

/**
 * The half of an agent that is the same whichever agent it is.
 *
 * Its prompt, its name, its tool permissions — plus every setting this screen
 * does not render, carried straight through from the stored entry. That
 * carry-through is not defensive: `agents.list.*` is replaced wholesale, so an
 * entry rebuilt from the form alone loses its sandbox, its memory scope and its
 * exec allow-list every time the prompt is saved.
 *
 * The model and the sampling settings are deliberately *not* here — see
 * `AgentOwnFields` below, which states the type and the reason.
 */
/**
 * The half of an agent the two builders share.
 *
 * The model and the sampling settings are omitted because both callers compute
 * them — one parses boxes, the other copies a template — and a stale value
 * spread through here would make an unset field impossible to express, since
 * omitting a key is the only way this patch format can clear one.
 *
 * There is no `Required<>` guard beside it any more. `AgentEntry` is a complete
 * schema now rather than a patch of one, so every field is required by the type
 * itself and `satisfies AgentEntry` at each call site says it.
 */
type AgentOwnFields = Omit<
  AgentEntry,
  | 'provider'
  | 'model'
  | 'maxTokens'
  | 'contextWindowTokens'
  | 'temperature'
  | 'reasoningEffort'
  | 'toolTimeoutMs'
  | 'maxToolIterations'
  | 'loopWallTimeoutMs'
  | 'approvalTimeoutMs'
  | 'maxOutputChars'
  | 'exec'
>;

function ownFields(form: AgentEntryForm, entry: AgentEntry): AgentOwnFields {
  // Dropped rather than spread — see `AgentOwnFields`.
  const {
    provider,
    model,
    maxTokens,
    contextWindowTokens,
    temperature,
    reasoningEffort,
    toolTimeoutMs,
    maxToolIterations,
    loopWallTimeoutMs,
    approvalTimeoutMs,
    maxOutputChars,
    exec,
    subagents,
    ...carried
  } = entry;

  return {
    ...carried,
    label: form.label.trim(),
    systemPrompt: form.systemPrompt,
    // Untrimmed, all eight. A single space is how an operator deletes a
    // section, and it is the only way to say it — empty already means
    // "inherit".
    livePrompt: form.livePrompt,
    wrapUpPrompt: form.wrapUpPrompt,
    platformPrompt: form.platformPrompt,
    toolPolicyPrompt: form.toolPolicyPrompt,
    memoryPrompt: form.memoryPrompt,
    skillsPrompt: form.skillsPrompt,
    promptMode: isPromptMode(form.promptMode) ? form.promptMode : 'template',
    // Sent whole for the same reason `tools` is: the merge replaces
    // `agents.list.*` wholesale, so this is also the only way an override can be
    // removed. An entry that says nothing is dropped rather than written as a
    // row of empty strings — the editor holds one for every tool with a box on
    // screen, and storing those would fill the config with blanks that read as
    // deliberate.
    toolPrompts: pruneToolPrompts(form.toolPrompts),
    enabled: form.enabled,
    // Always written, never inherited-by-omission. The form arrived holding the
    // default agent's answer, so leaving these out would make the agent follow
    // a later change to the default it had already been shown disagreeing with.
    visionEnabled: form.visionEnabled,
    toolsEnabled: form.toolsEnabled,
    lazyDiscovery: form.lazyDiscovery,
    // Sent whole, every time. The merge replaces `agents.list.*` wholesale, so
    // this is also the only way a tool can be removed from an agent — a patch
    // that mentioned only what changed could never express a deletion.
    tools: { ...form.tools },
    // Same rule: the whole list, so a pin can be taken off.
    pinnedTools: [...form.pinnedTools],
    // Same rule, and the same reason a removed row has to be expressible.
    // A row with no agent chosen is dropped: the editor adds an empty one when
    // the operator presses Add, and saving before they pick would otherwise be
    // a `config` error from `assertBuildable` about an agent named "".
    subagents: form.subagents
      .filter((ref) => ref.id !== '')
      .map((ref) => ({ ...ref, prompt: ref.prompt.trim() })),
    environment: toEnvironment(form),
  };
}

/**
 * The environment the form describes, and what it may reach.
 *
 * Every list is dropped unless the mode actually uses it, so switching to
 * `none` and saving does not leave a stale set of CIDRs in the file waiting to
 * take effect the next time somebody switches back. The network goes with the
 * environment for the same reason: egress is enforced by the environment's
 * gateway, so a request left behind by an agent that no longer has one would
 * mean nothing until somebody picked one again.
 */
function toEnvironment(form: AgentEntryForm): AgentEntry['environment'] {
  const name = form.environmentName.trim();
  const mode = name === '' ? 'none' : networkMode(form.environmentNetworkMode);
  const scoped = mode === 'allowlist';
  return {
    name,
    // Kept whatever the name is. An agent pinned to the host is a real answer,
    // and the one that had no spelling before this field existed.
    alwaysUseOwn: form.environmentAlwaysUseOwn,
    network: {
      mode,
      allow: scoped ? parseList(form.environmentAllow) : [],
      hosts: scoped ? parseList(form.environmentHosts) : [],
      dns: scoped ? parseList(form.environmentDns) : [],
    },
  };
}

const NETWORK_MODES: ReadonlyArray<
  AgentEntry['environment']['network']['mode']
> = ['none', 'allowlist', 'open'];

function networkMode(
  value: string,
): AgentEntry['environment']['network']['mode'] {
  return NETWORK_MODES.find((mode) => mode === value) ?? 'none';
}

/**
 * One agent's form into a patch under `agents.list.<id>`.
 *
 * Every numeric field is checked and *all* failures are returned rather than
 * the first, matching `toAgentPatch`: a form that reports one error per press
 * makes the operator find them one press at a time.
 *
 * An empty box is an error for the fields that must have a value, because they
 * no longer have anywhere to fall back to — the form arrives holding the
 * default agent's numbers, so a blank one is something the operator deleted
 * rather than something they never set. The two that *can* be unset,
 * `temperature` and `reasoningEffort`, are left out of the patch when blank,
 * which is what clears them.
 */
export function toAgentEntryPatch(
  id: string,
  form: AgentEntryForm,
  entry: AgentEntry,
  t: TFunction,
): AgentPatchResult {
  const errors: Record<string, string> = {};

  if (id.trim() === '') errors.id = 'Required';
  if (form.provider.trim() === '') errors.provider = 'Required';
  if (form.model.trim() === '') errors.model = MODEL_REQUIRED;

  const required = (
    field: string,
    value: string,
    options: Parameters<typeof parseNumber>[2],
  ): number | undefined => {
    const result = parseNumber(value, t, options);
    if (!result.ok) {
      errors[field] = result.error;
      return undefined;
    }
    return result.value;
  };

  /** Parses only when the box has something in it — blank is a value here. */
  const optional = (
    field: string,
    value: string,
    options: Parameters<typeof parseNumber>[2],
  ): number | undefined => {
    if (value.trim() === '') return undefined;
    return required(field, value, options);
  };

  const maxTokens = required('maxTokens', form.maxTokens, {
    integer: true,
    min: 1,
  });
  const contextWindowTokens = required(
    'contextWindowTokens',
    form.contextWindowTokens,
    {
      integer: true,
      min: 1,
    },
  );
  const toolTimeout = required('toolTimeoutSeconds', form.toolTimeoutSeconds, {
    min: 0,
  });
  const maxToolIterations = required(
    'maxToolIterations',
    form.maxToolIterations,
    {
      integer: true,
      min: 1,
    },
  );
  const loopWallTimeout = required(
    'loopWallTimeoutSeconds',
    form.loopWallTimeoutSeconds,
    { min: 0 },
  );
  // `min: 1`, unlike every other duration here: an approval that never expires
  // holds the turn, its tool call and its provider connection open for a tab
  // that was closed an hour ago.
  const approvalTimeout = required(
    'approvalTimeoutSeconds',
    form.approvalTimeoutSeconds,
    { min: 1 },
  );
  // Also `min: 1`, and not because it is a duration: `read_file` sizes its
  // buffer from this, so 0 would read one byte of every file.
  const maxOutputChars = required('maxOutputChars', form.maxOutputChars, {
    integer: true,
    min: 1,
  });
  const execTimeout = required('execTimeoutSeconds', form.execTimeoutSeconds, {
    min: 0,
  });
  const execMaxOutputBytes = required(
    'execMaxOutputBytes',
    form.execMaxOutputBytes,
    { integer: true, min: 1 },
  );
  const temperature = optional('temperature', form.temperature, {
    min: 0,
    max: 2,
  });

  if (
    maxTokens === undefined ||
    contextWindowTokens === undefined ||
    toolTimeout === undefined ||
    maxToolIterations === undefined ||
    loopWallTimeout === undefined ||
    approvalTimeout === undefined ||
    maxOutputChars === undefined ||
    execTimeout === undefined ||
    execMaxOutputBytes === undefined ||
    Object.keys(errors).length > 0
  ) {
    return { ok: false, errors };
  }

  return {
    ok: true,
    patch: {
      agents: {
        list: {
          [id]: {
            ...ownFields(form, entry),
            provider: form.provider.trim(),
            model: form.model.trim(),
            maxTokens,
            contextWindowTokens,
            toolTimeoutMs: secondsToMs(toolTimeout),
            maxToolIterations,
            loopWallTimeoutMs: secondsToMs(loopWallTimeout),
            approvalTimeoutMs: secondsToMs(approvalTimeout),
            maxOutputChars,
            // The two boxes over the stored block, so the allow-list and the
            // environment allow-list this screen does not render survive a
            // save exactly as `ownFields` carries the rest.
            exec: {
              ...entry.exec,
              timeoutMs: secondsToMs(execTimeout),
              maxOutputBytes: execMaxOutputBytes,
            },
            ...(temperature === undefined ? {} : { temperature }),
            ...(isReasoningEffort(form.reasoningEffort)
              ? { reasoningEffort: form.reasoningEffort }
              : {}),
          },
        },
      },
    },
  };
}

/**
 * A new agent, prepopulated from the one it was started from.
 *
 * Everything is copied: the prompt, the tool permissions, the sandbox, the
 * memory scope, the model and the budget. Nothing is left out to be inherited,
 * because there is nothing to inherit from — an omitted field would be the
 * schema's default rather than the template's value, which is not a copy.
 *
 * Including the two that can be genuinely unset. A template that sends no
 * reasoning effort produces a copy that sends none either: spreading the
 * absence is what makes it a copy rather than an agent with a setting the
 * original did not have.
 */
export function toNewAgentPatch(
  id: string,
  label: string,
  template: AgentEntry,
): ConfigPatch {
  const { temperature, reasoningEffort } = template;

  return {
    agents: {
      list: {
        [id]: {
          label,
          enabled: true,
          systemPrompt: template.systemPrompt,
          // Every template the source agent had, for the same reason its prompt
          // is copied: a duplicate that reverted to the built-in platform note
          // would not be a copy of the agent it was stamped from.
          livePrompt: template.livePrompt,
          wrapUpPrompt: template.wrapUpPrompt,
          platformPrompt: template.platformPrompt,
          toolPolicyPrompt: template.toolPolicyPrompt,
          memoryPrompt: template.memoryPrompt,
          skillsPrompt: template.skillsPrompt,
          promptMode: template.promptMode,
          toolPrompts: structuredClone(template.toolPrompts),
          tools: { ...template.tools },
          // Written, not omitted — the same rule `ownFields` states for the
          // editor's save. An omitted key would take the schema's `true`, so
          // duplicating an agent with vision switched off produced one with
          // vision switched on.
          visionEnabled: template.visionEnabled,
          toolsEnabled: template.toolsEnabled,
          lazyDiscovery: template.lazyDiscovery,
          pinnedTools: [...template.pinnedTools],
          approvalTimeoutMs: template.approvalTimeoutMs,
          maxOutputChars: template.maxOutputChars,
          exec: { ...template.exec },
          // The one thing deliberately *not* copied. Everything else here is a
          // setting — how the agent thinks, what it may touch, what it costs —
          // and inheriting those is what "a copy of the default agent" means.
          // Delegation is not a setting but a relationship: it says this agent's
          // job is partly somebody else's, which is specific to the job and not
          // to the template it was stamped from.
          //
          // It also compounds. A prompt copied into an agent that did not want
          // it is one wrong sentence; a delegation copied into every agent
          // created afterwards puts a tool in front of each of them, and the
          // model will use it.
          subagents: [],
          environment: { ...template.environment },
          provider: template.provider,
          model: template.model,
          maxTokens: template.maxTokens,
          contextWindowTokens: template.contextWindowTokens,
          toolTimeoutMs: template.toolTimeoutMs,
          maxToolIterations: template.maxToolIterations,
          loopWallTimeoutMs: template.loopWallTimeoutMs,
          subagentTimeoutMs: template.subagentTimeoutMs,
          ...(temperature === undefined ? {} : { temperature }),
          ...(reasoningEffort === undefined ? {} : { reasoningEffort }),
          // `satisfies`, so leaving out a stated field is a compile error here
          // as well as in `ownFields`. The two builders write the same entry
          // from different sources — a form and a stored template — which is
          // exactly how they came to disagree.
        } satisfies AgentEntry,
      },
    },
  };
}

/**
 * Switching an agent on or off from the list, without opening it.
 *
 * The whole entry goes back for the same reason a rename sends one: the merge
 * replaces `agents.list.*` wholesale, so `{ enabled: false }` alone would
 * disable the agent by deleting everything else about it.
 *
 * A disabled agent is kept rather than removed — `listAgents` skips it and
 * `resolveAgent` refuses it, but its prompt and permissions are still there
 * when it is switched back on. That is the difference between this and Delete,
 * and it is why both are in the row menu.
 */
export function toAgentEnabledPatch(
  id: string,
  entry: AgentEntry,
  enabled: boolean,
): ConfigPatch {
  return { agents: { list: { [id]: { ...entry, enabled } } } };
}

/**
 * The patch that removes an agent.
 *
 * `null` is the one token the merge reads as a deletion, and only at the paths
 * in its `DELETE_BY_NULL` list — of which `agents.list.*` is one.
 */
export function toAgentDeletePatch(id: string): ConfigPatch {
  return { agents: { list: { [id]: null } } };
}
