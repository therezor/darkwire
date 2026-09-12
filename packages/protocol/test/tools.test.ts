/**
 * Rewriting what a tool tells the model about itself.
 *
 * The half worth testing is what `applyToolPrompts` *declines* to do. An
 * override is operator input reaching the one part of the payload that has to
 * stay in step with a validator it cannot see, so the rules are that a name it
 * does not recognise is reported rather than invented, and that nothing it
 * touches was shared with anyone else.
 */

import { describe, expect, it } from 'vitest';

import { applyToolPrompts, type ToolDefinition } from '#src/tools.js';

/** Shaped like a real one: frozen, and `additionalProperties: false`. */
function definition(overrides: Partial<ToolDefinition> = {}): ToolDefinition {
  return {
    name: 'read_file',
    description: 'Reads a file in the workspace.',
    risk: 'safe',
    source: 'builtin',
    parameters: Object.freeze({
      type: 'object',
      additionalProperties: false,
      required: ['path'],
      properties: Object.freeze({
        path: Object.freeze({
          type: 'string',
          description: 'Workspace-relative path.',
        }),
        limit: Object.freeze({ type: 'number' }),
      }),
    }),
    ...overrides,
  };
}

describe('applyToolPrompts', () => {
  it('returns the same array when there are no overrides', () => {
    const definitions = [definition()];

    const applied = applyToolPrompts(definitions, {});

    expect(applied.definitions).toBe(definitions);
    expect(applied.unknownTools).toEqual([]);
    expect(applied.unknownFields).toEqual([]);
  });

  it('replaces a description', () => {
    const applied = applyToolPrompts([definition()], {
      read_file: {
        description: 'Read a file. Prefer this over `cat`.',
        fields: {},
      },
    });

    expect(applied.definitions[0]?.description).toBe(
      'Read a file. Prefer this over `cat`.',
    );
  });

  it('inherits the built-in description when the override is empty', () => {
    const applied = applyToolPrompts([definition()], {
      read_file: { description: '', fields: {} },
    });

    expect(applied.definitions[0]?.description).toBe(
      'Reads a file in the workspace.',
    );
  });

  it('advertises no description at all when the override is a single space', () => {
    // The same "empty inherits, whitespace deletes" rule the prompt templates
    // use. Rarely wanted, and the only way to say it.
    const applied = applyToolPrompts([definition()], {
      read_file: { description: ' ', fields: {} },
    });

    expect(applied.definitions[0]?.description).toBe('');
  });

  it('replaces a field description without touching its type', () => {
    const applied = applyToolPrompts([definition()], {
      read_file: {
        description: '',
        fields: { path: 'Relative to the workspace root.' },
      },
    });

    const properties = applied.definitions[0]?.parameters.properties as Record<
      string,
      Record<string, unknown>
    >;
    expect(properties.path).toEqual({
      type: 'string',
      description: 'Relative to the workspace root.',
    });
    // Untouched siblings survive, and so does everything above `properties`.
    expect(properties.limit).toEqual({ type: 'number' });
    expect(applied.definitions[0]?.parameters.additionalProperties).toBe(false);
    expect(applied.definitions[0]?.parameters.required).toEqual(['path']);
  });

  it('adds a description to a field that had none', () => {
    const applied = applyToolPrompts([definition()], {
      read_file: {
        description: '',
        fields: { limit: 'Maximum lines to return.' },
      },
    });

    const properties = applied.definitions[0]?.parameters.properties as Record<
      string,
      Record<string, unknown>
    >;
    expect(properties.limit).toEqual({
      type: 'number',
      description: 'Maximum lines to return.',
    });
  });

  it('never mutates the frozen definition it was given', () => {
    // `parameters` is computed once at definition time and shared by every turn
    // on every agent through the registry's memoised list. Writing into it in
    // place would give one agent's wording to all of them.
    const original = definition();
    const before = JSON.stringify(original);

    applyToolPrompts([original], {
      read_file: {
        description: 'Different.',
        fields: { path: 'Different too.' },
      },
    });

    expect(JSON.stringify(original)).toBe(before);
  });

  it('reports an override naming no advertised tool, and applies nothing', () => {
    const applied = applyToolPrompts([definition()], {
      exec: { description: 'Run a program.', fields: {} },
    });

    expect(applied.unknownTools).toEqual(['exec']);
    expect(applied.definitions[0]?.description).toBe(
      'Reads a file in the workspace.',
    );
  });

  it('drops a field naming no property rather than inventing one', () => {
    // Inventing it would advertise an argument the model then passes and the
    // tool's own Zod schema then rejects, on every call, with nothing saying why.
    const applied = applyToolPrompts([definition()], {
      read_file: { description: '', fields: { pat: 'A typo.' } },
    });

    const properties = applied.definitions[0]?.parameters.properties as Record<
      string,
      unknown
    >;
    expect(properties.pat).toBeUndefined();
    expect(applied.unknownFields).toEqual(['read_file.pat']);
  });

  it('reports every field of a tool whose schema has no properties', () => {
    const applied = applyToolPrompts(
      [
        definition({
          name: 'ping',
          parameters: { type: 'object', additionalProperties: false },
        }),
      ],
      { ping: { description: '', fields: { host: 'Where to ping.' } } },
    );

    expect(applied.unknownFields).toEqual(['ping.host']);
  });

  it('leaves a definition nobody overrode by reference', () => {
    const untouched = definition({ name: 'exec' });

    const applied = applyToolPrompts([definition(), untouched], {
      read_file: { description: 'Changed.', fields: {} },
    });

    expect(applied.definitions[1]).toBe(untouched);
  });
});
