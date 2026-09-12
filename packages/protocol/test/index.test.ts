import { describe, expect, it } from 'vitest';

import * as protocol from '#src/index.js';

/**
 * The barrel is the package's public surface. These assertions exist so a
 * refactor that moves a schema between modules but forgets to re-export it fails
 * here rather than in whichever downstream package imported it.
 */
describe('public surface', () => {
  it('pins the wire protocol version', () => {
    expect(protocol.PROTOCOL_VERSION).toBe(2);
  });

  it.each(['isLoopbackHost', 'isSequencedServerMessage'] as const)(
    'exports %s',
    (name) => {
      expect(protocol[name]).toBeTypeOf('function');
    },
  );

  it.each([
    'ChatMessageSchema',
    'StoredMessageSchema',
    'ToolDefinitionSchema',
    'ConfigSchema',
    'ConfigPatchSchema',
    'AutomationJobSchema',
    'ClientMessageSchema',
    'ServerMessageSchema',
    'ErrorResponseSchema',
    'StatusResponseSchema',
  ] as const)('exports %s', (name) => {
    expect(protocol[name]).toBeDefined();
  });

  it('exports the schema registry', () => {
    expect(Object.keys(protocol.PROTOCOL_SCHEMAS).length).toBeGreaterThan(50);
  });

  it('has no name collisions across the re-exported modules', () => {
    // `export *` silently drops a duplicate binding, so a collision would remove
    // a schema from the surface without any error.
    const names = Object.keys(protocol);
    expect(new Set(names).size).toBe(names.length);
  });
});
