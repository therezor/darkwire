import { describe, expect, it } from 'vitest';

import {
  AgentSettingsSchema,
  AgentEntrySchema,
  ConfigPatchSchema,
  ConfigSchema,
  DEFAULT_AGENT_TOOLS,
  McpServerConfigSchema,
  agentSettingsPatch,
  isLoopbackHost,
} from '#src/config.js';

describe('ConfigSchema', () => {
  it('produces a fully populated tree from an empty object', () => {
    // Every nested block uses `.prefault({})`, so a brand-new install needs no
    // config file at all — including one agent, complete, under `default`.
    const config = ConfigSchema.parse({});

    expect(config.agents.list.default?.maxToolIterations).toBe(40);
    expect(config.server.port).toBe(3000);
    expect(config.server.auth.enabled).toBe(true);
    // The tool layer is the agent's: `tools` holds the MCP servers alone.
    expect(config.tools).toEqual({ mcpServers: {} });
    expect(config.agents.list.default?.approvalTimeoutMs).toBe(5 * 60 * 1000);
    expect(config.agents.list.default?.maxOutputChars).toBe(8192);
    expect(config.agents.list.default?.exec.maxOutputBytes).toBe(1024 * 1024);
    expect(config.agents.list.default?.lazyDiscovery).toBe(false);
    expect(config.agents.list.default?.pinnedTools).toEqual([]);
    expect(config.scheduler.concurrency).toBe(2);
    expect(config.scheduler.runRetention).toBe(200);
    // UTC, not the host zone: a server's own zone moves when the server does.
    // It lives on `ui` rather than `scheduler` because one install-wide zone
    // both renders every timestamp and reads every cron expression.
    expect(config.ui.timezone).toBe('UTC');
    expect(config.channels.sendProgress).toBe(true);
    expect(config.extensions.allowOverride).toBe(false);
    expect(config.extensions.settings).toEqual({});
  });

  it('keeps the scheduler block to the engine, with nothing describing a task', () => {
    // A heartbeat *is* a job: its interval is the job's schedule, its file and
    // model are the job's payload, its on/off is the job's own flag. This block
    // used to carry a `heartbeat` sub-block restating all of that — a second
    // vocabulary for one concept, and the one nothing read.
    const scheduler = ConfigSchema.parse({}).scheduler;

    // No `timezone` either, and that is the newer half of the same rule: the
    // zone is not a property of the engine, it is the install's one answer to
    // "whose clock" — read here *and* by every screen that renders a time — so
    // it lives on `ui` beside the locale.
    expect(Object.keys(scheduler).sort()).toEqual([
      'catchUpOnBoot',
      'concurrency',
      'enabled',
      'runRetention',
    ]);
  });

  // Any array default would do; `allowedBinaries` is simply one of them. The
  // defect this guards against is zod handing every parse the same array
  // instance, so a caller that pushes to one config's list silently edits every
  // other config in the process.
  it('does not share mutable defaults between parses', () => {
    const a = ConfigSchema.parse({});
    const b = ConfigSchema.parse({});
    expect(a.agents.list.default?.exec.allowedBinaries).not.toBe(
      b.agents.list.default?.exec.allowedBinaries,
    );
  });

  it('preserves a partial override without dropping siblings', () => {
    const config = ConfigSchema.parse({
      agents: { list: { default: { temperature: 0.7 } } },
    });
    expect(config.agents.list.default?.temperature).toBe(0.7);
    expect(config.agents.list.default?.maxTokens).toBe(8192);
  });

  it('gives a fresh install exactly one agent, complete and unconfigured', () => {
    // There is no settings layer above an agent, so `default` has to exist for
    // an unbound conversation to have anything to run on. Unconfigured is a
    // state rather than an error: the model is empty and everything else is
    // answered by the schema.
    const agent = ConfigSchema.parse({}).agents.list.default;

    expect(agent?.model).toBe('');
    expect(agent?.provider).toBe('auto');
    expect(agent?.maxTokens).toBe(8192);
    // The two that are genuinely optional stay absent — that is what "let the
    // provider decide" is spelled as.
    expect(agent).not.toHaveProperty('temperature');
    expect(agent).not.toHaveProperty('reasoningEffort');
  });

  it('does not share the prefaulted default agent between parses', () => {
    const a = ConfigSchema.parse({});
    const b = ConfigSchema.parse({});
    expect(a.agents.list.default).not.toBe(b.agents.list.default);
  });

  it('accepts unknown channel blocks so a channel extension needs no schema change', () => {
    const config = ConfigSchema.parse({
      channels: { telegram: { token: 'x', allowlist: ['1|me'] } },
    });
    expect(config.channels.telegram).toEqual({
      token: 'x',
      allowlist: ['1|me'],
    });
  });

  it('keys providers by instance id rather than a fixed field list', () => {
    const config = ConfigSchema.parse({
      providers: {
        laptop: { type: 'ollama', apiBase: 'http://localhost:11434' },
        anthropic: { type: 'anthropic' },
      },
    });
    expect(config.providers.laptop?.apiBase).toBe('http://localhost:11434');
    expect(config.providers.anthropic?.extraHeaders).toEqual({});
  });

  it('accepts two instances of one provider type', () => {
    // The shape this replaced was keyed by provider id, which capped the tree
    // at one endpoint per provider — two Ollama servers were inexpressible.
    const config = ConfigSchema.parse({
      providers: {
        ollama: { type: 'ollama' },
        'ollama-gpu': {
          type: 'ollama',
          label: 'GPU box',
          apiBase: 'http://gpu.lan:11434/v1',
        },
      },
    });
    expect(Object.keys(config.providers)).toEqual(['ollama', 'ollama-gpu']);
    expect(config.providers['ollama-gpu']?.label).toBe('GPU box');
    expect(config.providers.ollama?.enabled).toBe(true);
  });

  it('refuses an instance that does not name a type', () => {
    expect(ConfigSchema.safeParse({ providers: { ollama: {} } }).success).toBe(
      false,
    );
  });

  it('rejects an out-of-range port', () => {
    expect(ConfigSchema.safeParse({ server: { port: 70_000 } }).success).toBe(
      false,
    );
    expect(ConfigSchema.safeParse({ server: { port: 0 } }).success).toBe(false);
  });

  it('rejects a negative timeout but allows 0 as "no limit"', () => {
    expect(AgentSettingsSchema.safeParse({ toolTimeoutMs: -1 }).success).toBe(
      false,
    );
    expect(AgentSettingsSchema.parse({ toolTimeoutMs: 0 }).toolTimeoutMs).toBe(
      0,
    );
  });

  it('rejects a non-integer iteration cap', () => {
    expect(
      AgentSettingsSchema.safeParse({ maxToolIterations: 2.5 }).success,
    ).toBe(false);
  });

  it('constrains temperature to a sane range', () => {
    expect(AgentSettingsSchema.safeParse({ temperature: 3 }).success).toBe(
      false,
    );
    expect(AgentSettingsSchema.safeParse({ temperature: -0.1 }).success).toBe(
      false,
    );
  });

  it('omits an unset reasoning effort rather than defaulting it', () => {
    // A provider that does not support the parameter must not receive it at all;
    // `withResilience` drops it on a 400, and a default would mask that path.
    expect(AgentSettingsSchema.parse({})).not.toHaveProperty('reasoningEffort');
  });

  it('rejects an unknown reasoning effort', () => {
    expect(
      AgentSettingsSchema.safeParse({ reasoningEffort: 'extreme' }).success,
    ).toBe(false);
  });

  it('takes `off` as a reasoning effort, which is not the same as unset', () => {
    // Unset means the request carries no reasoning parameter and the provider
    // decides; `off` means it carries one asking for none. On a model that
    // thinks unless told otherwise those are two different turns, so the enum
    // has to be able to say the second.
    const config = AgentSettingsSchema.parse({ reasoningEffort: 'off' });

    expect(config.reasoningEffort).toBe('off');
    expect(AgentSettingsSchema.parse({})).not.toHaveProperty('reasoningEffort');
  });

  it('takes `xhigh`, which is a real level and not one of ours', () => {
    // Qwen3.8 calls its top rung `xhigh` and runs there unless told otherwise.
    // The enum has to be able to say it, because the alternative — leaving it
    // out — is an install that cannot ask for the level its model defaults to.
    const config = AgentSettingsSchema.parse({ reasoningEffort: 'xhigh' });

    expect(config.reasoningEffort).toBe('xhigh');
  });

  it('leaves vision and tool calling on, so an existing install is unchanged', () => {
    // Both are opt-*out*. Defaulting either to false would silently take a
    // capability away from every agent already configured on a model that has it.
    const config = AgentSettingsSchema.parse({});

    expect(config.visionEnabled).toBe(true);
    expect(config.toolsEnabled).toBe(true);
  });
});

