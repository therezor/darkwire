// Declares three kinds and implements one. The two it does not implement earn
// the mirror image of the undeclared-kind warning.
import { createInterface } from 'node:readline';

const METHODS = {
  initialize: () => ({ protocolVersion: '2025-06-18', capabilities: {} }),
  'tools/list': () => ({ tools: [] }),
};

const lines = createInterface({ input: process.stdin });
lines.on('line', (line) => {
  const message = JSON.parse(line);
  if (message.id === undefined || message.id === null) return;
  const method = METHODS[message.method];
  const frame =
    method === undefined
      ? {
          jsonrpc: '2.0',
          id: message.id,
          error: { code: -32601, message: `Method not found: ${message.method}` },
        }
      : { jsonrpc: '2.0', id: message.id, result: method(message.params) };
  process.stdout.write(`${JSON.stringify(frame)}\n`);
});
lines.on('close', () => process.exit(0));
