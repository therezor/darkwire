import {
  AgentEntrySchema,
  ConfigPatchSchema,
  type AgentEntry,
} from '@ghostwire/protocol';
import { describe, expect, it } from 'vitest';

import { createWebI18n } from '@ghostwire/i18n/web';

/** English, resolved: these assertions compare the message a user would read. */
const t = createWebI18n('en').getFixedT(null, 'web');

import {
  MODEL_REQUIRED,
  parseList,
  toAgentDeletePatch,
  toAgentEnabledPatch,
  toAgentEntryForm,
  toAgentEntryPatch,
  toNewAgentPatch,
  type AgentEntryForm,
} from '@/agents/agents-form.js';

const EMPTY = AgentEntrySchema.parse({});

/** A stated agent, since nothing is filled in from anywhere else any more. */
const STATED = AgentEntrySchema.parse({
  provider: 'ollama',
  model: 'llama3',
  maxTokens: 4096,
  toolTimeoutMs: 20_000,
});

function form(overrides: Partial<AgentEntryForm> = {}): AgentEntryForm {
  return { ...toAgentEntryForm(STATED), ...overrides };
}

/** Every patch this form produces has to survive the schema the server applies. */
function parsed(result: ReturnType<typeof toAgentEntryPatch>): unknown {
  if (!result.ok) {
    throw new Error(`expected a patch, got ${JSON.stringify(result.errors)}`);
  }
  return ConfigPatchSchema.parse(result.patch).agents?.list?.reviewer;
}

describe('toAgentEntryForm', () => {
  it('shows what the entry states, which the schema has already completed', () => {
    // Nothing on this screen inherits, so a blank box would be a claim that the
    // agent runs on no model and no budget — which is not what a turn would do.
    const shown = toAgentEntryForm(STATED);

    expect(shown.provider).toBe('ollama');
    expect(shown.model).toBe('llama3');
    expect(shown.maxTokens).toBe('4096');
    expect(shown.toolTimeoutSeconds).toBe('20');
  });

  it('shows an unconfigured agent as unconfigured rather than borrowing a model', () => {
    // An entry that states no model shows as unconfigured rather than holding
    // somebody else's — there is nowhere for a borrowed one to come from.
    const shown = toAgentEntryForm(EMPTY);

    expect(shown.model).toBe('');
    expect(shown.maxTokens).toBe('8192');
  });

  it('leaves genuinely unset settings unset, rather than inventing values', () => {
    // Neither has a default: unset means the request carries no such parameter,
    // which is the only thing that works for a model that rejects it.
    const shown = toAgentEntryForm(EMPTY);

    expect(shown.temperature).toBe('');
    expect(shown.reasoningEffort).toBe('');
  });

  it('shows the fields that belong to the agent itself', () => {
    const shown = toAgentEntryForm(
      AgentEntrySchema.parse({ label: 'Reviewer', systemPrompt: 'Be terse.' }),
    );

    expect(shown.label).toBe('Reviewer');
    expect(shown.systemPrompt).toBe('Be terse.');
    expect(shown.enabled).toBe(true);
  });

  it('keeps zero apart from absent, because zero is a temperature', () => {
    const shown = toAgentEntryForm(
      AgentEntrySchema.parse({
        model: 'qwen3:32b',
        temperature: 0,
        toolTimeoutMs: 30_000,
      }),
    );

    expect(shown.model).toBe('qwen3:32b');
    // Zero is a value, not an absence — it must not render as "the provider's own".
    expect(shown.temperature).toBe('0');
    expect(shown.toolTimeoutSeconds).toBe('30');
  });

  it('carries the tool permissions across as stored', () => {
    const shown = toAgentEntryForm(
      AgentEntrySchema.parse({
        tools: { read_file: 'allow', exec: 'ask', write_file: 'deny' },
      }),
    );

    expect(shown.tools).toEqual({
      read_file: 'allow',
      exec: 'ask',
      write_file: 'deny',
    });
  });
});

