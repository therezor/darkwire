/**
 * A model on a port, answering the `openai-chat` wire from a lookup table.
 *
 * The server under test is `darkwire serve` as a child process, and the only
 * seam left between it and a real model is the one every install already has —
 * an HTTP endpoint. So the fake lives out at that seam. Two routes of the wire,
 * `GET /models` and `POST /chat/completions`, are the whole of it.
 *
 * **The routing is a lookup, not a script, and that is the important part.** A
 * positional script reads its turns in order, which is right for a unit test
 * that owns the provider for one assertion and wrong for a browser suite: one
 * server serves a whole spec file, and a reload replays a turn the model has
 * already answered once. So each request answers from what the request itself
 * says — the last thing the user said picks the route, and how many assistant
 * turns have happened since picks the turn within it. Nothing here remembers
 * having answered, which is exactly what makes replay-after-reload produce the
 * same answer rather than the next one.
 *
 * The usage numbers are fixed for the same reason: the context panel and the
 * turn-stats dialog are asserted on, and a token count that moved with the
 * prompt would make those assertions depend on the system prompt's length.
 */

import { createServer, type IncomingMessage, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';

import type { Route, ScriptedTurn, ToolCall } from './script.js';

/** The models this endpoint publishes, and what a spec may ask for. */
const MODELS: readonly string[] = ['qwen3', 'gpt-oss'];

/**
 * What every answer reports it cost.
 *
 * A constant rather than a count of anything. The numbers reach the context
 * meter and the turn-stats dialog, both of which are asserted on, and counting
 * real tokens would make those assertions move whenever the system prompt does.
 */
const USAGE = {
  prompt_tokens: 412,
  completion_tokens: 96,
  total_tokens: 508,
} as const;

/**
 * How often a held-open answer says it is still there.
 *
 * The client gives up on a stream that produces no bytes for two minutes. A
 * turn held open for a spec to Stop is meant to last exactly as long as the
 * spec, not to end in a provider timeout the spec never asked about — and an
 * SSE comment is the wire's own way to say "still here" without emitting an
 * event the transcript would render.
 */
const KEEPALIVE_MS = 10_000;

/** What the harness needs back to point the config at this and to stop it. */
export interface FakeProvider {
  /** The `apiBase` a provider instance points at. Carries no trailing slash. */
  readonly url: string;
  close(): Promise<void>;
}

/** One part of a multi-part content array. Only the text matters here. */
interface WirePart {
  readonly text?: string;
}

/** One message as the wire carries it. Content is a string, parts, or null. */
interface WireMessage {
  readonly role: string;
  readonly content?: string | readonly WirePart[] | null;
}

interface WireRequest {
  readonly model?: string;
  readonly messages?: readonly WireMessage[];
  readonly stream?: boolean;
}

/** The text of a message, whichever of the two shapes it arrived in. */
function textOf(message: WireMessage): string {
  const { content } = message;
  if (typeof content === 'string') return content;
  if (content === undefined || content === null) return '';
  return content
    .map((part) => part.text ?? '')
    .filter((text) => text !== '')
    .join('\n');
}

/**
 * Whether this is the loop's trailing prompt turn rather than something a
 * person said.
 *
 * The runtime half of the prompt — live state, the turn's delimiter — travels
 * as a `user` message after the history, so the session stays inside the
 * provider's cached prefix. A real model reads it as the operator metadata its
 * envelope says it is; this fake keys off "the last thing the user said", so
 * without this check every request would look like the user had typed a clock.
 */
function isRuntimeReminder(message: WireMessage): boolean {
  return (
    message.role === 'user' && textOf(message).startsWith('<system-reminder>')
  );
}

/** The last thing the user said, or `''` before they have said anything. */
function lastUserText(messages: readonly WireMessage[]): string {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (message === undefined || isRuntimeReminder(message)) continue;
    if (message.role === 'user') return textOf(message);
  }
  return '';
}

/**
 * How many model turns have already happened since the last thing the user
 * said.
 *
 * The loop appends an assistant message for every iteration and a tool message
 * for every call it made, so counting assistant messages after the final user
 * message gives the index of the turn about to be produced. A reload replays
 * the same history and therefore lands on the same index — which is what makes
 * the resume spec assert on an answer rather than on a coincidence.
 */
function turnIndex(messages: readonly WireMessage[]): number {
  let index = 0;
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (message === undefined) continue;
    if (isRuntimeReminder(message)) continue;
    if (message.role === 'user') break;
    if (message.role === 'assistant') index += 1;
  }
  return index;
}

/**
 * A default that is deliberately dull.
 *
 * Every screen that is not the chat view still boots a session, and a fallback
 * that refused would turn "the settings panel rendered" into "the model had no
 * script for the empty string".
 */
const FALLBACK: ScriptedTurn = { text: 'Ready.' };

function turnFor(routes: readonly Route[], request: WireRequest): ScriptedTurn {
  const messages = request.messages ?? [];
  const text = lastUserText(messages);
  const route = routes.find((candidate) => candidate.match.test(text));
  if (route === undefined) return FALLBACK;
  const index = Math.min(turnIndex(messages), route.turns.length - 1);
  return route.turns[index] ?? FALLBACK;
}

