// A channel, and the two notifications a channel sends back.
//
// `ghostai/channels/start` is answered *and* followed by a publish, because an
// inbound message arriving the moment a transport connects is the case the
// binding order has to survive: the context must be bound at build time, not at
// start time.
import { createInterface } from 'node:readline';

function send(frame) {
  process.stdout.write(`${JSON.stringify(frame)}\n`);
}

const METHODS = {
  initialize: () => ({ protocolVersion: '2025-06-18', capabilities: {} }),
  'ghostai/channels/list': () => ({
    channels: [{ id: 'talkative' }, { id: 'talkative-dm' }, { id: 'squatter' }],
  }),
  'ghostai/channels/start': (params) => {
    send({
      jsonrpc: '2.0',
      method: 'ghostai/channels/publish',
      params: {
        channelId: params.channelId,
        sessionKey: 'inbound',
        senderId: 'someone',
        content: [{ type: 'text', text: 'hello from the transport' }],
      },
    });
    send({
      jsonrpc: '2.0',
      method: 'ghostai/channels/control',
      params: {
        channelId: params.channelId,
        sessionKey: 'inbound',
        frame: { type: 'turn.stop', sessionKey: 'inbound' },
      },
    });
    return {};
  },
  'ghostai/channels/send': (params) => {
    // Echoed back on stderr so the host's drain has something to carry, and so
    // a human reading the log can see what was rendered.
    process.stderr.write(`rendered ${params.message.kind}\n`);
    return {};
  },
  'ghostai/context/static': () => ({ sections: [{ title: 'Talkative', body: 'Present.' }] }),
  'ghostai/context/runtime': () => ({ sections: [{ body: 'Live state.' }] }),
};

const lines = createInterface({ input: process.stdin });
lines.on('line', (line) => {
  const message = JSON.parse(line);
  if (message.id === undefined || message.id === null) return;
  const method = METHODS[message.method];
  if (method === undefined) {
    send({
      jsonrpc: '2.0',
      id: message.id,
      error: { code: -32601, message: `Method not found: ${message.method}` },
    });
    return;
  }
  send({ jsonrpc: '2.0', id: message.id, result: method(message.params ?? {}) });
});
lines.on('close', () => process.exit(0));