describe('toAgentEntryPatch', () => {
  it('writes the settings down, so the agent stops depending on the defaults', () => {
    // The form was filled from `agents.list.default`; the point of saving it is
    // that a later change to those defaults no longer moves this agent.
    const entry = parsed(toAgentEntryPatch('reviewer', form(), EMPTY, t));

    expect(entry).toMatchObject({
      provider: 'ollama',
      model: 'llama3',
      maxTokens: 4096,
      toolTimeoutMs: 20_000,
    });
  });

  it('leaves temperature and reasoning effort out when they are blank', () => {
    // The two that can genuinely be unset. Omitting the key is what clears it,
    // because `agents.list.*` is replaced wholesale.
    const entry = parsed(toAgentEntryPatch('reviewer', form(), EMPTY, t));

    expect(entry).not.toHaveProperty('temperature');
    expect(entry).not.toHaveProperty('reasoningEffort');
  });

  it('refuses a required box the operator emptied', () => {
    // It arrived holding a number. A blank one is a deletion, not an unset —
    // and there is nothing left for it to fall through to.
    const result = toAgentEntryPatch(
      'reviewer',
      form({ maxTokens: '' }),
      EMPTY,
      t,
    );

    expect(result.ok).toBe(false);
    if (result.ok) throw new Error('unreachable');
    expect(result.errors.maxTokens).toBe('Required');
  });

  it('carries the values that were filled in', () => {
    const entry = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({ model: 'qwen3:32b', temperature: '0', reasoningEffort: 'high' }),
        EMPTY,
        t,
      ),
    );

    expect(entry).toMatchObject({
      model: 'qwen3:32b',
      temperature: 0,
      reasoningEffort: 'high',
    });
  });

  it('writes both capability switches down rather than leaving them to inherit', () => {
    // The form arrived holding the default agent's answer, so an omitted key
    // would quietly re-point this agent at a default it had already been shown
    // disagreeing with the next time somebody changed it.
    const entry = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({ visionEnabled: false, toolsEnabled: false }),
        EMPTY,
        t,
      ),
    );

    expect(entry).toMatchObject({ visionEnabled: false, toolsEnabled: false });
  });

  it('keeps the settings this screen does not render', () => {
    // `agents.list.*` is replaced wholesale, so an entry rebuilt from the form
    // alone would drop the exec allow-list every time the prompt was saved —
    // silently, and with no way to notice from the screen.
    const stored = AgentEntrySchema.parse({
      provider: 'ollama',
      model: 'llama3',
      exec: { allowedBinaries: ['git'] },
    });

    const entry = parsed(toAgentEntryPatch('reviewer', form(), stored, t));

    expect(entry).toMatchObject({ exec: { allowedBinaries: ['git'] } });
  });

  it('drives the toolbox from the form, now that the screen renders it', () => {
    // The opposite of the case above, and the reason the two are separate tests:
    // a field the screen shows must come from the screen, or clearing it in the
    // UI would silently keep the stored value.
    const stored = AgentEntrySchema.parse({
      provider: 'ollama',
      model: 'llama3',
      toolbox: {
        name: 'kali-pentest',
        network: { mode: 'allowlist', allow: ['10.0.0.0/8'] },
      },
    });

    const entry = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({ toolboxName: 'ghost-research', toolboxNetworkMode: 'open' }),
        stored,
        t,
      ),
    );

    expect((entry as { toolbox: Record<string, unknown> }).toolbox).toEqual({
      name: 'ghost-research',
      network: { mode: 'open', allow: [] },
      tools: {},
    });
  });

  it('carries the per-box defaults a preset wrote, which it cannot edit', () => {
    // This screen edits `tools`, which sits above `toolbox.tools`. `toToolbox`
    // rebuilds the object from named fields, so a field with nowhere to live on
    // the form is one a save deletes — and an agent installed with two of a
    // box's programs would quietly acquire the rest the first time somebody
    // renamed it.
    const stored = AgentEntrySchema.parse({
      provider: 'ollama',
      model: 'llama3',
      toolbox: {
        name: 'recon',
        network: { mode: 'open', allow: [] },
        tools: { '*': 'deny', nmap: 'allow' },
      },
    });

    const entry = parsed(
      toAgentEntryPatch('reviewer', form(toAgentEntryForm(stored)), stored, t),
    );

    expect(
      (entry as { toolbox: Record<string, unknown> }).toolbox.tools,
    ).toEqual({ '*': 'deny', nmap: 'allow' });
  });

  it('drops the per-box defaults along with the box', () => {
    // A default for a toolbox the agent no longer works in would take effect
    // again the moment somebody picked one, which nobody asked for.
    const stored = AgentEntrySchema.parse({
      provider: 'ollama',
      model: 'llama3',
      toolbox: { name: 'recon', tools: { '*': 'deny' } },
    });

    const entry = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({ ...toAgentEntryForm(stored), toolboxName: '' }),
        stored,
        t,
      ),
    );

    expect(
      (entry as { toolbox: Record<string, unknown> }).toolbox.tools,
    ).toEqual({});
  });

  it('clears the allow-list when the mode stops using it', () => {
    // Otherwise a stale set of CIDRs sits in the file waiting to take effect the
    // next time somebody switches back to `allowlist`.
    const entry = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({
          toolboxName: 'kali',
          toolboxNetworkMode: 'none',
          toolboxAllow: '10.0.0.0/8',
        }),
        EMPTY,
        t,
      ),
    );

    expect(
      (entry as { toolbox: Record<string, unknown> }).toolbox.network,
    ).toEqual({
      mode: 'none',
      allow: [],
    });
  });

  it('forces an agent with no toolbox onto no network', () => {
    // Egress scoping is enforced by the sandbox, so it means nothing on the host
    // — and `assertBuildable` refuses the combination outright, which would turn
    // a save into a 400 rather than a setting that quietly does nothing.
    const entry = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({ toolboxName: '', toolboxNetworkMode: 'open' }),
        EMPTY,
        t,
      ),
    );

    expect((entry as { toolbox: Record<string, unknown> }).toolbox).toEqual({
      name: '',
      network: { mode: 'none', allow: [] },
      tools: {},
    });
  });

  it('reports every bad field at once, not the first', () => {
    const result = toAgentEntryPatch(
      'reviewer',
      form({ maxTokens: 'lots', temperature: '9', contextWindowTokens: '-1' }),
      EMPTY,
      t,
    );

    expect(result.ok).toBe(false);
    if (result.ok) throw new Error('unreachable');
    expect(Object.keys(result.errors).sort()).toEqual([
      'contextWindowTokens',
      'maxTokens',
      'temperature',
    ]);
  });

  it('refuses an empty id', () => {
    const result = toAgentEntryPatch('  ', form(), EMPTY, t);

    expect(result.ok).toBe(false);
    if (result.ok) throw new Error('unreachable');
    expect(result.errors.id).toBe('Required');
  });

  it('refuses an empty provider, which would resolve to nothing', () => {
    const result = toAgentEntryPatch(
      'reviewer',
      form({ provider: '  ' }),
      EMPTY,
      t,
    );

    expect(result.ok).toBe(false);
    if (result.ok) throw new Error('unreachable');
    expect(result.errors.provider).toBe('Required');
  });

  it('refuses an empty model, which is an agent that cannot run', () => {
    // This used to be saved as a value, on the reading that an empty model asks
    // the registry to resolve one. It does not: `Runtime#resolveProvider` turns
    // an empty model into `noModelError` and hands the loop a `null` provider,
    // so what the form was writing down was an agent with no way to take a turn.
    const result = toAgentEntryPatch(
      'reviewer',
      form({ model: '  ' }),
      EMPTY,
      t,
    );

    expect(result.ok).toBe(false);
    if (result.ok) throw new Error('unreachable');
    expect(result.errors.model).toBe(MODEL_REQUIRED);
  });

  it('converts seconds to milliseconds, matching the defaults form', () => {
    const entry = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({ toolTimeoutSeconds: '45' }),
        EMPTY,
        t,
      ),
    );
    expect(entry).toMatchObject({ toolTimeoutMs: 45_000 });
  });

  it('ignores a reasoning effort the enum does not have', () => {
    const entry = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({ reasoningEffort: 'extreme' }),
        EMPTY,
        t,
      ),
    );
    expect(entry).not.toHaveProperty('reasoningEffort');
  });

  it('keeps `xhigh`, which the enum does have', () => {
    // The counterpart to the case above, and the one that catches the failure
    // mode of adding a level to the schema alone: `REASONING_EFFORTS` is what
    // the guard checks, so a value missing from it is dropped on save without
    // a word.
    const entry = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({ reasoningEffort: 'xhigh' }),
        EMPTY,
        t,
      ),
    );
    expect(entry).toMatchObject({ reasoningEffort: 'xhigh' });
  });

  it('always sends the whole tool map, so a tool can be removed', () => {
    // The merge replaces this object wholesale; sending only what changed would
    // make "this agent no longer has that tool" impossible to express.
    const entry = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({ tools: { read_file: 'allow' } }),
        EMPTY,
        t,
      ),
    );
    expect(entry).toMatchObject({ tools: { read_file: 'allow' } });
    expect(
      (entry as { tools: Record<string, unknown> }).tools,
    ).not.toHaveProperty('exec');
  });

  it('lets an agent hold no tools at all', () => {
    const entry = parsed(
      toAgentEntryPatch('reviewer', form({ tools: {} }), EMPTY, t),
    );
    expect(entry).toMatchObject({ tools: {} });
  });

  it('carries a disabled tool through rather than dropping the key', () => {
    // `deny` and absent mean the same thing to the runtime, but only `deny`
    // survives a round trip through the editor as a row with an off position.
    const entry = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({ tools: { exec: 'deny' } }),
        EMPTY,
        t,
      ),
    );
    expect(entry).toMatchObject({ tools: { exec: 'deny' } });
  });

  it('produces a patch the server’s own schema accepts', () => {
    const result = toAgentEntryPatch(
      'reviewer',
      form({ label: 'Reviewer', model: 'qwen3:32b', tools: { exec: 'deny' } }),
      EMPTY,
      t,
    );

    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error('unreachable');
    expect(ConfigPatchSchema.safeParse(result.patch).success).toBe(true);
  });
});

