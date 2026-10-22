/**
 * The agent editor's form state, and everything its sections read off it.
 *
 * One hook rather than state spread across the sections, because the sections
 * are not independent: the tool switches gate the prompt warnings, the
 * environment picker feeds the platform template, and Save reads all of it.
 */

import { useQuery } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

import {
  DEFAULT_AGENT_ID,
  DEFAULT_LIVE_STATE_TEMPLATE,
  deriveAgentId,
  namesDelimiter,
  type AgentEntry,
  type SubagentRef,
  type ToolPermission,
  type ToolPromptOverride,
} from '@darkwire/protocol';

import type { WebKey } from '@/i18n/keys.js';
import { api } from '@/lib/api.js';
import { queryKeys } from '@/lib/query.js';
import { useAgentsQuery } from '@/lib/queries.js';
import { modelOptions } from '@/components/form/fields.js';
import { useSaveSettings } from '@/settings/use-settings.js';
import {
  toAgentDeletePatch,
  toAgentEntryForm,
  toAgentEntryPatch,
  type AgentEntryForm,
} from './agents-form.js';
import { useAgent } from './agent-context.js';
import type { Bound, Toggle } from './editor/bound-fields.js';
import {
  EXEC_TOOL,
  FEATURE_TOOLS,
  HOST_ENVIRONMENT,
  MCP_TOOL_PREFIX,
  TOOL_SEARCH,
} from './editor/constants.js';

export interface AgentEditorProps {
  /**
   * `create` seeds from the template and POSTs the agent into existence on
   * Save; `edit` loads the stored entry and patches it. It decides three things
   * and nothing else: the seed, what Save does, and whether the edit-only
   * controls render (Delete, and the id box's rename behaviour).
   */
  readonly mode: 'create' | 'edit';
  readonly agentId: string;
  readonly entry: AgentEntry;
  readonly list: Readonly<Record<string, AgentEntry>>;
}

export type AgentEditor = ReturnType<typeof useAgentEditor>;

/**
 * The string-valued boxes on the form.
 *
 * Narrowed by value type rather than listed, so `tools` and `toolPrompts` cannot
 * be bound this way and trying is a compile error rather than a control that
 * silently edits nothing.
 */
type StringField = {
  [K in keyof AgentEntryForm]: AgentEntryForm[K] extends string ? K : never;
}[keyof AgentEntryForm];

/** The same, for the switches. */
type BooleanField = {
  [K in keyof AgentEntryForm]: AgentEntryForm[K] extends boolean ? K : never;
}[keyof AgentEntryForm];

/**
 * The placeholders whose value differs between two requests in the same turn.
 *
 * Only used to warn a raw template about the prefix cache. In Sections mode
 * these live in the block at the end, which is rebuilt every iteration anyway;
 * one of them in a raw template makes the *whole* prompt uncacheable.
 */
const VOLATILE: readonly string[] = [
  'time',
  'wrapUp',
  'iteration',
  'iterationsLeft',
  'nonce',
  'tag',
  'toolPolicy',
  'runtimeSections',
  'correction',
];

