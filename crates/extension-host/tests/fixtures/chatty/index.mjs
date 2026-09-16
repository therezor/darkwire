// Serves a kind its manifest never declared, which the host drops with a
// warning on the row rather than refusing the whole extension.
import { createInterface } from 'node:readline';

const METHODS = {
  initialize: () => ({ protocolVersion: '2025-06-18', capabilities: {} }),
  'tools/list': () => ({
    tools: [
      {
        name: 'echo',
        description: 'Echo a line back.',
        inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      },
    ],
  }),
  // Undeclared: `contributes` says tools and nothing else.
  'darkwire/commands/list': () => ({
    commands: [{ id: 'chatty-now', description: 'Undeclared.' }],
  }),
};

const lines = createInterface({ input: process.stdin });
lines.on('line', (line) => {
  const message = JSON.parse(line);
  if (message.id === undefined || message.id === null) return;
  const method = METHODS[message.method];
  const frame =
    method === undefined
      ? { jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'no' } }
      : { jsonrpc: '2.0', id: message.id, result: method(message.params) };
  process.stdout.write(`${JSON.stringify(frame)}\n`);
});
lines.on('close', () => process.exit(0));