describe('toNewAgentPatch', () => {
  const template: AgentEntry = AgentEntrySchema.parse({
    provider: 'ollama',
    model: 'llama3',
    maxTokens: 4096,
    toolTimeoutMs: 20_000,
    systemPrompt: 'House style: be terse.',
    tools: { read_file: 'allow', exec: 'deny' },
  });

  it('copies the model and budget the template would have run on', () => {
    // Copied rather than omitted: an omitted field would take the schema's
    // default, and an agent running on 8192 tokens because its template's 4096
    // was left out is not a copy.
    const patch = toNewAgentPatch('reviewer', 'Reviewer', template);

    expect(patch.agents?.list?.reviewer).toMatchObject({
      label: 'Reviewer',
      enabled: true,
      provider: 'ollama',
      model: 'llama3',
      maxTokens: 4096,
      toolTimeoutMs: 20_000,
    });
  });

  it('copies how the template behaves as well as what it runs on', () => {
    const patch = toNewAgentPatch('reviewer', 'Reviewer', template);

    expect(patch.agents?.list?.reviewer).toMatchObject({
      systemPrompt: 'House style: be terse.',
      tools: { read_file: 'allow', exec: 'deny' },
    });
    expect(ConfigPatchSchema.safeParse(patch).success).toBe(true);
  });

  it('carries a switched-off capability into the copy', () => {
    // Inheritance is the wrong default here: `AgentEntrySchema` makes these
    // optional and `agents.list.default` says `true`, so an omitted `false` reads
    // as "ask the default" and comes back on. Duplicating an agent with vision
    // switched off produced one with vision switched on.
    const restricted = AgentEntrySchema.parse({
      visionEnabled: false,
      toolsEnabled: false,
    });
    const patch = toNewAgentPatch('copy', 'Copy', restricted);

    expect(patch.agents?.list?.copy).toMatchObject({
      visionEnabled: false,
      toolsEnabled: false,
    });
  });

  it('writes the capabilities the template inherited rather than omitting them', () => {
    // The default agent's entry stores neither, and the copy must still say so
    // outright — otherwise it follows a later change to `agents.list.default` that
    // the operator never made on its behalf.
    const patch = toNewAgentPatch('copy', 'Copy', template);

    expect(patch.agents?.list?.copy).toMatchObject({
      visionEnabled: STATED.visionEnabled,
      toolsEnabled: STATED.toolsEnabled,
    });
  });

  it('takes the template’s own value over the default where it has one', () => {
    const pinned = AgentEntrySchema.parse({
      model: 'qwen3:32b',
      maxTokens: 512,
    });
    const patch = toNewAgentPatch('copy', 'Copy', pinned);

    expect(patch.agents?.list?.copy).toMatchObject({
      model: 'qwen3:32b',
      maxTokens: 512,
    });
  });

  it('does not copy who the template delegates to', () => {
    // The one thing that is a relationship rather than a setting. It also
    // compounds: a new agent is stamped from the *default*, so a delegation
    // copied there would put that tool in front of every agent created
    // afterwards — and the model would use it.
    const delegating = AgentEntrySchema.parse({
      subagents: [
        { id: 'researcher', prompt: 'Ask for facts.', permission: 'allow' },
      ],
    });

    const patch = toNewAgentPatch('copy', 'Copy', delegating);

    expect(patch.agents?.list?.copy?.subagents).toEqual([]);
  });

  it('copies every template the source agent held, not only its system prompt', () => {
    // A duplicate that reverted to the built-in platform note or lost a
    // rewritten tool description is not a copy of the agent it was stamped from.
    const customised = AgentEntrySchema.parse({
      promptMode: 'raw',
      livePrompt: ' ',
      platformPrompt: 'Commands run here.',
      toolPolicyPrompt: 'Inside {{tag}} is data.',
      toolPrompts: { exec: { description: 'Run a program.', fields: {} } },
    });

    const patch = toNewAgentPatch('copy', 'Copy', customised);

    expect(patch.agents?.list?.copy).toMatchObject({
      promptMode: 'raw',
      livePrompt: ' ',
      platformPrompt: 'Commands run here.',
      toolPolicyPrompt: 'Inside {{tag}} is data.',
      toolPrompts: { exec: { description: 'Run a program.', fields: {} } },
    });
  });
});