/** One chunk of the stream, in the shape the adapter reads. */
function chunk(
  model: string,
  delta: Record<string, unknown>,
  finish: string | null = null,
): string {
  return frame({
    id: 'chatcmpl-e2e',
    object: 'chat.completion.chunk',
    created: 0,
    model,
    choices: [{ index: 0, delta, finish_reason: finish }],
  });
}

/** One SSE frame: `data:`, the payload, and the blank line that ends it. */
function frame(payload: unknown): string {
  return `data: ${JSON.stringify(payload)}\n\n`;
}

/** The tool-call delta, which carries `index` so continuations could follow. */
function toolCallDelta(calls: readonly ToolCall[]): Record<string, unknown> {
  return {
    tool_calls: calls.map((call, index) => ({
      index,
      id: call.id,
      type: 'function',
      function: { name: call.name, arguments: JSON.stringify(call.args) },
    })),
  };
}

/** Splits a turn's prose into a few deltas, so the transcript streams. */
function deltasOf(text: string): readonly string[] {
  const parts = text.match(/[\s\S]{1,24}/gu);
  return parts ?? [];
}

async function readBody(request: IncomingMessage): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const part of request) chunks.push(part as Buffer);
  return Buffer.concat(chunks).toString('utf8');
}

/**
 * Starts the endpoint on a port the OS picks.
 *
 * Loopback, because the provider layer refuses to send a key over plain HTTP to
 * anything else — and because a test suite that listened on a routable address
 * would be a service on the developer's network for as long as the run lasts.
 */
export async function startFakeProvider(
  routes: readonly Route[],
): Promise<FakeProvider> {
  /** Held-open answers, so `close()` cannot leave a socket behind. */
  const held = new Set<() => void>();

  const server: Server = createServer((request, response) => {
    if ((request.url ?? '').endsWith('/models')) {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(
        JSON.stringify({
          object: 'list',
          data: MODELS.map((id) => ({ id, object: 'model' })),
        }),
      );
      return;
    }

    void (async () => {
      const body = await readBody(request);
      const parsed = JSON.parse(body === '' ? '{}' : body) as WireRequest;
      const model = parsed.model ?? MODELS[0] ?? 'qwen3';
      const turn = turnFor(routes, parsed);

      // `stream` in the body, not the `Accept` header: the adapter asks for
      // `application/json` on both requests and says which one it wants here.
      if (parsed.stream !== true) {
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(
          JSON.stringify({
            id: 'chatcmpl-e2e',
            object: 'chat.completion',
            created: 0,
            model,
            choices: [
              {
                index: 0,
                message: {
                  role: 'assistant',
                  content: turn.text ?? '',
                  ...(turn.reasoning === undefined
                    ? {}
                    : { reasoning_content: turn.reasoning }),
                  ...(turn.toolCalls === undefined
                    ? {}
                    : {
                        tool_calls: turn.toolCalls.map((call) => ({
                          id: call.id,
                          type: 'function',
                          function: {
                            name: call.name,
                            arguments: JSON.stringify(call.args),
                          },
                        })),
                      }),
                },
                finish_reason:
                  turn.toolCalls === undefined ? 'stop' : 'tool_calls',
              },
            ],
            usage: USAGE,
          }),
        );
        return;
      }

      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache',
        connection: 'keep-alive',
      });

      // A turn that never ends on its own. What Stop looks like over TCP is the
      // client hanging up, so the answer stays open until it does — with a
      // comment line often enough that the adapter's idle timer never fires,
      // since a provider timeout is not the state any spec is asking about.
      if (turn.hold === true) {
        const timer = setInterval(
          () => response.write(': ping\n\n'),
          KEEPALIVE_MS,
        );
        const stop = (): void => {
          clearInterval(timer);
          held.delete(stop);
          response.end();
        };
        held.add(stop);
        request.on('close', stop);
        return;
      }

      if (turn.reasoning !== undefined) {
        for (const part of deltasOf(turn.reasoning)) {
          response.write(chunk(model, { reasoning_content: part }));
        }
      }
      if (turn.text !== undefined) {
        for (const part of deltasOf(turn.text)) {
          response.write(chunk(model, { content: part }));
        }
      }
      if (turn.toolCalls !== undefined) {
        response.write(chunk(model, toolCallDelta(turn.toolCalls)));
      }
      response.write(
        chunk(model, {}, turn.toolCalls === undefined ? 'stop' : 'tool_calls'),
      );
      // The trailer `stream_options: {include_usage: true}` asks for: no
      // choices, usage only. Read from the top level, not from inside a choice.
      response.write(frame({ choices: [], usage: USAGE }));
      response.write('data: [DONE]\n\n');
      response.end();
    })();
  });

  await new Promise<void>((resolve) => {
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address() as AddressInfo;

  return {
    url: `http://127.0.0.1:${String(address.port)}`,
    close: async () => {
      for (const stop of [...held]) stop();
      server.closeAllConnections();
      await new Promise<void>((resolve) => {
        server.close(() => {
          resolve();
        });
      });
    },
  };
}
