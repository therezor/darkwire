// A child that stops reading its stdin once asked to, and stays alive.
//
// The case the bounded outbound queue exists for: the host's writes pile up
// behind a pipe nobody drains, and the host has to call it hung.
import { writeSync } from 'node:fs';
import { createInterface } from 'node:readline';

const METHODS = {
  initialize: () => ({ protocolVersion: '2025-06-18', capabilities: {} }),
  'darkwire/commands/list': () => ({
    commands: [{ id: 'deaf-now', description: 'Stops reading stdin.' }],
  }),
};

const lines = createInterface({ input: process.stdin });
lines.on('line', (line) => {
  const message = JSON.parse(line);
  if (message.id === undefined || message.id === null) return;
  if (message.method === 'darkwire/commands/run') {
    // Written synchronously, then the whole process blocks, so nothing ever
    // drains the pipe again. Pausing the stream is not enough: Node keeps
    // reading into it.
    writeSync(
      1,
      `${JSON.stringify({ jsonrpc: '2.0', id: message.id, result: { message: 'deaf', ok: true } })}\n`,
    );
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0);
    return;
  }
  const method = METHODS[message.method];
  const frame =
    method === undefined
      ? { jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'no' } }
      : { jsonrpc: '2.0', id: message.id, result: method(message.params) };
  process.stdout.write(`${JSON.stringify(frame)}\n`);
});
setInterval(() => {}, 1000);
