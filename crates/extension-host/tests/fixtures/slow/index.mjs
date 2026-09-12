// A command that never answers, and a runtime context section that never
// answers. Both are the case the caps exist for: the host stops waiting, the
// turn survives, and the extension stays loaded.
import { createInterface } from 'node:readline';

const METHODS = {
  initialize: () => ({ protocolVersion: '2025-06-18', capabilities: {} }),
  'ghostai/commands/list': () => ({
    commands: [{ id: 'slow-forever', description: 'Never answers.' }],
  }),
  'ghostai/context/static': () => ({
    sections: [{ title: 'Slow', body: 'A section that arrives.' }],
  }),
};

const lines = createInterface({ input: process.stdin });
lines.on('line', (line) => {
  const message = JSON.parse(line);
  if (message.id === undefined || message.id === null) return;
  // These two are answered by never answering.
  if (
    message.method === 'ghostai/commands/run' ||
    message.method === 'ghostai/context/runtime'
  ) {
    return;
  }
  const method = METHODS[message.method];
  const frame =
    method === undefined
      ? { jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'no' } }
      : { jsonrpc: '2.0', id: message.id, result: method(message.params) };
  process.stdout.write(`${JSON.stringify(frame)}\n`);
});
lines.on('close', () => process.exit(0));