describe('DEFAULT_AGENT_TOOLS', () => {
  it('asks before exec and allows reads and jailed writes', () => {
    expect(DEFAULT_AGENT_TOOLS).toMatchObject({
      read: 'allow',
      ls: 'allow',
      write: 'allow',
      edit: 'allow',
      exec: 'ask',
    });
  });

  it('cannot be mutated by whoever seeds an agent from it', () => {
    // Spread into every new entry, so a caller that pushed into it would change
    // what the *next* agent is created with.
    expect(Object.isFrozen(DEFAULT_AGENT_TOOLS)).toBe(true);
  });
});

describe('McpServerConfigSchema', () => {
  it('exposes every advertised tool by default', () => {
    expect(McpServerConfigSchema.parse({}).enabledTools).toEqual(['*']);
  });

  it('leaves transport unset so it can be inferred from command vs url', () => {
    expect(McpServerConfigSchema.parse({})).not.toHaveProperty('type');
  });

  it('rejects an unknown transport', () => {
    expect(McpServerConfigSchema.safeParse({ type: 'grpc' }).success).toBe(
      false,
    );
  });

  it('requires the full triad when oauth is present', () => {
    expect(
      McpServerConfigSchema.safeParse({
        url: 'https://x',
        oauth: { clientId: 'a' },
      }).success,
    ).toBe(false);
    expect(
      McpServerConfigSchema.safeParse({
        url: 'https://x',
        oauth: { authUrl: 'https://a', tokenUrl: 'https://t', clientId: 'c' },
      }).success,
    ).toBe(true);
  });
});