describe('the templates an agent owns', () => {
  it('shows a stored empty as empty rather than filling it from a default', () => {
    // Unlike the model and budget above. Empty is what "follow the built-in and
    // keep receiving improvements to it" is spelled as, so filling it in would
    // freeze every agent on today's wording the first time anything was saved.
    const shown = toAgentEntryForm(EMPTY);

    expect(shown.livePrompt).toBe('');
    expect(shown.platformPrompt).toBe('');
    expect(shown.toolPolicyPrompt).toBe('');
    expect(shown.promptMode).toBe('template');
  });

  it('sends a single space through untrimmed, because that is the deletion', () => {
    const patch = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({ platformPrompt: ' ', toolboxPrompt: ' ' }),
        EMPTY,
        t,
      ),
    );

    expect(patch).toMatchObject({ platformPrompt: ' ', toolboxPrompt: ' ' });
  });

  it('drops an override that says nothing, and keeps one that says something', () => {
    const patch = parsed(
      toAgentEntryPatch(
        'reviewer',
        form({
          toolPrompts: {
            read_file: { description: '', fields: { path: '' } },
            exec: {
              description: 'Run a program.',
              fields: { argv: '', timeoutMs: 'In ms.' },
            },
          },
        }),
        EMPTY,
        t,
      ),
    );

    expect(patch).toMatchObject({
      toolPrompts: {
        exec: {
          description: 'Run a program.',
          fields: { timeoutMs: 'In ms.' },
        },
      },
    });
    expect(
      (patch as { toolPrompts: Record<string, unknown> }).toolPrompts.read_file,
    ).toBeUndefined();
  });

  it('falls back to template for a mode nothing recognises', () => {
    const patch = parsed(
      toAgentEntryPatch('reviewer', form({ promptMode: 'weird' }), EMPTY, t),
    );

    expect(patch).toMatchObject({ promptMode: 'template' });
  });
});

