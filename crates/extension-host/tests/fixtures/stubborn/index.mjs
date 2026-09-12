// A child that will not leave when asked politely.
//
// It ignores a closed stdin and traps SIGTERM, which is the case the escalation
// exists for: without SIGKILL this process outlives the host that spawned it.
import { createInterface } from 'node:readline';

process.on('SIGTERM', () => {});
process.on('SIGINT', () => {});

const lines = createInterface({ input: process.stdin });
lines.on('line', (line) => {
  const message = JSON.parse(line);
  if (message.id === undefined || message.id === null) return;
  process.stdout.write(
    `${JSON.stringify({ jsonrpc: '2.0', id: message.id, result: {} })}\n`,
  );
});
// Deliberately no `close` handler, and a timer so the event loop never drains.
lines.on('close', () => {});
setInterval(() => {}, 1000);