describe('AgentEntrySchema', () => {
  it('completes itself from the schema, leaving only the two that mean "unset"', () => {
    // The whole of the change: an entry is complete after parsing, so nothing
    // downstream has to merge it with anything. `model` is the one field with no
    // useful default — empty is *unconfigured*, which is a state an agent is
    // listed and edited in and refused a turn in.
    const agent = AgentEntrySchema.parse({});

    expect(agent.model).toBe('');
    expect(agent.provider).toBe('auto');
    expect(agent.maxTokens).toBe(8192);
    // Absent, not defaulted: unset means the request carries no such parameter
    // and the provider applies its own, which is a different turn from any value.
    expect(agent).not.toHaveProperty('temperature');
    expect(agent).not.toHaveProperty('reasoningEffort');
  });

  it('defaults the fields that belong to the agent itself', () => {
    const agent = AgentEntrySchema.parse({});

    expect(agent.label).toBe('');
    expect(agent.systemPrompt).toBe('');
    // Empty means "inherit the built-in", which is what keeps an install that
    // never customised a section receiving improvements to it on upgrade.
    expect(agent.memoryPrompt).toBe('');
    expect(agent.enabled).toBe(true);
    expect(agent.tools).toEqual(DEFAULT_AGENT_TOOLS);
    expect(agent.environment).toEqual({
      name: '',
      alwaysUseOwn: false,
      network: { mode: 'none', allow: [], hosts: [], dns: [] },
    });
  });

  it('does not let an agent pin its own workspace', () => {
    // The working folder is a session axis shared by every agent. Accepting it
    // here would let one agent quietly work somewhere else.
    const agent = AgentEntrySchema.parse({ workspace: '/tmp/elsewhere' });
    expect(agent).not.toHaveProperty('workspace');
  });

  it('keeps overrides it is given', () => {
    const agent = AgentEntrySchema.parse({
      label: 'Code Reviewer',
      model: 'claude-opus-5',
      temperature: 0,
      tools: { read: 'allow', exec: 'deny' },
    });

    expect(agent.model).toBe('claude-opus-5');
    expect(agent.temperature).toBe(0);
    expect(agent.tools).toEqual({ read: 'allow', exec: 'deny' });
  });

  it('seeds the built-in tools when an entry names none', () => {
    expect(AgentEntrySchema.parse({}).tools).toEqual(DEFAULT_AGENT_TOOLS);
  });

  it('replaces the seed rather than merging into it', () => {
    // The distinction the whole model rests on: an entry naming one tool has
    // one tool, or switching a seeded tool off would be inexpressible.
    const agent = AgentEntrySchema.parse({ tools: { read: 'allow' } });
    expect(agent.tools).toEqual({ read: 'allow' });
  });

  it('accepts an empty map as "nothing enabled"', () => {
    expect(AgentEntrySchema.parse({ tools: {} }).tools).toEqual({});
  });

  it('rejects an unknown permission', () => {
    expect(
      AgentEntrySchema.safeParse({ tools: { exec: 'maybe' } }).success,
    ).toBe(false);
  });

  it('still validates an inherited field it is given', () => {
    expect(AgentEntrySchema.safeParse({ temperature: 9 }).success).toBe(false);
    expect(
      AgentEntrySchema.safeParse({ reasoningEffort: 'nope' }).success,
    ).toBe(false);
  });
});