export function useAgentEditor({
  mode,
  agentId,
  entry,
  list,
}: AgentEditorProps) {
  const { t } = useTranslation();
  const creating = mode === 'create';
  // A new agent is never the default one, whatever its id box says.
  const isDefault = !creating && agentId === DEFAULT_AGENT_ID;
  const navigate = useNavigate();
  const { agentId: active, select } = useAgent();
  const { save, saving } = useSaveSettings();

  const [form, setForm] = useState<AgentEntryForm>(() =>
    toAgentEntryForm(entry),
  );
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>({});
  const [dirty, setDirty] = useState(false);
  /**
   * The id box, which is an ordinary field even though it is not an ordinary save.
   *
   * Changing it needs its own request: a settings patch could move the key, but
   * it could not say whether that meant "rename" or "delete and create", which
   * differ on whether this agent's conversations and approvals follow. That is a
   * fact about the wire, and it is not a reason to put a second commit button on
   * a screen that already has one: every other box here waits for Save, and one
   * control that did not would be a rule the operator learns by surprise.
   *
   * It also went wrong in a way worth recording. Renaming immediately meant
   * navigating to the new id, which remounts this editor (`key={agentId}` on
   * the route), and every unsaved edit in every other box went with it,
   * silently.
   * One button cannot lose a change it is the one committing.
   */
  const [idDraft, setIdDraft] = useState(agentId);

  const agents = useAgentsQuery();
  const providers = useQuery({
    queryKey: queryKeys.providers,
    queryFn: ({ signal }) => api.providers(signal),
  });
  const models = useQuery({
    queryKey: queryKeys.models,
    queryFn: ({ signal }) => api.models(signal),
  });
  const tools = useQuery({
    queryKey: queryKeys.tools,
    queryFn: ({ signal }) => api.tools(signal),
  });

  const environments = useQuery({
    queryKey: queryKeys.environments,
    queryFn: ({ signal }) => api.environments(signal),
  });

  const resolved = agents.data?.agents.find((agent) => agent.id === agentId);

  const environmentOptions = [
    { value: HOST_ENVIRONMENT, label: t('agents.environmentHost') },
    ...(environments.data?.environments ?? []).map((environment) => ({
      value: environment.name,
      // A definition that did not parse still gets an option: it is installed,
      // an agent may already name it, and the notes below say what is wrong.
      label: environment.definition?.shared
        ? t('agents.environmentOptionShared', { name: environment.name })
        : t('agents.environmentOptionPrivate', { name: environment.name }),
    })),
  ];
  const chosenEnvironment = environments.data?.environments.find(
    (environment) => environment.name === form.environmentName,
  );

  const networkOptions = [
    { value: 'none', label: t('agents.environmentNetworkNone') },
    { value: 'allowlist', label: t('agents.environmentNetworkAllowlist') },
    { value: 'open', label: t('agents.environmentNetworkOpen') },
  ] as const;

  const update = <K extends keyof AgentEntryForm>(
    key: K,
    value: AgentEntryForm[K],
  ): void => {
    setForm((current) => ({ ...current, [key]: value }));
    setDirty(true);
  };

  /** `update`, for the id box, which lives outside the form. */
  const updateIdDraft = (value: string): void => {
    setIdDraft(value);
    setDirty(true);
  };

  /**
   * One box, wired to the form.
   *
   * There is one subtree, so there is one place a field can live and no branch
   * to take. `bind` survives the collapse as the thing that turns a key into the
   * `{value, set}` pair the field components take.
   */
  const bind = (key: StringField): Bound => ({
    value: form[key],
    set: (value) => {
      update(key, value);
    },
  });

  /** `bind`, for the switches. */
  const bindToggle = (key: BooleanField): Toggle => ({
    checked: form[key],
    set: (checked) => {
      update(key, checked);
    },
  });

  const fields = {
    provider: bind('provider'),
    model: bind('model'),
    temperature: bind('temperature'),
    reasoningEffort: bind('reasoningEffort'),
    maxTokens: bind('maxTokens'),
    contextWindowTokens: bind('contextWindowTokens'),
    toolTimeoutSeconds: bind('toolTimeoutSeconds'),
    maxToolIterations: bind('maxToolIterations'),
    loopWallTimeoutSeconds: bind('loopWallTimeoutSeconds'),
    approvalTimeoutSeconds: bind('approvalTimeoutSeconds'),
    maxOutputChars: bind('maxOutputChars'),
    execTimeoutSeconds: bind('execTimeoutSeconds'),
    execMaxOutputKb: bind('execMaxOutputKb'),
  } satisfies Record<string, Bound>;

  const switches = {
    visionEnabled: bindToggle('visionEnabled'),
    toolsEnabled: bindToggle('toolsEnabled'),
    lazyDiscovery: bindToggle('lazyDiscovery'),
    environmentAlwaysUseOwn: bindToggle('environmentAlwaysUseOwn'),
  } satisfies Record<string, Toggle>;

  const raw = form.promptMode === 'raw';

  /** Bumped by a revert, to remount the template editors. See `onRevert`. */
  const [formEpoch, setFormEpoch] = useState(0);

  // A clock or a counter in a raw template: not an error (it is a legitimate
  // thing to want) but a price an operator cannot see on the bill, so it is
  // said here.
  const promptUncacheable =
    raw && VOLATILE.some((hole) => form.systemPrompt.includes(`{{${hole}}}`));

  // Whether the policy would leave the model unable to identify a tool-output
  // fence. `namesDelimiter` is the protocol's, which is what `assertBuildable`
  // asks on the server. So the editor says it before the save rather than the
  // settings response saying it after, and the two cannot drift apart.
  //
  // The delimiter has to be named somewhere, not specifically in the policy. The
  // built-in policy names none on purpose: it is prose that never changes, so
  // it caches, and the live-state section supplies the turn's tag. Both
  // templates have to drop it before the model is left unable to identify a
  // fence.
  const policyUnfenced =
    form.toolPolicyPrompt.trim() !== '' &&
    !namesDelimiter(form.toolPolicyPrompt) &&
    !namesDelimiter(
      form.livePrompt === '' ? DEFAULT_LIVE_STATE_TEMPLATE : form.livePrompt,
    );

  /**
   * Every tool this agent could name, registered or not.
   *
   * The union matters because `agents.list.*` is replaced wholesale on save: a
   * list built only from the live registry would silently drop a tool this
   * agent has an opinion about but whose MCP server happens to be down, and
   * that opinion would be gone the next time anything on this screen was saved.
   *
   */
  const toolNames = useMemo(() => {
    const names = new Set<string>(
      (tools.data?.tools ?? []).map((tool) => tool.name),
    );
    for (const name of Object.keys(form.tools)) {
      names.add(name);
    }
    // The door has its own group and no permission row: see `ToolSearchRow`.
    names.delete(TOOL_SEARCH);
    // `exec` first, then A–Z. Alphabetical put the one tool that runs arbitrary
    // programs on this machine second from the top by accident of spelling, and
    // it is the row an operator opens this section to look at. Everything below
    // it reads a file or writes one inside the jail.
    return [...names].sort((a, b) => {
      if (a === EXEC_TOOL) return -1;
      if (b === EXEC_TOOL) return 1;
      return a.localeCompare(b);
    });
  }, [tools.data, form.tools]);

  const registered = useMemo(
    () => new Map((tools.data?.tools ?? []).map((tool) => [tool.name, tool])),
    [tools.data],
  );

  /**
   * `memory` and `skill`, split out of the list above into a group of their own.
   *
   * They are not the same kind of thing as the rows they sat among. `read`,
   * `exec` and the rest are *actions* an agent takes during a turn, and a
   * permission on one is about that call. These two are the write halves of two
   * whole features, and denying one also removes a section from every prompt on
   * the workspace. So an operator setting `memory` to `deny` in an alphabetical
   * list beside `ls` is making a much larger decision than the row admits.
   *
   * The switches above are the front door for that decision. This group is the
   * same values with the `ask` state and the wording overrides the switches do
   * not offer, which is why both exist and why they are next to each other.
   */
  const featureToolNames = useMemo(
    () => toolNames.filter((name) => FEATURE_TOOLS.has(name)),
    [toolNames],
  );

  /**
   * Everything an MCP server contributed, in a group of its own.
   *
   * Same kind of thing as an action tool (a call the model makes during a turn)
   * but from somewhere else, and that difference is what an operator is
   * reading for. Built-ins are this build's and change when DarkWire is upgraded;
   * these arrive and leave with a server the operator configured, and one of
   * them going missing means "the server is down", not "the tool was removed".
   * Alphabetical order put `mcp_github_search_issues` between `ls` and
   * `read`, where nothing said which of the three was which.
   *
   * `source` first, the name prefix second. See `MCP_TOOL_PREFIX`. Filtered out
   * of the action list below rather than merely added here, so the three groups
   * partition the list instead of overlapping it.
   */
  const mcpToolNames = useMemo(
    () =>
      toolNames.filter(
        (name) =>
          !FEATURE_TOOLS.has(name) &&
          (registered.get(name)?.source === 'mcp' ||
            (!registered.has(name) && name.startsWith(MCP_TOOL_PREFIX))),
      ),
    [toolNames, registered],
  );

  const actionToolNames = useMemo(() => {
    const mcp = new Set(mcpToolNames);
    return toolNames.filter(
      (name) => !FEATURE_TOOLS.has(name) && !mcp.has(name),
    );
  }, [toolNames, mcpToolNames]);

  /**
   * Read from the switch rather than from the saved config, so the list greys
   * out as it is flipped rather than a save later. It is the same value the
   * request is built from, which is what lets this section say something true
   * about the next turn instead of about the last one.
   */
  const toolsOff = !switches.toolsEnabled.checked;

  // Both prompt sections are gated on their own tool rather than on
  // `toolsEnabled`, exactly as `runtime.ts` gates the two contributors. Absent
  // counts as denied there, so it counts as denied here.
  //
  // `ask` reads as on, because it is: the capability is granted and answered per
  // call, and `runtime.ts` places the section for it. Switching off and on again
  // therefore lands on `allow` rather than back on `ask`. The Tools row below
  // is where that distinction is made, and this switch does not pretend to.
  const memoryOff =
    form.tools.memory === undefined || form.tools.memory === 'deny';
  const skillsOff =
    form.tools.skill === undefined || form.tools.skill === 'deny';

  const setToolPermission = (
    name: string,
    permission: ToolPermission,
  ): void => {
    update('tools', { ...form.tools, [name]: permission });
  };

  /**
   * Where the Running commands built-in came from, in one sentence.
   *
   * Two answers rather than three: there is one default text now, so the only
   * question left is whether this environment's definition replaced it. The
   * host and a container whose image is silent both inherit the same wording
   * and get the same sentence.
   */
  const platformHint: WebKey =
    (chosenEnvironment?.definition?.prompt ?? '').trim() === ''
      ? 'agents.promptPlatformHintDefault'
      : 'agents.promptPlatformHintFrom';

  const setPinned = (name: string, pinned: boolean): void => {
    const without = form.pinnedTools.filter((pin) => pin !== name);
    update('pinnedTools', pinned ? [...without, name] : without);
  };

  const setToolPrompt = (name: string, override: ToolPromptOverride): void => {
    // Kept as typed, blanks and all. `pruneToolPrompts` drops the empty ones on
    // the way to the patch. Doing it here instead would delete a row from under
    // the operator the moment they cleared the box to start again.
    update('toolPrompts', { ...form.toolPrompts, [name]: override });
  };

  const setSubagents = (next: readonly SubagentRef[]): void => {
    update('subagents', next);
  };

  const setSubagent = (index: number, next: SubagentRef): void => {
    setSubagents(form.subagents.map((ref, at) => (at === index ? next : ref)));
  };

  /**
   * The agents this one could delegate to.
   *
   * Everything `GET /api/agents` returns except this agent, which is refused at
   * save as a self-reference. Disabled agents are already absent (`listAgents`
   * skips them), which is the same set `assertBuildable` will accept.
   */
  const delegable = useMemo(
    () => (agents.data?.agents ?? []).filter((agent) => agent.id !== agentId),
    [agents.data, agentId],
  );

  const chosenSubagents = useMemo(
    () => new Set(form.subagents.map((ref) => ref.id)),
    [form.subagents],
  );

  const providerOptions = useMemo(() => {
    const instances = (providers.data?.instances ?? []).filter(
      (instance) => instance.enabled,
    );
    return [
      { value: 'auto', label: 'auto: resolve from whichever has credentials' },
      ...instances.map((instance) => ({
        value: instance.id,
        label: instance.displayName === '' ? instance.id : instance.displayName,
      })),
    ];
  }, [providers.data]);

  /**
   * The catalogue for the chosen provider, plus whatever is already pinned.
   *
   * `modelOptions` in `settings/fields.ts` already holds both rules that
   * matter: `auto` offers everything, and the current model is always in the
   * list even when no endpoint advertises it. So a model pinned by hand, or one
   * a provider stopped listing, survives being looked at.
   */
  const modelChoices = useMemo(
    () =>
      modelOptions(
        models.data?.models ?? [],
        fields.provider.value,
        fields.model.value,
      ),
    [models.data, fields.provider.value, fields.model.value],
  );

  /**
   * Changing the provider, and dropping the model when the new one cannot serve it.
   *
   * The model list is per provider, so switching endpoints leaves a pin that
   * may name nothing on the other side. And because `modelOptions` always
   * keeps the current value in the list (so a hand-typed or temporarily
   * unlisted model survives being looked at), the stale pin would go on
   * *looking* valid in the select right up until a turn failed on it.
   *
   * Two cases deliberately do **not** clear it, because in both the honest
   * answer is "unknown" rather than "no":
   *
   *  - **`auto`**, which is not an endpoint but an instruction to resolve one,
   *    so every model is still on the table.
   *  - **An empty catalogue**: the query is still in flight, or every endpoint
   *    is unreachable. Clearing on that would unpin a working model because a
   *    server was briefly down, which is the failure this guard exists to avoid
   *    causing.
   */
  const onProviderChange = (value: string): void => {
    fields.provider.set(value);

    const pinned = fields.model.value;
    if (pinned === '') return;

    const catalogue = models.data?.models ?? [];
    if (catalogue.length === 0) return;

    if (value === 'auto') return;

    // `current` empty, so this is what the new provider actually offers rather
    // than that plus the value being judged.
    if (!modelOptions(catalogue, value, '').includes(pinned)) {
      fields.model.set('');
    }
  };

  /**
   * What the id box would actually produce, and why it might not be allowed.
   *
   * Slugified rather than validated-as-typed: the box accepts a label's worth of
   * typing and the hint below it says what that becomes, which is the same
   * bargain the create dialog makes. `''` means the box holds nothing usable,
   * which is only an error once it differs from the id the agent already has.
   */
  // While creating, an untouched id box follows the name. That is the same
  // bargain the dialog this replaced made, and what the workspace create page
  // does with its folder. Typing in the box takes it over.
  const idSource = creating && idDraft.trim() === '' ? form.label : idDraft;
  const proposedId = idSource.trim() === '' ? '' : deriveAgentId(idSource);
  // Creating is never renaming: there is no old id for the new one to move
  // away from, so the same box means "what this will be called" instead.
  const renaming =
    !creating && !isDefault && proposedId !== '' && proposedId !== agentId;

  const idError = ((): string | undefined => {
    if (creating) {
      if (proposedId === '') return t('agents.idEmpty');
      return list[proposedId] === undefined
        ? undefined
        : t('agents.idTaken', { id: proposedId });
    }
    if (isDefault || idDraft.trim() === agentId) return undefined;
    if (proposedId === '') return t('agents.idEmpty');
    // Checked against the settings tree rather than the agent listing, which
    // omits the disabled ones. Colliding with an agent that is merely switched
    // off is still a collision, and the server would refuse it with a 409.
    if (proposedId !== agentId && list[proposedId] !== undefined) {
      return t('agents.idTaken', { id: proposedId });
    }
    return undefined;
  })();

  const onDelete = (): void => {
    save(toAgentDeletePatch(agentId));
    // Anything pointed at the agent that just went has to move, or the next
    // conversation would name one the server will refuse.
    if (active === agentId) select(DEFAULT_AGENT_ID);
    // Leaving immediately is safe in this direction: the list this returns to
    // does not depend on the agent that is going away.
    void navigate({ to: '/agents' });
  };

  const onSave = (): void => {
    // The id is settled first, because everything below is addressed *to* an id
    // and the patch has to name the one the entry will be under by the time it
    // arrives. A failed rename must therefore leave the settings untouched
    // rather than half-applied to a key that no longer exists.
    if (idError !== undefined) {
      setErrors({ agentId: idError });
      return;
    }

    const target = creating || renaming ? proposedId : agentId;
    const result = toAgentEntryPatch(target, form, entry, t);
    if (!result.ok) {
      setErrors(result.errors);
      return;
    }
    setErrors({});

    if (creating) {
      // The first write this page makes. On success, not on the press: the
      // editor it navigates to reads the settings cache, and arriving before
      // the write lands is the "There is no agent called…" path.
      save(result.patch, {
        onSuccess: () => {
          void navigate({
            to: '/agents/$agentId',
            params: { agentId: target },
          });
        },
      });
      setDirty(false);
      return;
    }

    // One request, patch and rename together. As two it was two writes with a
    // window between them: the rename could land and the patch fail, leaving the
    // agent under its new name holding its old settings.
    //
    // No invalidation on this line: `useSaveSettings` refreshes the agents query
    // once the write has landed, and doing it here fired it *before* the PATCH
    // resolved, so the refetch answered from the old config and a renamed label
    // never reached the composer's picker.
    save(
      renaming
        ? { ...result.patch, renameAgents: [{ from: agentId, to: proposedId }] }
        : result.patch,
      renaming
        ? {
            // Only once the write has landed and the cache holds it. Navigating
            // first lands the editor on an id the settings tree does not have
            // yet. That is the race that renders "There is no agent called…".
            onSuccess: () => {
              // This browser's remembered choice is the one reference the server
              // cannot reach, and nothing else fixes it: the picker only resets
              // an id that names *nothing*, and this one now names the renamed
              // agent.
              if (active === agentId) select(proposedId);
              void navigate({
                to: '/agents/$agentId',
                params: { agentId: proposedId },
              });
            },
          }
        : {},
    );
    setDirty(false);
  };

  const onRevert = (): void => {
    setForm(toAgentEntryForm(entry));
    setIdDraft(agentId);
    // Remounts every `TemplateEditor`, which is what re-derives "does this agent
    // own this template" from the stored value. Without it a revert leaves each
    // box holding the stored (empty) template while still claiming the agent
    // owns one. The state is deliberately not derived from the value, so
    // nothing else would reset it.
    setFormEpoch((epoch) => epoch + 1);
    setErrors({});
    setDirty(false);
  };

  const name =
    form.label === ''
      ? creating
        ? t('agents.newTitle')
        : agentId
      : form.label;

  return {
    agentId,
    creating,
    isDefault,
    name,
    resolved,
    form,
    errors,
    dirty,
    saving,
    formEpoch,
    update,
    fields,
    switches,
    idDraft,
    updateIdDraft,
    proposedId,
    renaming,
    idError,
    models,
    providerOptions,
    modelChoices,
    onProviderChange,
    tools,
    registered,
    actionToolNames,
    mcpToolNames,
    featureToolNames,
    toolsOff,
    memoryOff,
    skillsOff,
    setToolPermission,
    setPinned,
    setToolPrompt,
    delegable,
    chosenSubagents,
    setSubagent,
    setSubagents,
    environments,
    environmentOptions,
    networkOptions,
    chosenEnvironment,
    raw,
    promptUncacheable,
    policyUnfenced,
    platformHint,
    onSave,
    onRevert,
    onDelete,
  };
}
