#!/usr/bin/env node
/**
 * The mock model `pnpm demo` records against. Started by `demo-cast.mjs`.
 *
 * Two routes of the `openai-chat` wire — `GET /models` and a streaming
 * `POST /chat/completions` — scripted to call `list_dir` and then answer from
 * what came back. The same shape `packages/e2e/src/harness/script.ts` gives the
 * browser suite. Nothing here reaches the network.
 *
 * **It is a separate process on purpose.** `demo-cast.mjs` drives the recorder
 * with `execFileSync`, which blocks its event loop for the length of the take —
 * a server living in that process would accept no connection at all, and the
 * recording would be eleven seconds of `thinking…`. Which is exactly what it
 * was, before this file existed.
 */

import { createServer } from 'node:http';

const PORT = 11500;
const MODEL = 'qwen3:8b';

const ANSWER = [
  'The workspace holds a single note file. ',
  'Reading it back is one line:\n\n',
  '```ts\n',
  "const note = await readFile('notes.md', 'utf8');\n",
  '```\n\n',
  'That is the whole of it.',
];

const chunk = (delta, finish = null) => ({
  id: 'chatcmpl-demo',
  object: 'chat.completion.chunk',
  created: 0,
  model: MODEL,
  choices: [{ index: 0, delta, finish_reason: finish }],
});

createServer((req, res) => {
  if ((req.url ?? '').endsWith('/models')) {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ data: [{ id: MODEL, object: 'model' }] }));
    return;
  }

  let body = '';
  req.on('data', (d) => (body += d));
  req.on('end', () => {
    // A `tool` message in the history means the call already ran, so this is
    // the second request of the turn and the model answers from its result.
    const answering = (JSON.parse(body || '{}').messages ?? []).some(
      (m) => m.role === 'tool',
    );
    const send = (o) => res.write(`data: ${JSON.stringify(o)}\n\n`);

    res.writeHead(200, {
      'content-type': 'text/event-stream',
      'cache-control': 'no-cache',
    });

    if (answering) {
      for (const text of ANSWER) send(chunk({ content: text }));
      send(chunk({}, 'stop'));
    } else {
      send(
        chunk({
          tool_calls: [
            {
              index: 0,
              id: 'call-list',
              type: 'function',
              function: { name: 'list_dir', arguments: '{"path":"."}' },
            },
          ],
        }),
      );
      send(chunk({}, 'tool_calls'));
    }
    res.write('data: [DONE]\n\n');
    res.end();
  });
}).listen(PORT, '127.0.0.1', () => process.stdout.write('ready\n'));