describe('AgentsConfigSchema', () => {
  it('starts with the default agent and nothing else', () => {
    const agents = ConfigSchema.parse({}).agents;
    expect(Object.keys(agents.list)).toEqual(['default']);
    expect(agents.list.default?.provider).toBe('auto');
  });

  it('keeps the scheduler block to the engine, with nothing describing a task', () => {
    // A heartbeat *is* a job: its interval is the job's schedule, its file and
    // model are the job's payload, its on/off is the job's own flag. This block
    // used to carry a `heartbeat` sub-block restating all of that — a second
    // vocabulary for one concept, and the one nothing read.
    const scheduler = ConfigSchema.parse({}).scheduler;

    // No `timezone` either, and that is the newer half of the same rule: the
    // zone is not a property of the engine, it is the install's one answer to
    // "whose clock" — read here *and* by every screen that renders a time — so
    // it lives on `ui` beside the locale.
    expect(Object.keys(scheduler).sort()).toEqual([
      'catchUpOnBoot',
      'concurrency',
      'enabled',
      'runRetention',
    ]);
  });

  it('does not share mutable defaults between parses', () => {
    const one = AgentEntrySchema.parse({});
    const two = AgentEntrySchema.parse({});

    expect(one.tools).not.toBe(two.tools);
  });

  it('keys agents by an id the operator chooses', () => {
    const agents = ConfigSchema.parse({
      agents: { list: { reviewer: { label: 'Reviewer' }, writer: {} } },
    }).agents;

    expect(Object.keys(agents.list)).toEqual(['reviewer', 'writer']);
    expect(agents.list.reviewer?.label).toBe('Reviewer');
  });
});

