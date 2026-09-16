// A minimal extension: one tool and one prompt section, over MCP's wire.
//
// It exists so the composition root's wiring can be tested against a real child
// process — the host boundary is a process, so a fake loader would be testing
// something the shipped code does not do.
import { createInterface } from 'node:readline';

const METHODS = {
  initialize: () => ({ protocolVersion: '2025-06-18', capabilities: {} }),
  'tools/list': () => ({
    tools: [
      {
        name: 'greet',
        description: 'Say hello.',
        inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      },
    ],
  }),
  'darkwire/context/static': () => ({
    sections: [{ title: 'Greeting', body: 'Be warm.' }],
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