describe('toAgentEnabledPatch', () => {
  const entry = AgentEntrySchema.parse({
    label: 'Reviewer',
    systemPrompt: 'Only ever read.',
    tools: { read_file: 'allow', exec: 'deny' },
  });

  it('carries the whole agent, because the patch replaces it wholesale', () => {
    // `{ enabled: false }` alone would switch the agent off by deleting its
    // prompt and its tool permissions — so switching it back on would return an
    // empty agent wearing the same name.
    const patch = toAgentEnabledPatch('reviewer', entry, false);

    expect(patch.agents?.list?.reviewer).toMatchObject({
      enabled: false,
      systemPrompt: 'Only ever read.',
      tools: { read_file: 'allow', exec: 'deny' },
    });
    expect(ConfigPatchSchema.safeParse(patch).success).toBe(true);
  });

  it('switches one back on', () => {
    const off = AgentEntrySchema.parse({ label: 'Reviewer', enabled: false });
    expect(
      toAgentEnabledPatch('reviewer', off, true).agents?.list?.reviewer,
    ).toMatchObject({
      enabled: true,
    });
  });
});

describe('parseList', () => {
  it.each([
    ['', []],
    ['exec', ['exec']],
    ['a, b ,  c', ['a', 'b', 'c']],
    ['a, , b', ['a', 'b']],
    ['  ', []],
  ])('parses %j', (value, expected) => {
    expect(parseList(value)).toEqual(expected);
  });
});