describe('ConfigPatchSchema', () => {
  /**
   * The incident, encoded.
   *
   * Removing `agents.defaults` left every already-loaded browser tab sending
   * the old shape for `/model`. A stripping schema turned that into a 200 and
   * "the agent now runs it. Saved." over a config the request never touched —
   * the only failure a person can neither see nor act on. A refusal names the
   * key, and an old client's existing error path is what shows it.
   */
  it('refuses a section it does not have rather than dropping it', () => {
    const stale = ConfigPatchSchema.safeParse({
      agents: { defaults: { provider: 'ollama', model: 'qwen3' } },
    });

    expect(stale.success).toBe(false);
    expect(JSON.stringify(stale.error?.issues)).toContain('defaults');
  });

  it('refuses an unknown key at the root, for the same reason', () => {
    expect(ConfigPatchSchema.safeParse({ workspaces: {} }).success).toBe(false);
  });

  it('accepts a null to delete a named agent', () => {
    const patch = ConfigPatchSchema.parse({
      agents: { list: { reviewer: null } },
    });
    expect(patch.agents?.list?.reviewer).toBeNull();
  });

  it('patches one agent without restating the others or its own siblings', () => {
    const patch = ConfigPatchSchema.parse({
      agents: { list: { reviewer: { temperature: 0 } } },
    });

    expect(patch.agents?.list?.reviewer?.temperature).toBe(0);
    expect(patch.agents?.list?.reviewer).not.toHaveProperty('label');
  });

  it('accepts a single deeply nested field', () => {
    const patch = ConfigPatchSchema.parse({
      agents: { list: { coder: { temperature: 0.5 } } },
    });
    expect(patch.agents?.list?.coder?.temperature).toBe(0.5);
  });

  it('does not fill in defaults for absent sections', () => {
    // A patch must describe only what changed, or saving one settings panel
    // would rewrite every other section with its defaults.
    expect(ConfigPatchSchema.parse({})).toEqual({});
  });

  it('patches a nested block without its siblings', () => {
    const patch = ConfigPatchSchema.parse({
      server: { auth: { enabled: false } },
    });
    expect(patch.server?.auth?.enabled).toBe(false);
    expect(patch.server).not.toHaveProperty('port');
  });

  it('still validates the fields it is given', () => {
    expect(ConfigPatchSchema.safeParse({ server: { port: -1 } }).success).toBe(
      false,
    );
    expect(
      ConfigPatchSchema.safeParse({
        agents: { list: { a: { tools: { exec: 'nope' } } } },
      }).success,
    ).toBe(false);
  });

  it('carries a null through to delete an MCP server', () => {
    // `mergeConfigPatch` has listed `tools.mcpServers.*` in `DELETE_BY_NULL`
    // since before there was a client, and could never fire: the patch schema
    // rejected the null one layer above it. This is the case that proves the
    // two agree.
    const patch = ConfigPatchSchema.parse({
      tools: { mcpServers: { github: null } },
    });
    expect(patch.tools?.mcpServers?.github).toBeNull();
  });

  it('patches one field of an MCP server without restating the rest', () => {
    const patch = ConfigPatchSchema.parse({
      tools: { mcpServers: { github: { enabled: false } } },
    });
    expect(patch.tools?.mcpServers?.github?.enabled).toBe(false);
    expect(patch.tools?.mcpServers?.github).not.toHaveProperty('command');
    expect(patch.tools).not.toHaveProperty('exec');
  });

  it('still validates an MCP server it is given', () => {
    expect(
      ConfigPatchSchema.safeParse({
        tools: { mcpServers: { github: { args: 'not-an-array' } } },
      }).success,
    ).toBe(false);
  });
});

describe('isLoopbackHost', () => {
  it.each([
    '127.0.0.1',
    '127.1.2.3',
    '127.255.255.255',
    'localhost',
    'LOCALHOST',
    '::1',
    '[::1]',
  ])('treats %s as loopback', (host) => {
    expect(isLoopbackHost(host)).toBe(true);
  });

  it.each([
    '0.0.0.0',
    '::',
    '',
    '192.168.1.10',
    '10.0.0.1',
    'example.com',
    '128.0.0.1',
  ])('treats %s as remote', (host) => {
    expect(isLoopbackHost(host)).toBe(false);
  });

  it('ignores surrounding whitespace', () => {
    expect(isLoopbackHost('  127.0.0.1 ')).toBe(true);
  });

  it('does not treat a host merely starting with 127 as loopback', () => {
    expect(isLoopbackHost('127.0.0.1.evil.com')).toBe(false);
    expect(isLoopbackHost('1270.0.0.1')).toBe(false);
  });
});

