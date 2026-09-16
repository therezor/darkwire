/**
 * Editing one agent.
 *
 * A route of its own rather than a panel beside a list, because an agent has
 * several sections' worth of settings and a master/detail that put both on one
 * screen made the list a narrow column of nothing and the form a scroll. The
 * list picks; this edits; the back link returns.
 *
 * **The default agent is edited here too, and it is the reason Settings has no
 * "Agent" panel.** It is an entry in `agents.list` like any other, so a second
 * screen for the same fields would be two doors into one room.
 *
 * **It is the same screen, field for field**, with no branch anywhere for which
 * agent is being edited: there is one subtree, so every control writes to the
 * same place.
 *
 * **Nothing on this screen inherits.** Every box holds this agent's own value,
 * and one opened on an agent that stored none is filled from the defaults so
 * that saving writes them down — see the note at the top of `agents-form.ts`.
 * There is no "Inherit — …" option and no empty box that means "ask the other
 * screen": a setting you cannot read off the screen it is on is a setting an
 * operator has to go and derive.
 *
 * **The system prompt is last.** It is the tallest thing on the screen by a
 * wide margin — a full editor holding a page of text — and in the middle it
 * split the short settings into a group above and a group below, so reaching
 * the tools meant scrolling past a document. Everything that fits on a line
 * comes first; the one thing that does not comes after, where it can be as tall
 * as it likes without pushing anything.
 */

import { useQuery } from '@tanstack/react-query';
import { Link, useNavigate, useParams } from '@tanstack/react-router';
import { ArrowLeft, Plus, Trash2, Wrench } from 'lucide-react';
import { useMemo, useState, type JSX } from 'react';
import { useTranslation } from 'react-i18next';

import {
  AgentEntrySchema,
  DEFAULT_AGENT_ID,
  DEFAULT_LIVE_STATE_TEMPLATE,
  DEFAULT_MEMORY_TEMPLATE,
  DEFAULT_SKILLS_TEMPLATE,
  DEFAULT_PLATFORM_CONTAINER_TEMPLATE,
  ENVIRONMENT_PROMPT_PLACEHOLDERS,
  DEFAULT_PLATFORM_HOST_TEMPLATE,
  DEFAULT_SYSTEM_PROMPT_TEMPLATE,
  DEFAULT_TOOL_POLICY_TEMPLATE,
  DEFAULT_WRAP_UP_TEMPLATE,
  deriveAgentId,
  LIVE_PROMPT_PLACEHOLDERS,
  MEMORY_PROMPT_PLACEHOLDERS,
  SKILLS_PROMPT_PLACEHOLDERS,
  PLATFORM_PROMPT_PLACEHOLDERS,
  PROMPT_PLACEHOLDERS,
  RAW_PROMPT_PLACEHOLDERS,
  TOOL_POLICY_PLACEHOLDERS,
  type AgentEntry,
  type SubagentRef,
  type ToolPermission,
  type ToolPromptOverride,
  namesDelimiter,
} from '@ghostwire/protocol';

import { Badge } from '@/components/ui/badge.js';
import { NoticeBlock } from '@/components/ui/notice.js';
import { Button } from '@/components/ui/button.js';
import { DropdownMenuItem } from '@/components/ui/dropdown-menu.js';
import { ConfirmDialog } from '@/components/crud/confirm-dialog.js';
import { RowActions } from '@/components/crud/row-actions.js';
import { api } from '@/lib/api.js';
import { cn } from '@/lib/cn.js';
import { queryKeys } from '@/lib/query.js';
import {
  FieldGrid,
  SaveBar,
  Section,
  SelectField,
  SwitchRow,
  TextField,
} from '@/components/form/controls.js';
import { modelOptions } from '@/components/form/fields.js';
import { useSaveSettings, useSettings } from '@/settings/use-settings.js';
import {
  REASONING_EFFORTS,
  UNSET_VALUE,
  toAgentDeletePatch,
  toAgentEntryForm,
  toAgentEntryPatch,
  type AgentEntryForm,
} from './agents-form.js';
import { useAgent } from './agent-context.js';
import { SubagentRow } from './subagent-row.js';
import { TemplateEditor } from './template-editor.js';
import { ToolRow, parameterFields } from './tool-row.js';

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

/** One box on the form. */
interface Bound {
  readonly value: string;
  readonly set: (value: string) => void;
}

/** The same, for a field that is on or off. */
interface Toggle {
  readonly checked: boolean;
  readonly set: (checked: boolean) => void;
}

/**
 * A select whose blank is a real answer.
 *
 * `reasoningEffort` unset means the request carries no such parameter — not "no
 * value chosen" — so it may not render as a blank trigger. `unsetLabel` is the
 * sentence that says what the blank does.
 */
function OptionalSelect({
  label,
  bound,
  options,
  unsetLabel,
}: {
  readonly label: string;
  readonly bound: Bound;
  readonly options: readonly string[];
  readonly unsetLabel: string;
}): JSX.Element {
  return (
    <SelectField
      label={label}
      value={bound.value === '' ? UNSET_VALUE : bound.value}
      options={[
        { value: UNSET_VALUE, label: unsetLabel },
        ...options.map((option) => ({ value: option, label: option })),
      ]}
      onValueChange={(next) => {
        bound.set(next === UNSET_VALUE ? '' : next);
      }}
    />
  );
}