describe('toAgentDeletePatch', () => {
  it('uses the null the merge reads as a deletion', () => {
    const patch = toAgentDeletePatch('reviewer');

    expect(patch).toEqual({ agents: { list: { reviewer: null } } });
    expect(ConfigPatchSchema.safeParse(patch).success).toBe(true);
  });

  it('stays one key, and does not try to clean up the delegations to it', () => {
    // Stripping the references belongs to the server, which owns the only
    // chokepoint every write goes through — a client-side cascade would miss a
    // delete made through the API or by hand, and would have to be right about
    // a merge rule it does not implement.
    const patch = toAgentDeletePatch('reviewer');

    expect(Object.keys(patch.agents?.list ?? {})).toEqual(['reviewer']);
  });
});

describe('subagents', () => {
  const RESEARCHER = {
    id: 'researcher',
    prompt: 'Ask for facts.',
    permission: 'ask',
  } as const;

  it('round-trips a stored ref through the form', () => {
    const entry = AgentEntrySchema.parse({
      provider: 'ollama',
      model: 'llama3',
      subagents: [RESEARCHER],
    });

    expect(toAgentEntryForm(entry).subagents).toEqual([RESEARCHER]);
  });

  it('fills in the defaults a bare ref leaves out', () => {
    const entry = AgentEntrySchema.parse({ subagents: [{ id: 'researcher' }] });

    expect(toAgentEntryForm(entry).subagents).toEqual([
      { id: 'researcher', prompt: '', permission: 'allow' },
    ]);
  });

  it('sends the whole list, so removing one is expressible', () => {
    const entry = AgentEntrySchema.parse({
      subagents: [
        RESEARCHER,
        { id: 'reviewer', prompt: '', permission: 'allow' },
      ],
    });
    const result = toAgentEntryPatch(
      'main',
      form({ subagents: [RESEARCHER] }),
      entry,
      t,
    );

    if (!result.ok) throw new Error(JSON.stringify(result.errors));
    expect(result.patch.agents?.list?.main?.subagents).toEqual([RESEARCHER]);
  });

  it('drops a row the operator added and never filled in', () => {
    // The editor appends `{ id: '' }` on Add. Sending it would be a `config`
    // error from `assertBuildable` about an agent named "".
    const result = toAgentEntryPatch(
      'main',
      form({
        subagents: [RESEARCHER, { id: '', prompt: '', permission: 'allow' }],
      }),
      EMPTY,
      t,
    );

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.patch.agents?.list?.main?.subagents).toEqual([RESEARCHER]);
  });

  it('trims the guidance, which becomes a tool description', () => {
    const result = toAgentEntryPatch(
      'main',
      form({ subagents: [{ ...RESEARCHER, prompt: '  Ask for facts.  ' }] }),
      EMPTY,
      t,
    );

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.patch.agents?.list?.main?.subagents?.[0]?.prompt).toBe(
      'Ask for facts.',
    );
  });

  it('produces a patch the server would accept', () => {
    const result = toAgentEntryPatch(
      'main',
      form({ subagents: [RESEARCHER] }),
      EMPTY,
      t,
    );

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(ConfigPatchSchema.safeParse(result.patch).success).toBe(true);
  });

  it('does not lose them when the prompt is the only thing edited', () => {
    // `agents.list.*` replaces wholesale, so a field the form fails to carry
    // through is erased on every save that touches anything else.
    const entry = AgentEntrySchema.parse({
      provider: 'ollama',
      model: 'llama3',
      subagents: [RESEARCHER],
    });
    const result = toAgentEntryPatch(
      'main',
      { ...toAgentEntryForm(entry), systemPrompt: 'Be brief.' },
      entry,
      t,
    );

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.patch.agents?.list?.main?.subagents).toEqual([RESEARCHER]);
  });
});

