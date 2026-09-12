import { describe, expect, it } from 'vitest';
import { z } from 'zod';

import { PROTOCOL_SCHEMAS, SCHEMA_MODULES } from '#src/schemas.js';

const entries = Object.entries(PROTOCOL_SCHEMAS);

describe('JSON Schema conversion', () => {
  it('has a non-empty registry', () => {
    expect(entries.length).toBeGreaterThan(50);
  });

  // The done-criterion for this package: every schema must survive
  // `z.toJSONSchema`, because the OpenAPI document and every tool definition are
  // generated through it. A schema using an unrepresentable construct — a
  // transform, a `Date`, a `bigint` — throws here rather than at server boot.
  describe.each(entries)('%s', (name, schema) => {
    it('converts in output mode', () => {
      const json = z.toJSONSchema(schema);
      expect(json).toBeTypeOf('object');
      expect(Object.keys(json).length).toBeGreaterThan(1);
    });

    it('converts in input mode', () => {
      // Input mode is what request-body validation documents: defaults make the
      // input and output shapes differ, and only input mode describes what a
      // client is allowed to send.
      expect(() => z.toJSONSchema(schema, { io: 'input' })).not.toThrow();
    });
  });
});

describe('registry completeness', () => {
  it('registers every exported schema', () => {
    // Reflection rather than a hand-kept list: a schema added to a module but
    // not to the registry would otherwise silently escape the conversion test
    // above.
    const registered = new Set<unknown>(Object.values(PROTOCOL_SCHEMAS));
    const missing: string[] = [];

    for (const [moduleName, module] of Object.entries(SCHEMA_MODULES)) {
      for (const [exportName, value] of Object.entries(module)) {
        if (!exportName.endsWith('Schema')) continue;
        if (!registered.has(value)) missing.push(`${moduleName}.${exportName}`);
      }
    }

    expect(missing).toEqual([]);
  });

  it('maps every registry key to a distinct schema', () => {
    expect(new Set(Object.values(PROTOCOL_SCHEMAS)).size).toBe(entries.length);
  });
});

describe('generated shapes', () => {
  it('emits a discriminated union as a oneOf over every variant', () => {
    const json = z.toJSONSchema(PROTOCOL_SCHEMAS.ClientMessage) as {
      oneOf?: unknown[];
    };
    expect(json.oneOf).toHaveLength(
      PROTOCOL_SCHEMAS.ClientMessage.options.length,
    );
  });

  it('carries defaults into the document so a client can see them', () => {
    const json = z.toJSONSchema(PROTOCOL_SCHEMAS.AgentSettings) as {
      properties: Record<string, { default?: unknown }>;
    };
    expect(json.properties.maxToolIterations?.default).toBe(40);
    expect(json.properties.maxTokens?.default).toBe(8192);
    // `temperature` deliberately has none: unset means the provider's own, and
    // a default here would be this project guessing at someone else's tuning.
    expect(json.properties.temperature).not.toHaveProperty('default');
  });

  it('marks defaulted fields optional in input mode and present in output mode', () => {
    const input = z.toJSONSchema(PROTOCOL_SCHEMAS.AgentSettings, {
      io: 'input',
    }) as {
      required?: string[];
    };
    const output = z.toJSONSchema(PROTOCOL_SCHEMAS.AgentSettings) as {
      required?: string[];
    };
    expect(input.required ?? []).not.toContain('maxToolIterations');
    expect(output.required ?? []).toContain('maxToolIterations');
  });

  it('leaves the channels block open to extension-supplied keys', () => {
    const json = z.toJSONSchema(PROTOCOL_SCHEMAS.ChannelsConfig) as {
      additionalProperties?: unknown;
    };
    expect(json.additionalProperties).not.toBe(false);
  });
});
