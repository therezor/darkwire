/**
 * The extension contract with nothing else in it.
 *
 * `examples/loopback-channel` does this for a channel — the transport removed,
 * so what is left is the contract. This is the same thing for an extension: it
 * contributes one of each of three kinds, reaches for nothing a real extension
 * would not, and has no dependencies at all. Node and `node:readline` are the
 * whole toolchain; there is nothing here to build.
 *
 * An extension is a **child process** speaking JSON-RPC 2.0 over its own stdio,
 * one JSON object per line, which is exactly MCP's stdio transport. That has a
 * consequence worth stating before anything else: a plain MCP server — one that
 * has never heard of DarkWire — is already a valid tools-only extension. The
 * `darkwire/` methods below are additions on top of a handshake it already
 * speaks, and a server that answers `-32601` to all of them still works.
 *
 * Five things a first-time reader usually gets wrong, each shown rather than
 * described:
 *
 *  - **stdout is the wire. Never print to it.** A stray `console.log` is a
 *    protocol error. Diagnostics go to stderr, which the host drains into its
 *    own log under the target `extension.hello`.
 *  - **Everything the host tells you arrives in `initialize`.** The settings
 *    block, the data directory and the extension id are under
 *    `params._meta.darkwire` — the field MCP reserves for exactly this. There is
 *    no environment to read beyond the two variables named below, and no
 *    config file to find.
 *  - **`contributes` in the manifest has to match what this answers.** The host
 *    drops a registration whose kind the manifest never declared and puts a
 *    warning on the extension's row; it also warns the other way, when a
 *    declared kind answers `-32601`.
 *  - **Every id is namespaced.** The command is `hello-time`, not `time`,
 *    because two extensions must not be able to fight over a slash command. The
 *    tool alone is exempt in *spelling*: the host rewrites `greet` to
 *    `ext_hello_greet` on the way in, since tool names have their own character
 *    class.
 *  - **Exit when stdin closes.** That is how the host asks a child to stop. It
 *    follows with SIGTERM and then SIGKILL, so a process that ignores the
 *    closed pipe is killed rather than waited for.
 *
 * Nothing here writes to the data directory, and that is the last thing worth
 * copying: an extension that writes on startup fails on an install where that
 * directory does not exist yet, so state is written lazily or not at all.
 */

import { createInterface } from 'node:readline';

/** Filled from `initialize`. Read once, exactly like a v1 `activate` did. */
let greeting = 'Hello';

const TOOLS = [
  {
    name: 'greet',
    description:
      'Greet someone by name. Registered by the hello extension; harmless.',
    inputSchema: {
      type: 'object',
      properties: {
        who: { type: 'string', description: 'The name to greet.' },
      },
      required: ['who'],
      additionalProperties: false,
    },
    // The one hint the host believes at face value: this tool only reads.
    annotations: { readOnlyHint: true },
  },
];

const COMMANDS = [
  {
    id: 'hello-time',
    description: 'Show the time this install thinks it is.',
    argsHint: '',
  },
];

/** One line out. The only thing in this file allowed to touch stdout. */
function send(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function reply(id, result) {
  send({ jsonrpc: '2.0', id, result });
}

function fail(id, code, message) {
  send({ jsonrpc: '2.0', id, error: { code, message } });
}

function initialize(params) {
  // Absent when a plain MCP client connects, which is a case worth surviving:
  // this file is runnable under any MCP inspector.
  const darkwire = params?._meta?.darkwire;
  if (typeof darkwire?.settings?.greeting === 'string') {
    greeting = darkwire.settings.greeting;
  }
  return {
    protocolVersion: params?.protocolVersion ?? '2025-06-18',
    capabilities: { tools: {} },
    serverInfo: { name: 'hello', version: '2.0.0' },
  };
}

function callTool(params) {
  if (params?.name !== 'greet') {
    // A tool that does not exist is a *result*, not an error: the model reads
    // what went wrong and adapts, instead of the turn dying.
    return {
      content: [{ type: 'text', text: `No tool called "${params?.name}"` }],
      isError: true,
    };
  }
  const who = params?.arguments?.who;
  if (typeof who !== 'string' || who.length === 0) {
    return {
      content: [{ type: 'text', text: '"who" is required.' }],
      isError: true,
    };
  }
  return { content: [{ type: 'text', text: `${greeting}, ${who}!` }] };
}

/**
 * The system-prompt section.
 *
 * The agent reads this on every turn, so it is one short paragraph — a
 * contributor that wrote a page would cost that page in every request of every
 * turn on this install.
 */
function staticContext() {
  return {
    sections: [
      {
        title: 'Hello',
        body:
          'An example extension is installed. Its `ext_hello_greet` tool ' +
          'greets someone by name, and does nothing else.',
      },
    ],
  };
}

function runCommand(params) {
  if (params?.id !== 'hello-time') {
    return { message: `No command called "${params?.id}"`, ok: false };
  }
  return {
    message: `${greeting} — it is ${new Date().toISOString()}`,
    ok: true,
  };
}

/** Every method this extension answers. Anything else is `-32601`. */
const METHODS = {
  initialize,
  'tools/list': () => ({ tools: TOOLS }),
  'tools/call': callTool,
  'darkwire/context/static': staticContext,
  'darkwire/commands/list': () => ({ commands: COMMANDS }),
  'darkwire/commands/run': runCommand,
};

function handle(line) {
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }
  // A notification carries no id and is never answered — including
  // `notifications/initialized` and `notifications/cancelled`.
  if (message.id === undefined || message.id === null) return;

  const method = METHODS[message.method];
  if (method === undefined) {
    fail(message.id, -32601, `Method not found: ${message.method}`);
    return;
  }
  try {
    reply(message.id, method(message.params));
  } catch (error) {
    fail(
      message.id,
      -32603,
      error instanceof Error ? error.message : String(error),
    );
  }
}

const lines = createInterface({ input: process.stdin });
lines.on('line', handle);
// The host closes stdin to ask for a stop. Leaving without waiting is the
// whole of a well-behaved shutdown; anything slower gets SIGTERM.
lines.on('close', () => process.exit(0));