/** A text field over whichever subtree this agent keeps the setting in. */
function BoundField({
  label,
  bound,
  error,
  hint,
  inputMode,
  placeholder,
}: {
  readonly label: string;
  readonly bound: Bound;
  readonly error?: string | undefined;
  readonly hint?: string;
  readonly inputMode?: 'numeric' | 'decimal';
  readonly placeholder?: string;
}): JSX.Element {
  return (
    <TextField
      label={label}
      value={bound.value}
      {...(error === undefined ? {} : { error })}
      {...(hint === undefined ? {} : { hint })}
      {...(inputMode === undefined ? {} : { inputMode })}
      {...(placeholder === undefined ? {} : { placeholder })}
      onValueChange={bound.set}
    />
  );
}

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

/**
 * The tool pinned to the top of the list.
 *
 * A name rather than a risk band: `exec` is the specific tool that runs a
 * program, and pinning the whole `exec` band would float every MCP tool that
 * shares it, which is the second list, in its own group, where source order is
 * the useful order.
 */
const EXEC_TOOL = 'exec';

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
 * is `write_file`, which is squarely an action tool.
 */
const FEATURE_TOOLS: ReadonlySet<string> = new Set(['memory', 'skill']);

/**
 * The prefix `flattenToolName` puts on every tool an MCP server contributes.
 *
 * A fallback for the rows the registry cannot describe. `ToolDefinition.source`
 * is the answer whenever the tool is registered, and it is the one used first —
 * but `toolNames` deliberately includes tools this agent has an opinion about
 * whose server is down, and those have no definition at all. Without this they
 * would sit in the action list wearing a "not installed" badge, which is the
 * mixing this grouping exists to undo, on the rows most likely to be looked at.
 *
 * A prefix test and nothing more. `mcp_{server}_{tool}` cannot be parsed back —
 * `sanitise` collapses characters and a long name is truncated onto a digest —
 * so this asks only whether a name came from a server, never which one.
 */
const MCP_TOOL_PREFIX = 'mcp_';

/**
 * The dropdown's value for the host.
 *
 * **The host is an environment, not the absence of one.** It is where commands
 * run when no definition is named, and the prompt says so in its own words. The
 * config spells it as an empty name because there is no definition file to
 * point at, but nothing above that layer should read it as "none chosen".
 *
 * A `SelectItem` may not carry an empty value — Radix reserves it for "nothing
 * chosen", which is exactly the meaning to avoid here, so this option needs a
 * value of its own. It begins with `-`, which cannot collide with an installed
 * environment name.
 */
const HOST_ENVIRONMENT = '-host-';

/**
 * Creating an agent, on the page that edits one.
 *
 * The same `Editor`, seeded from the default agent's entry — which is exactly
 * what the dialog it replaced used as its template, only now the operator sees
 * it and can change it *before* anything is written. Nothing reaches the
 * settings tree until Save, so an abandoned create leaves no agent behind.
 */
export function AgentCreateRoute(): JSX.Element {
  const { t } = useTranslation();
  const settings = useSettings();

  if (settings.isPending) {
    return <p className="page__note">{t('agents.loading')}</p>;
  }
  if (settings.isError) {
    return (
      <p role="alert" className="page__error">
        {t('agents.loadDefaultsError', { message: settings.error.message })}
      </p>
    );
  }

  const { config } = settings.data;
  // The template a new agent is stamped from, and the same one `toNewAgentPatch`
  // used: the default agent as it actually stands, not the schema's defaults.
  const template =
    config.agents.list[DEFAULT_AGENT_ID] ?? AgentEntrySchema.parse({});

  return (
    <Editor
      mode="create"
      agentId=""
      entry={template}
      list={config.agents.list}
    />
  );
}

export function AgentEditorRoute(): JSX.Element {
  const { t } = useTranslation();
  const { agentId } = useParams({ from: '/agents/$agentId' });
  const settings = useSettings();

  if (settings.isPending) {
    return <p className="page__note">{t('agents.loading')}</p>;
  }
  if (settings.isError) {
    return (
      <p role="alert" className="page__error">
        {t('agents.loadOneError', { message: settings.error.message })}
      </p>
    );
  }

  const { config } = settings.data;
  const isDefault = agentId === DEFAULT_AGENT_ID;
  const entry = config.agents.list[agentId];

  // A named agent that is not in the settings is a stale link — a bookmark to
  // one that was deleted, or a hand-typed id. Saying so beats an empty form
  // that silently creates it on the first save.
  if (entry === undefined && !isDefault) {
    return (
      <div className="stack page page--wide">
        <p role="alert" className="page__error">
          {t('agents.noSuchAgent', { id: agentId })}
        </p>
        <Link to="/agents" className="page__back">
          <ArrowLeft aria-hidden="true" />
          {t('agents.backToAgents')}
        </Link>
      </div>
    );
  }

  return (
    <Editor
      // Remounts on a change of agent, so one agent's edits cannot survive into
      // the next one's boxes.
      key={agentId}
      mode="edit"
      agentId={agentId}
      entry={entry ?? AgentEntrySchema.parse({})}
      // The whole list, not the `/api/agents` listing: that one omits the
      // disabled agents, and a rename onto a switched-off agent's id is still
      // the collision the server refuses with a 409.
      list={config.agents.list}
    />
  );
}