describe('memory and skills', () => {
  it('reads a granted tool as on, and an absent one as off', () => {
    // Absent is denied, which is `runtime.ts`'s rule for the contributor too —
    // an install predating these tools has no key and gets no section.
    const granted = AgentEntrySchema.parse({
      tools: { memory: 'allow', skill: 'allow' },
    });
    const absent = AgentEntrySchema.parse({ tools: { read_file: 'allow' } });

    expect(toAgentEntryForm(granted).tools).toMatchObject({
      memory: 'allow',
      skill: 'allow',
    });
    expect(toAgentEntryForm(absent).tools.memory).toBeUndefined();
    expect(toAgentEntryForm(absent).tools.skill).toBeUndefined();
  });

  it('writes the permission the switch stands for, and nothing beside it', () => {
    // The switch and the Tools row are one value. A second key here is what the
    // permission was chosen over, so the patch must carry no other trace.
    const stored = AgentEntrySchema.parse({
      provider: 'ollama',
      model: 'llama3',
      tools: { read_file: 'allow', memory: 'allow', skill: 'allow' },
    });

    const patch = parsed(
      toAgentEntryPatch(
        'reviewer',
        {
          ...toAgentEntryForm(stored),
          tools: { read_file: 'allow', memory: 'deny', skill: 'deny' },
        },
        stored,
        t,
      ),
    );

    expect(patch).toMatchObject({
      tools: { read_file: 'allow', memory: 'deny', skill: 'deny' },
    });
    expect(patch).not.toHaveProperty('memoryEnabled');
    expect(patch).not.toHaveProperty('skillsEnabled');
  });

  it('carries the memory prompt raw, so a space can delete the section', () => {
    // `''` and `' '` mean different things — inherit the built-in, and remove
    // the section — so a trim anywhere on the way through would make deleting
    // it impossible to express.
    expect(
      parsed(
        toAgentEntryPatch('reviewer', form({ memoryPrompt: ' ' }), EMPTY, t),
      ),
    ).toMatchObject({ memoryPrompt: ' ' });

    expect(
      parsed(
        toAgentEntryPatch('reviewer', form({ memoryPrompt: '' }), EMPTY, t),
      ),
    ).toMatchObject({ memoryPrompt: '' });
  });

  it('carries every other override through a memory-only edit', () => {
    // `agents.list.*` is REPLACE_WHOLESALE, so a patch that dropped what it did
    // not touch would delete the rest of this agent. `ownFields` is what stops
    // that, and this is the assertion that keeps it honest.
    const stored = AgentEntrySchema.parse({
      provider: 'ollama',
      model: 'llama3',
      label: 'Reviewer',
      tools: { read_file: 'allow', memory: 'deny' },
      enabled: false,
    });

    const patch = parsed(
      toAgentEntryPatch(
        'reviewer',
        {
          ...toAgentEntryForm(stored),
          memoryPrompt: '## What I know\n\n{{index}}',
        },
        stored,
        t,
      ),
    );

    expect(patch).toMatchObject({
      memoryPrompt: '## What I know\n\n{{index}}',
      label: 'Reviewer',
      enabled: false,
      tools: { read_file: 'allow', memory: 'deny' },
    });
  });
});
