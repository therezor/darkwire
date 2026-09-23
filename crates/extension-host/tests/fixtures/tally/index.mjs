// Records every process that was started for it, so a test can count the
// children a host left running. Answers the handshake and nothing else.
import { appendFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { createInterface } from 'node:readline';

const dir = process.env.DARKWIRE_EXTENSION_DATA_DIR;
mkdirSync(dir, { recursive: true });
appendFileSync(join(dir, 'pids'), `${process.pid}\n`);

const lines = createInterface({ input: process.stdin });
lines.on('line', (line) => {
  const message = JSON.parse(line);
  if (message.id === undefined || message.id === null) return;
  const frame =
    message.method === 'initialize'
      ? { jsonrpc: '2.0', id: message.id, result: { protocolVersion: '2025-06-18', capabilities: {} } }
      : { jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'no' } };
  process.stdout.write(`${JSON.stringify(frame)}\n`);
});
lines.on('close', () => process.exit(0));