function Editor({
  mode,
  agentId,
  entry,
  list,
}: {
  /**
   * `create` seeds from the template and POSTs the agent into existence on
   * Save; `edit` loads the stored entry and patches it. It decides three things
   * and nothing else — the seed, what Save does, and whether the edit-only
   * controls render (Delete, and the id box's rename behaviour).
   */
  readonly mode: 'create' | 'edit';
  readonly agentId: string;
  readonly entry: AgentEntry;
  readonly list: Readonly<Record<string, AgentEntry>>;
}): JSX.Element {
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
  const [confirmingDelete, setConfirmingDelete] = useState(false);
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
   * navigating to the new id, which remounts this editor — `key={agentId}` on
   * the route — and every unsaved edit in every other box went with it, silently.
   * One button cannot lose a change it is the one committing.
   */
  const [idDraft, setIdDraft] = useState(agentId);

  const agents = useQuery({
    queryKey: queryKeys.agents,
    queryFn: ({ signal }) => api.agents(signal),
  });
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
      label: environment.shared
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
  } satisfies Record<string, Bound>;

  const switches = {
    visionEnabled: bindToggle('visionEnabled'),
    toolsEnabled: bindToggle('toolsEnabled'),
  } satisfies Record<string, Toggle>;

  // ── The prompt ───────────────────────────────────────────────────────────
  //
  // Six templates, one component. Empty means "use the built-in", so each box
  // shows the built-in and typing into it is what makes the wording this
  // agent's own — an operator cannot choose to edit something they have never
  // been shown. See `TemplateEditor` for the three states each one holds.
  //
  // Nothing here is hidden behind a "show advanced" toggle. The point of the
  // feature is that the prompt an install runs on is one an operator can read;
  // a section they have to go looking for is one they will not know exists.
  const raw = form.promptMode === 'raw';

  /** Bumped by a revert, to remount the template editors. See `onRevert`. */
  const [formEpoch, setFormEpoch] = useState(0);

  // A clock or a counter in a raw template: not an error — it is a legitimate
  // thing to want — but a price an operator cannot see on the bill, so it is
  // said here.
  const promptUncacheable =
    raw && VOLATILE.some((hole) => form.systemPrompt.includes(`{{${hole}}}`));

  // Whether the policy would leave the model unable to identify a tool-output
  // fence. `namesDelimiter` is the protocol's, which is what `assertBuildable`
  // asks on the server — so the editor says it before the save rather than the
  // settings response saying it after, and the two cannot drift apart.
  //
  // The delimiter has to be named somewhere, not specifically in the policy. The
  // built-in policy names none on purpose — it is prose that never changes, so it
  // caches, and the live-state section supplies the turn's tag. Both templates
  // have to drop it before the model is left unable to identify a fence.
  const policyUnfenced =
    form.toolPolicyPrompt.trim() !== '' &&
    !namesDelimiter(form.toolPolicyPrompt) &&
    !namesDelimiter(
      form.livePrompt === '' ? DEFAULT_LIVE_STATE_TEMPLATE : form.livePrompt,
    );

  // ── Tools ────────────────────────────────────────────────────────────────
  //
  // One row per tool, one control on it. Enabling a tool and choosing what
  // happens when it runs are the same act — `deny` is the off position — so
  // there is no switch beside the select and no mode toggle above the list.

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
   * They are not the same kind of thing as the rows they sat among. `read_file`,
   * `exec` and the rest are *actions* an agent takes during a turn, and a
   * permission on one is about that call. These two are the write halves of two
   * whole features, and denying one also removes a section from every prompt on
   * the workspace — so an operator setting `memory` to `deny` in an alphabetical
   * list beside `list_dir` is making a much larger decision than the row admits.
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
   * Same kind of thing as an action tool — a call the model makes during a turn
   * — but from somewhere else, and that difference is what an operator is
   * reading for. Built-ins are this build's and change when GhostAI is upgraded;
   * these arrive and leave with a server the operator configured, and one of
   * them going missing means "the server is down", not "the tool was removed".
   * Alphabetical order put `mcp_github_search_issues` between `list_dir` and
   * `read_file`, where nothing said which of the three was which.
   *
   * `source` first, the name prefix second — see `MCP_TOOL_PREFIX`. Filtered out
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
  // therefore lands on `allow` rather than back on `ask` — the Tools row below
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

  const setToolPrompt = (name: string, override: ToolPromptOverride): void => {
    // Kept as typed, blanks and all. `pruneToolPrompts` drops the empty ones on
    // the way to the patch — doing it here instead would delete a row from under
    // the operator the moment they cleared the box to start again.
    update('toolPrompts', { ...form.toolPrompts, [name]: override });
  };

  /**
   * One row, so the two groups below cannot drift apart.
   *
   * They render the same control over the same state and differ only in which
   * heading they sit under — which is exactly the case where two copies of the
   * JSX end up with two sets of props.
   */
  const toolRow = (name: string): JSX.Element => {
    const tool = registered.get(name);
    return (
      <ToolRow
        key={name}
        name={name}
        detail={tool?.description ?? ''}
        risk={tool?.risk}
        permission={form.tools[name] ?? 'deny'}
        fields={tool === undefined ? [] : parameterFields(tool.parameters)}
        override={form.toolPrompts[name]}
        disabled={toolsOff}
        onChange={(next) => {
          setToolPermission(name, next);
        }}
        onOverrideChange={(next) => {
          setToolPrompt(name, next);
        }}
      />
    );
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
   * save as a self-reference. Disabled agents are already absent — `listAgents`
   * skips them — which is the same set `assertBuildable` will accept.
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
      { value: 'auto', label: 'auto — resolve from whichever has credentials' },
      ...instances.map((instance) => ({
        value: instance.id,
        label: instance.displayName === '' ? instance.id : instance.displayName,
      })),
    ];
  }, [providers.data]);

  /**
   * The catalogue for the chosen provider, plus whatever is already pinned.
   *
   * `modelOptions` in `settings/fields.ts` already holds both rules that matter
   * — `auto` offers everything, and the current model is always in the list
   * even when no endpoint advertises it — so a model pinned by hand, or one a
   * provider stopped listing, survives being looked at.
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
   * may name nothing on the other side — and because `modelOptions` always
   * keeps the current value in the list (so a hand-typed or temporarily
   * unlisted model survives being looked at), the stale pin would go on
   * *looking* valid in the select right up until a turn failed on it.
   *
   * Two cases deliberately do **not** clear it, because in both the honest
   * answer is "unknown" rather than "no":
   *
   *  - **`auto`**, which is not an endpoint but an instruction to resolve one,
   *    so every model is still on the table.
   *  - **An empty catalogue** — the query is still in flight, or every endpoint
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
  // While creating, an untouched id box follows the name — the same bargain the
  // dialog this replaced made, and what the workspace create page does with its
  // folder. Typing in the box takes it over.
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
    // omits the disabled ones — colliding with an agent that is merely switched
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
      // The first write this page makes. On success, not on the press — the
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
    // resolved — the refetch answered from the old config and a renamed label
    // never reached the composer's picker.
    save(
      renaming
        ? { ...result.patch, renameAgents: [{ from: agentId, to: proposedId }] }
        : result.patch,
      renaming
        ? {
            // Only once the write has landed and the cache holds it. Navigating
            // first lands the editor on an id the settings tree does not have
            // yet — the race that renders "There is no agent called…".
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
    // owns one — the state is deliberately not derived from the value, so
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

  return (
    <div className="stack page page--wide agent-editor">
      <div className="editor__head">
        <Link to="/agents" className="page__back">
          <ArrowLeft aria-hidden="true" />
          Agents
        </Link>

        <div className="cluster editor__title">
          <h1 className="page__title">{name}</h1>
          {isDefault && <Badge>default</Badge>}
          <span className="spacer" />
          {/* Not a section at the bottom of the form any more. A destructive
              action does not belong in the reading order of the settings it
              would destroy, and it used to fire without asking. Absent while
              creating: there is nothing yet to delete. */}
          {!isDefault && !creating && (
            <RowActions label={name}>
              <DropdownMenuItem
                className="menu__item--danger"
                onSelect={() => {
                  setConfirmingDelete(true);
                }}
              >
                <Trash2 />
                {t('agents.deleteAgent')}
              </DropdownMenuItem>
            </RowActions>
          )}
        </div>

        <p className="page__note">
          {/* A switched-off agent is absent from `/api/agents` just as a
              deleted one is, so `resolved` being undefined cannot on its own
              mean "no model". Asked in this order, the disabled case answers
              for itself and the model line is only reached by an agent that
              could actually run — otherwise a disabled agent was told it had
              no model, and choosing one would not have helped. */}
          {isDefault
            ? 'Every session that names no agent runs on this one, and a new agent starts as a copy of it.'
            : !form.enabled
              ? 'Switched off: it cannot take a turn, and it is hidden from the picker. Its settings and its sessions are kept.'
              : `Runs on ${resolved?.model === '' || resolved === undefined ? 'no model yet — it cannot take a turn until one is chosen' : resolved.model}.`}
        </p>
      </div>

      <Section
        title={t('agents.identity')}
        description={t('agents.identityDesc')}
      >
        <FieldGrid>
          <TextField
            label={t('common.name')}
            value={form.label}
            placeholder={agentId}
            onValueChange={(value) => {
              update('label', value);
            }}
            hint={t('agents.labelHint', { token: '{{name}}' })}
          />
          {!isDefault && (
            <TextField
              label={t('agents.idLabel')}
              value={idDraft}
              {...(creating ? { placeholder: proposedId } : {})}
              onValueChange={(value) => {
                setIdDraft(value);
                setDirty(true);
              }}
              {...(errors.agentId === undefined || creating
                ? {}
                : { error: errors.agentId })}
              hint={
                // Creating shows what the id *will be* as it is typed, and says
                // so inline rather than as an error — a name that collides is a
                // thing to fix, not a failure that has happened.
                creating
                  ? (idError ?? t('agents.idCreatePreview', { id: proposedId }))
                  : renaming
                    ? t('agents.idPreview', { id: proposedId })
                    : t('agents.idHint')
              }
            />
          )}
          {!isDefault && (
            <SwitchRow
              label={t('agents.enabled')}
              hint={t('agents.enabledHint')}
              checked={form.enabled}
              onCheckedChange={(checked) => {
                update('enabled', checked);
              }}
            />
          )}
        </FieldGrid>
      </Section>

      <Section
        title={t('agents.modelSection')}
        description={
          isDefault
            ? 'What this agent runs on, and what a new agent is created holding.'
            : 'What this agent runs on.'
        }
      >
        <FieldGrid>
          <SelectField
            label={t('agents.provider')}
            value={fields.provider.value}
            options={providerOptions}
            onValueChange={onProviderChange}
            error={errors.provider}
          />
          {/* No "resolved automatically" option, because there is no such
              resolution: an empty model is `noModelError` and a `null` provider
              in the runtime, so offering it here dressed an unconfigured
              install up as a choice. Blank is a placeholder now — a question
              the form asks — and saving without answering it is refused. */}
          <SelectField
            label={t('agents.model')}
            value={fields.model.value}
            placeholder={
              modelChoices.length === 0
                ? 'No models to choose from'
                : 'Choose a model'
            }
            options={modelChoices.map((model) => ({
              value: model,
              label: model,
            }))}
            onValueChange={fields.model.set}
            error={errors.model}
            hint={
              modelChoices.length === 0
                ? 'Nothing could be listed. Add an endpoint and its credentials in Settings → Providers.'
                : models.isError
                  ? 'The model lists could not be fetched. Anything already pinned is still offered.'
                  : 'Endpoints that list their own models are enumerated live.'
            }
          />
          <BoundField
            label={t('agents.temperature')}
            bound={fields.temperature}
            inputMode="decimal"
            error={errors.temperature}
            placeholder={t('agents.providerDefault')}
            hint={t('agents.temperatureHint')}
          />
          {/* The same sentence as the temperature placeholder beside it, and
              the same one `/effort` shows: a blank here means this agent sends
              no reasoning parameter, so the provider applies its own. There is
              nothing above an agent for it to mean anything else. */}
          <OptionalSelect
            label={t('agents.reasoningEffort')}
            bound={fields.reasoningEffort}
            options={REASONING_EFFORTS}
            unsetLabel={t('agents.providerDefault')}
          />
          {/* Below the model rather than beside the toolset, because that is
              what they are about: what this model can be asked to do, not what
              this agent is allowed to do. Both default on, so an operator only
              comes here once they have met a model that cannot keep up. */}
          <SwitchRow
            label={t('agents.vision')}
            hint={t('agents.visionHint')}
            checked={switches.visionEnabled.checked}
            onCheckedChange={switches.visionEnabled.set}
          />
          <SwitchRow
            label={t('agents.toolsEnabled')}
            hint={t('agents.toolsEnabledHint')}
            checked={switches.toolsEnabled.checked}
            onCheckedChange={switches.toolsEnabled.set}
          />
        </FieldGrid>
      </Section>

      <Section
        title={t('agents.toolsSection')}
        description={t('agents.toolsDesc')}
      >
        {tools.isPending && (
          <p className="page__note">{t('agents.loadingTools')}</p>
        )}
        {/* Above the list rather than in place of it. The rows say what this
            agent is configured to do, and that is still true and still saved —
            what has changed is only that this model is not being told about any
            of it. Emptying the list would read as the config being gone, which
            is the one thing the switch must not do. */}
        {toolsOff && (
          <NoticeBlock
            tone="warning"
            icon={Wrench}
            title={t('agents.toolsOffTitle')}
            message={t('agents.toolsOffNote')}
          />
        )}
        {actionToolNames.length > 0 && (
          <ul
            className={cn(
              'stack agent-editor__tools',
              toolsOff && 'agent-editor__tools--off',
            )}
          >
            {actionToolNames.map((name) => toolRow(name))}
          </ul>
        )}

        {/* Directly under the action list, because these are action tools —
            calls the model makes during a turn, under the same permissions.
            What separates them is where they came from, which is the thing an
            operator is scanning for when a row is missing or unfamiliar. Not
            split per server: `localeCompare` already clusters `mcp_github_*`
            ahead of `mcp_linear_*`, so the list reads by server for free. */}
        {mcpToolNames.length > 0 && (
          <>
            <h3 className="agent-editor__tool-group">
              {t('agents.mcpToolsGroup')}
            </h3>
            <p className="page__note">{t('agents.mcpToolsNote')}</p>
            <ul
              className={cn(
                'stack agent-editor__tools',
                toolsOff && 'agent-editor__tools--off',
              )}
            >
              {mcpToolNames.map((name) => toolRow(name))}
            </ul>
          </>
        )}

        {/* Below the action tools rather than above them, and under the same
            kind of heading the environment group uses. An operator scanning for
            "what may this agent do in a turn" reads the list above; these two
            are read when the question is "does this agent have memory at all",
            which is the question the switches near the top answer. */}
        {featureToolNames.length > 0 && (
          <>
            <h3 className="agent-editor__tool-group">
              {t('agents.featureToolsGroup')}
            </h3>
            <p className="page__note">{t('agents.featureToolsNote')}</p>
            <ul
              className={cn(
                'stack agent-editor__tools',
                toolsOff && 'agent-editor__tools--off',
              )}
            >
              {featureToolNames.map((name) => toolRow(name))}
            </ul>
          </>
        )}
      </Section>

      {/* After the tools, because delegating *is* a tool from the model's side —
          one per subagent, named after it — and an operator reading down the
          page has just decided what this agent may do on its own. */}
      <Section
        title={t('agents.subagentsSection')}
        description={t('agents.subagentsDesc')}
      >
        {delegable.length === 0 ? (
          <p className="page__note">{t('agents.subagentsNoneAvailable')}</p>
        ) : (
          <>
            {form.subagents.length === 0 && (
              <p className="page__note">{t('agents.subagentsEmpty')}</p>
            )}

            <ul className="stack agent-editor__subagents">
              {form.subagents.map((ref, index) => (
                <SubagentRow
                  // By position, not by id: a row the operator has not filled in
                  // yet has no id, and two of them would collide on `''` — which
                  // React resolves by reusing one input for both.
                  key={index}
                  subagentRef={ref}
                  index={index}
                  // The agent itself is never offered, and neither is one already
                  // chosen — both are refused at save, and a picker that offers
                  // what the save refuses teaches the wrong thing.
                  options={delegable.filter(
                    (agent) =>
                      agent.id === ref.id || !chosenSubagents.has(agent.id),
                  )}
                  // The form's value, not the saved one, so the hint follows
                  // the environment picker further down the page as it moves.
                  callerEnvironment={form.environmentName}
                  onChange={(next) => {
                    setSubagent(index, next);
                  }}
                  onRemove={() => {
                    setSubagents(
                      form.subagents.filter((unused, at) => at !== index),
                    );
                  }}
                />
              ))}
            </ul>

            <Button
              variant="secondary"
              disabled={form.subagents.length >= delegable.length}
              onClick={() => {
                setSubagents([
                  ...form.subagents,
                  {
                    id: '',
                    prompt: '',
                    permission: 'allow',
                    inheritEnvironment: true,
                  },
                ]);
              }}
            >
              <Plus aria-hidden="true" />
              {t('agents.subagentAdd')}
            </Button>
          </>
        )}
      </Section>

      {/* After the tools, because an environment decides where exec runs. */}
      <Section
        title={t('agents.environmentSection')}
        description={t('agents.environmentDesc')}
      >
        {environments.isPending && (
          <p className="page__note">{t('agents.environmentLoading')}</p>
        )}
        {environments.data?.environments.length === 0 && (
          <p className="page__note">{t('agents.environmentNoProfiles')}</p>
        )}

        <FieldGrid>
          <SelectField
            label={t('agents.environmentProfile')}
            value={
              form.environmentName === ''
                ? HOST_ENVIRONMENT
                : form.environmentName
            }
            onValueChange={(value) => {
              update(
                'environmentName',
                value === HOST_ENVIRONMENT ? '' : value,
              );
            }}
            options={environmentOptions}
          />
          {/* Only with an environment. Egress is enforced by its
              gateway, so offering the control without one would be offering a
              setting the save refuses. */}
          {chosenEnvironment !== undefined && (
            <SelectField
              label={t('agents.environmentNetwork')}
              value={form.environmentNetworkMode}
              onValueChange={(value) => {
                update('environmentNetworkMode', value);
              }}
              options={networkOptions}
            />
          )}
        </FieldGrid>

        {/* Only the states an operator has to act on. A manifest that parses
            needs no line of its own. */}
        {chosenEnvironment !== undefined && (
          <p className="page__note">
            {chosenEnvironment.shared
              ? t('agents.environmentShared', { name: chosenEnvironment.name })
              : t('agents.environmentPrivate', {
                  name: chosenEnvironment.name,
                })}
          </p>
        )}
        {chosenEnvironment?.problem !== undefined && (
          <p className="page__note">{chosenEnvironment.problem}</p>
        )}
        {chosenEnvironment !== undefined &&
          chosenEnvironment.weakened.length > 0 && (
            <p className="page__note">
              {t('agents.environmentWeakened', {
                what: chosenEnvironment.weakened.join(', '),
              })}
            </p>
          )}
        {/* Resolved by the server from the definition's own uid, privileges and
            capabilities, so an operator learns a restricted allow-list is
            impossible here while they are still choosing rather than on save. */}
        {chosenEnvironment?.gatewayProblem !== undefined &&
          form.environmentNetworkMode === 'allowlist' && (
            <p className="page__note">
              {t('agents.environmentGatewayProblem', {
                why: chosenEnvironment.gatewayProblem,
              })}
            </p>
          )}
        {chosenEnvironment !== undefined &&
          form.environmentNetworkMode === 'allowlist' && (
            <>
              <TextField
                label={t('agents.environmentAllow')}
                value={form.environmentAllow}
                onValueChange={(value) => {
                  update('environmentAllow', value);
                }}
                hint={t('agents.environmentAllowHint')}
              />
              <TextField
                label={t('agents.environmentHosts')}
                value={form.environmentHosts}
                onValueChange={(value) => {
                  update('environmentHosts', value);
                }}
                hint={t('agents.environmentHostsHint')}
              />
              <TextField
                label={t('agents.environmentDns')}
                value={form.environmentDns}
                onValueChange={(value) => {
                  update('environmentDns', value);
                }}
                hint={t('agents.environmentDnsHint')}
              />
            </>
          )}
      </Section>

      {/* Below the tools and above the prompt, in plain sight. It sat behind a
          "Show limits" press on the theory that nobody changes a budget twice a
          year — but the numbers are now this agent's own rather than inherited,
          and a setting an operator has to go looking for to find out what it
          says is not one they can be said to have chosen. */}
      <Section title={t('agents.limits')} description={t('agents.limitsDesc')}>
        <FieldGrid>
          <BoundField
            label={t('agents.maxOutputTokens')}
            bound={fields.maxTokens}
            inputMode="numeric"
            error={errors.maxTokens}
          />
          <BoundField
            label={t('agents.contextWindow')}
            bound={fields.contextWindowTokens}
            inputMode="numeric"
            error={errors.contextWindowTokens}
          />
          <BoundField
            label={t('agents.toolTimeout')}
            bound={fields.toolTimeoutSeconds}
            inputMode="numeric"
            error={errors.toolTimeoutSeconds}
            hint={t('agents.zeroDisablesHint')}
          />
          {/* Per agent, like everything beside them. These two were the
              default agent's alone while they lived in a subtree of their own;
              a tool budget is as much a property of one agent as its model. */}
          <BoundField
            label={t('agents.maxToolIterations')}
            bound={fields.maxToolIterations}
            inputMode="numeric"
            error={errors.maxToolIterations}
          />
          <BoundField
            label={t('agents.turnTimeout')}
            bound={fields.loopWallTimeoutSeconds}
            inputMode="numeric"
            error={errors.loopWallTimeoutSeconds}
            hint={t('agents.zeroDisablesHint')}
          />
        </FieldGrid>
      </Section>

      {/* Before the prompt section, which is where both of these end up.

          These two switches *are* the `memory` and `skill` rows in Tools below —
          they write the same `allow`/`deny`, and the row moves when the switch
          does. That is deliberate, and is why there is no second config key: a
          boolean beside the permission would be a way for the two to disagree,
          and the permission is the one the runtime already gates the prompt
          section on.

          What they buy over the rows is that the rows do not look like feature
          switches. `memory` and `skill` sit in an alphabetical list of file and
          shell tools, where "this agent does not remember" reads as one denied
          call rather than as the whole capability being off. */}
      <Section
        title={t('agents.knowledge')}
        description={t('agents.knowledgeDesc')}
      >
        <FieldGrid>
          <SwitchRow
            label={t('agents.memoryEnabled')}
            hint={t('agents.memoryEnabledHint')}
            checked={!memoryOff}
            onCheckedChange={(next) => {
              setToolPermission('memory', next ? 'allow' : 'deny');
            }}
          />
          <SwitchRow
            label={t('agents.skillsEnabled')}
            hint={t('agents.skillsEnabledHint')}
            checked={!skillsOff}
            onCheckedChange={(next) => {
              setToolPermission('skill', next ? 'allow' : 'deny');
            }}
          />
        </FieldGrid>
      </Section>

      <Section
        title={t('agents.systemPrompt')}
        description={t('agents.promptDesc')}
      >
        <div className="stack agent-editor__prompt">
          <TemplateEditor
            key={`${String(formEpoch)}-system`}
            name={name}
            label="agents.promptSystem"
            builtIn={DEFAULT_SYSTEM_PROMPT_TEMPLATE}
            value={form.systemPrompt}
            placeholders={raw ? RAW_PROMPT_PLACEHOLDERS : PROMPT_PLACEHOLDERS}
            removable={false}
            hint={
              raw ? 'agents.promptSystemRawHint' : 'agents.promptSystemHint'
            }
            {...(promptUncacheable
              ? {
                  // Not an error: a clock in the prompt is a legitimate thing to
                  // want. It is a price, and one an operator cannot see on the
                  // bill, so it is said here instead.
                  warning: {
                    title: t('agents.promptUncacheableTitle'),
                    message: t('agents.promptUncacheable'),
                  },
                }
              : {})}
            onChange={(next) => {
              update('systemPrompt', next);
            }}
          />

          {/* A disclosure, not a second mode picker. The sections below are the
              rest of the prompt, and an operator who has not gone looking for
              them should not have to answer a question about them first — the
              old "Assembly: Sections / Raw" select made a rare decision the
              opening move of an ordinary screen.

              `<details>` rather than a button and state: it is what the element
              is for, and the keyboard behaviour and `aria-expanded` come out
              correct without being written. */}
          <details className="agent-editor__advanced">
            <summary>{t('agents.promptAdvanced')}</summary>
            <div className="stack agent-editor__advanced-body">
              {/* Phrased as what it does rather than as a mode with a name. An
                  operator reaching for this wants "stop adding things to my
                  prompt", which is a behaviour; `raw` is our word for it. */}
              <SwitchRow
                label={t('agents.promptOnlySystem')}
                hint={t('agents.promptOnlySystemHint')}
                checked={raw}
                onCheckedChange={(next) => {
                  update('promptMode', next ? 'raw' : 'template');
                }}
              />

              {/* Hidden rather than disabled: nothing places them, so a box that
                  still edited one would be a control with no effect on screen.
                  The stored values survive — switching back restores them. */}
              {!raw && (
                <>
                  <TemplateEditor
                    key={`${String(formEpoch)}-live`}
                    name={name}
                    label="agents.promptLive"
                    builtIn={DEFAULT_LIVE_STATE_TEMPLATE}
                    value={form.livePrompt}
                    placeholders={LIVE_PROMPT_PLACEHOLDERS}
                    hint="agents.promptLiveHint"
                    onChange={(next) => {
                      update('livePrompt', next);
                    }}
                  />
                  <TemplateEditor
                    key={`${String(formEpoch)}-wrapup`}
                    name={name}
                    label="agents.promptWrapUp"
                    builtIn={DEFAULT_WRAP_UP_TEMPLATE}
                    value={form.wrapUpPrompt}
                    placeholders={['iterationsLeft']}
                    hint="agents.promptWrapUpHint"
                    onChange={(next) => {
                      update('wrapUpPrompt', next);
                    }}
                  />
                  <TemplateEditor
                    key={`${String(formEpoch)}-platform`}
                    name={name}
                    label="agents.promptPlatform"
                    builtIn={
                      form.environmentName.trim() === ''
                        ? DEFAULT_PLATFORM_HOST_TEMPLATE
                        : DEFAULT_PLATFORM_CONTAINER_TEMPLATE
                    }
                    value={form.platformPrompt}
                    placeholders={PLATFORM_PROMPT_PLACEHOLDERS}
                    hint="agents.promptPlatformHint"
                    // Tool-shaped like the two below it: every line it renders
                    // describes running a command, and the file tools it names
                    // are tools too. With none of them there is nothing left for
                    // the section to be about.
                    {...(toolsOff
                      ? {
                          warning: {
                            title: t('agents.toolsOffTitle'),
                            message: t('agents.promptNotPlacedNoTools'),
                          },
                        }
                      : {})}
                    onChange={(next) => {
                      update('platformPrompt', next);
                    }}
                  />
                  {/* After the command policy, which says *where* commands
                      run: this says what is *there*. Its built-in is the chosen
                      environment's own `prompt` rather than anything this repo
                      ships. Nobody but the operator knows what is installed in
                      an image, so an agent on the host has nothing to inherit
                      and the section is simply not placed. */}
                  <TemplateEditor
                    key={`${String(formEpoch)}-environment`}
                    name={name}
                    label="agents.promptEnvironment"
                    builtIn={chosenEnvironment?.prompt ?? ''}
                    value={form.environmentPrompt}
                    placeholders={ENVIRONMENT_PROMPT_PLACEHOLDERS}
                    hint="agents.promptEnvironmentHint"
                    {...(toolsOff
                      ? {
                          warning: {
                            title: t('agents.toolsOffTitle'),
                            message: t('agents.promptNotPlacedNoTools'),
                          },
                        }
                      : {})}
                    onChange={(next) => {
                      update('environmentPrompt', next);
                    }}
                  />
                  <TemplateEditor
                    key={`${String(formEpoch)}-policy`}
                    name={name}
                    label="agents.promptToolPolicy"
                    builtIn={DEFAULT_TOOL_POLICY_TEMPLATE}
                    value={form.toolPolicyPrompt}
                    placeholders={TOOL_POLICY_PLACEHOLDERS}
                    hint="agents.promptToolPolicyHint"
                    // Ahead of the two delimiter warnings, and it replaces them:
                    // neither says anything while the section is not placed, and
                    // a box carrying three lines about a prompt nothing sends is
                    // three chances to act on the wrong one.
                    {...(toolsOff
                      ? {
                          warning: {
                            title: t('agents.toolsOffTitle'),
                            message: t('agents.promptNotPlacedNoTools'),
                          },
                        }
                      : policyUnfenced
                        ? {
                            warning: {
                              title: t('agents.promptToolPolicyUnfencedTitle'),
                              // Passed rather than written into the bundle:
                              // i18next does not rescan an interpolated value,
                              // which is the only way a literal `{{…}}` survives
                              // to the screen.
                              message: t('agents.promptToolPolicyUnfenced', {
                                tag: '{{tag}}',
                                nonce: '{{nonce}}',
                              }),
                            },
                          }
                        : namesDelimiter(form.toolPolicyPrompt)
                          ? {
                              // Naming the tag here is legal and costs the cache:
                              // this section is otherwise identical for the life
                              // of a session, so it rides the cached prefix —
                              // unless it spells out a delimiter that changes
                              // every turn, at which point the whole of it is
                              // re-sent per step.
                              warning: {
                                title: t(
                                  'agents.promptToolPolicyUncacheableTitle',
                                ),
                                message: t(
                                  'agents.promptToolPolicyUncacheable',
                                  {
                                    tag: '{{tag}}',
                                  },
                                ),
                              },
                            }
                          : {})}
                    onChange={(next) => {
                      update('toolPolicyPrompt', next);
                    }}
                  />
                  <TemplateEditor
                    key={`${String(formEpoch)}-memory`}
                    name={name}
                    label="agents.promptMemory"
                    builtIn={DEFAULT_MEMORY_TEMPLATE}
                    value={form.memoryPrompt}
                    placeholders={MEMORY_PROMPT_PLACEHOLDERS}
                    hint="agents.promptMemoryHint"
                    // Two conditions, broadest first. `toolsEnabled` off means
                    // the request advertises no tools at all, so nothing can
                    // open a memory and the section is not placed whatever the
                    // `memory` permission says — naming the narrower reason
                    // there would send an operator to fix the wrong switch.
                    {...(toolsOff
                      ? {
                          warning: {
                            title: t('agents.toolsOffTitle'),
                            message: t('agents.promptNotPlacedNoTools'),
                          },
                        }
                      : memoryOff
                        ? {
                            warning: {
                              title: t('agents.toolsOffTitle'),
                              message: t('agents.promptNotPlacedNoMemory'),
                            },
                          }
                        : {})}
                    onChange={(next) => {
                      update('memoryPrompt', next);
                    }}
                  />
                  <TemplateEditor
                    key={`${String(formEpoch)}-skills`}
                    name={name}
                    label="agents.promptSkills"
                    builtIn={DEFAULT_SKILLS_TEMPLATE}
                    value={form.skillsPrompt}
                    placeholders={SKILLS_PROMPT_PLACEHOLDERS}
                    hint="agents.promptSkillsHint"
                    {...(toolsOff
                      ? {
                          warning: {
                            title: t('agents.toolsOffTitle'),
                            message: t('agents.promptNotPlacedNoTools'),
                          },
                        }
                      : skillsOff
                        ? {
                            warning: {
                              title: t('agents.toolsOffTitle'),
                              message: t('agents.promptNotPlacedNoSkills'),
                            },
                          }
                        : {})}
                    onChange={(next) => {
                      update('skillsPrompt', next);
                    }}
                  />
                </>
              )}
            </div>
          </details>
        </div>
      </Section>

      <SaveBar
        dirty={dirty}
        saving={saving}
        onSave={onSave}
        onRevert={onRevert}
      />

      <ConfirmDialog
        open={confirmingDelete}
        onOpenChange={setConfirmingDelete}
        title={t('agents.deleteTitle')}
        description={`${name} is removed from the settings. Its sessions keep their history and fall back to the default agent.`}
        confirmLabel="Delete"
        pending={saving}
        onConfirm={onDelete}
      />
    </div>
  );
}
