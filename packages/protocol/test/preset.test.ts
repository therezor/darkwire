import { describe, expect, it } from 'vitest';

import {
  AgentEntrySchema,
  AgentPresetSchema,
  DEFAULT_AGENT_TOOLS,
  presetToAgentEntry,
} from '#src/index.js';

const MINIMAL = { schema: 'ghostai.agent-preset/1', id: 'researcher' };

describe('AgentPresetSchema', () => {
  it('needs only a schema tag and an id', () => {
    const preset = AgentPresetSchema.parse(MINIMAL);

    expect(preset.label).toBe('');
    expect(preset.promptMode).toBe('template');
    expect(preset.tools).toEqual(DEFAULT_AGENT_TOOLS);
    expect(preset.toolbox).toEqual({
      name: '',
      network: { mode: 'none', allow: [] },
      tools: {},
    });
    expect(preset.subagents).toEqual([]);
    expect(preset.skills).toEqual([]);
    // Absent means the schema's `true`, which only an absent key can say.
    expect(preset.toolsEnabled).toBeUndefined();
  });

  it('refuses a skill name that could climb out of the skills folder', () => {
    // The traversal boundary, and the reason `skills` is a slug rather than a
    // string: the name becomes a path segment, it arrived over the network, and
    // the copier is not the thing that should be judging it.
    for (const name of ['..', '../evil', 'a/b', '~/x', 'Code Review', '']) {
      expect(
        AgentPresetSchema.safeParse({ ...MINIMAL, skills: [name] }).success,
      ).toBe(false);
    }

    expect(
      AgentPresetSchema.parse({ ...MINIMAL, skills: ['code-review', 'deploy'] })
        .skills,
    ).toEqual(['code-review', 'deploy']);
  });

  it('refuses a schema tag it does not recognise', () => {
    // A literal rather than a string, for the reason the toolbox manifest
    // gives: a breaking format change has to fail loudly on the old file
    // rather than parse it into something that means something else now.
    expect(() =>
      AgentPresetSchema.parse({ ...MINIMAL, schema: 'ghostai.agent/1' }),
    ).toThrow();
  });

  it('cannot name an image, caps or limits', () => {
    // The whole point of the shape: a preset reuses `AgentToolboxSchema`, so
    // the boundary fields live only in the operator-approved manifest and a
    // preset has no field through which to widen them.
    const preset = AgentPresetSchema.parse({
      ...MINIMAL,
      toolbox: { name: 'web-research', network: { mode: 'open' } },
    });

    expect(preset.toolbox).toEqual({
      name: 'web-research',
      network: { mode: 'open', allow: [] },
      tools: {},
    });
    expect('image' in preset.toolbox).toBe(false);
  });
});

describe('presetToAgentEntry', () => {
  it('installs enabled, with the entry schema applying its own defaults', () => {
    const entry = presetToAgentEntry(
      AgentPresetSchema.parse({
        ...MINIMAL,
        label: 'Researcher',
        systemPrompt: '# {{name}}\n\nResearch things.',
      }),
    );

    expect(entry.enabled).toBe(true);
    expect(entry.label).toBe('Researcher');
    expect(entry.systemPrompt).toContain('Research things.');
    // Round-trips through the schema that owns the shape.
    expect(AgentEntrySchema.parse(entry)).toEqual(entry);
  });

  it('takes the schema default for toolsEnabled unless the preset set it', () => {
    const ordinary = presetToAgentEntry(AgentPresetSchema.parse(MINIMAL));
    expect(ordinary.toolsEnabled).toBe(true);

    const disabled = presetToAgentEntry(
      AgentPresetSchema.parse({ ...MINIMAL, toolsEnabled: false }),
    );
    expect(disabled.toolsEnabled).toBe(false);
  });

  it('carries neither the schema tag nor the id into the entry', () => {
    // The id becomes the `agents.list` key; the tag described the file. An
    // entry holding either would round-trip them into every settings save.
    const entry = presetToAgentEntry(AgentPresetSchema.parse(MINIMAL));

    expect('id' in entry).toBe(false);
    expect('schema' in entry).toBe(false);
  });

  it('carries no skills list into the entry', () => {
    // Which sheets to copy is an instruction to the installer. An entry holding
    // it would round-trip it into every settings save, and re-saving the agent
    // would read as a request to copy them again.
    const entry = presetToAgentEntry(
      AgentPresetSchema.parse({ ...MINIMAL, skills: ['code-review'] }),
    );

    expect('skills' in entry).toBe(false);
  });
});