describe('ConfigPatchSchema: environments', () => {
  it('accepts a network patch that changes only the mode', () => {
    // `patchOf` is not recursive, so without the hand-restated `network` this
    // would demand `allow` back — and a panel that never rendered the allow-list
    // would clear it on every save of the mode.
    const patch = ConfigPatchSchema.parse({
      agents: {
        list: { boxed: { environment: { network: { mode: 'open' } } } },
      },
    });

    expect(patch.agents?.list?.boxed?.environment?.network).toEqual({
      mode: 'open',
    });
  });
});

describe('agentSettingsPatch', () => {
  /** A config with one named agent that overrides more than it inherits. */
  function withCoder(): ReturnType<typeof ConfigSchema.parse> {
    return ConfigSchema.parse({
      agents: {
        defaults: { model: 'qwen3', provider: 'ollama' },
        list: {
          coder: {
            label: 'Coder',
            systemPrompt: 'Write code.',
            model: 'llama3',
            provider: 'ollama',
            temperature: 0.7,
            tools: { read: 'allow' },
          },
        },
      },
    });
  }

  it('sends the agent back whole, because the subtree replaces wholesale', () => {
    // The bug this function exists to make unwriteable, and it is the same rule
    // for every agent now: `agents.list.*` is in `REPLACE_WHOLESALE`, so a patch
    // naming `model` alone does not set one field — it replaces the entry.
    const patch = agentSettingsPatch(ConfigSchema.parse({}), 'default', {
      model: 'gpt-4o',
      provider: 'openai',
    });

    const entry = patch.agents?.list?.default;
    expect(entry?.model).toBe('gpt-4o');
    expect(entry?.provider).toBe('openai');
    expect(entry?.maxTokens).toBe(8192);
  });

  it('sends a named agent back whole, because that subtree replaces wholesale', () => {
    // The bug this function exists to make unwriteable. `agents.list.*` is in
    // `REPLACE_WHOLESALE` — the patch *is* the agent — so a patch naming
    // `model` alone does not set one field, it replaces the entry and takes
    // the label, the prompt and the tools with it.
    const patch = agentSettingsPatch(withCoder(), 'coder', {
      model: 'gpt-4o',
      provider: 'openai',
    });

    const entry = patch.agents?.list?.coder;
    expect(entry?.model).toBe('gpt-4o');
    expect(entry?.provider).toBe('openai');
    expect(entry?.label).toBe('Coder');
    expect(entry?.systemPrompt).toBe('Write code.');
    expect(entry?.temperature).toBe(0.7);
    expect(entry?.tools).toEqual({ read: 'allow' });
  });

  it('clears on a named agent by omitting the key, never by nulling it', () => {
    // The opposite mechanic to the default agent's, and the reason both live
    // here rather than at two call sites. A `null` would reach
    // `AgentEntrySchema` as a value and be rejected.
    const patch = agentSettingsPatch(withCoder(), 'coder', {
      temperature: null,
    });

    const entry = patch.agents?.list?.coder ?? undefined;
    expect(Object.keys(entry ?? {})).not.toContain('temperature');
    expect(entry?.label).toBe('Coder');
  });

  it('re-parses as a patch, so the merge is never handed an illegal tree', () => {
    expect(
      ConfigPatchSchema.safeParse(
        agentSettingsPatch(withCoder(), 'coder', { model: 'gpt-4o' }),
      ).success,
    ).toBe(true);
  });

  it('writes a whole agent for an id that names no entry', () => {
    // An agent deleted underneath a conversation. The patch creates a complete
    // one rather than a fragment: half an agent under a dead id would be worse
    // than a whole one, and the schema is what completes it.
    const patch = agentSettingsPatch(withCoder(), 'ghost', { model: 'gpt-4o' });

    const entry = patch.agents?.list?.ghost;
    expect(entry?.model).toBe('gpt-4o');
    expect(entry?.maxTokens).toBe(8192);
  });

  it('sends the entry back unchanged when nothing is mentioned', () => {
    // `undefined` is dropped: an absent key in `changes` means "leave it", not
    // "clear it" — `null` is what clears.
    const patch = agentSettingsPatch(withCoder(), 'coder', {});

    expect(patch.agents?.list?.coder?.model).toBe('llama3');
    expect(patch.agents?.list?.coder?.label).toBe('Coder');
  });
});
