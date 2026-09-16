/**
 * What the model says, keyed by what was said to it.
 *
 * Every spec drives the app by typing a sentence, so the sentences are the API
 * of this file: `stream a long answer` gets prose and a code fence, `list the
 * workspace` gets a tool card, `run the version command` gets an approval
 * prompt, `wait for me` gets a turn that stays in flight until something ends
 * it. A spec that types anything else gets `Ready.` — a fallback rather than a
 * throw, because most screens boot a session without ever sending a message.
 *
 * The three tools are chosen for their risk bands, not their usefulness:
 *
 *  - `list_dir` is `safe`, so it runs unattended and produces a tool card with
 *    nothing in front of it.
 *  - `exec` is `ask`, so it produces the approval prompt. Both the approve and
 *    the deny path continue into the same second turn.
 *  - `e2e_wait` is the binary's own, compiled in behind the `test-hooks`
 *    feature and armed by `DARKWIRE_TEST_HOOKS=1`. It exists because "Stop
 *    aborts mid-tool" and "a reload rebuilds an in-flight turn" both need a
 *    tool that is reliably still running a moment after it started. Sleeping on
 *    a real binary would make those two assertions depend on `sleep(1)`
 *    resolving the way this machine's coreutils resolve it; waiting on the
 *    turn's own cancellation token depends on nothing.
 *
 * This file is data and only data: the model is a server on a port, so a turn
 * is what can be put on a wire and nothing else.
 */

/** One call the model asks for. */
export interface ToolCall {
  /** The id the tool result comes back under. */
  readonly id: string;
  readonly name: string;
  /** The arguments, as an object; the wire carries them encoded. */
  readonly args: Readonly<Record<string, unknown>>;
}

/** One model turn, as far as the wire is concerned. */
export interface ScriptedTurn {
  /** The answer. Streamed in pieces; absent is a turn that only calls a tool. */
  readonly text?: string;
  /** The reasoning trace, streamed before the answer. */
  readonly reasoning?: string;
  readonly toolCalls?: readonly ToolCall[];
  /**
   * Keep the answer open and never finish it.
   *
   * The shortest path to "a turn is running" for a spec that only needs the
   * composer to be showing Stop — and over a socket that is all "the model is
   * still thinking" can mean. It ends when the client hangs up, which is what
   * Stop does.
   */
  readonly hold?: boolean;
}

/** One entry of the lookup: a sentence to match, and the turns it produces. */
export interface Route {
  /** Matched against the most recent user message. */
  readonly match: RegExp;
  /** One entry per model turn in the exchange. The last one repeats. */
  readonly turns: readonly ScriptedTurn[];
}

/** A call, spelled out so a route reads as one line. */
export function toolCall(
  id: string,
  name: string,
  args: Readonly<Record<string, unknown>>,
): ToolCall {
  return { id, name, args };
}

/** The answer whose markdown exercises the block splitter and the highlighter. */
const LONG_ANSWER = [
  'Here is what I found.\n\n',
  'The workspace holds a single note file. ',
  'Reading it back is one line:\n\n',
  '```ts\n',
  "const note = await readFile('notes.md', 'utf8');\n",
  'console.log(note.trim());\n',
  '```\n\n',
  'That is the whole of it.',
].join('');

/**
 * What the caller asks its subagent for.
 *
 * Exported because the spec asserts it: the task is the argument on the
 * delegating card *and* the sentence that routes the subagent's own turn, and a
 * test that restated it would keep passing after the two drifted apart.
 */
export const SUBAGENT_TASK = 'find the note file';

/**
 * The task whose subagent is still working when the page reloads.
 *
 * A second task rather than a flag on the first: the route is chosen by the
 * words in it, and a delegation that sometimes finished and sometimes did not
 * would make every assertion about either one depend on which spec ran.
 */
export const SUBAGENT_HELD_TASK = 'inspect the folder and hold';

export const ROUTES: readonly Route[] = [
  {
    // Prose, a fenced block and a reasoning trace: the three things the
    // transcript renders differently from each other.
    match: /\bstream\b/i,
    turns: [
      {
        reasoning: 'Checking the workspace before answering.',
        text: LONG_ANSWER,
      },
    ],
  },
  {
    // `list_dir` is `safe`, so the card appears with no prompt in front of it.
    match: /\blist\b/i,
    turns: [
      { toolCalls: [toolCall('call-list', 'list_dir', { path: '.' })] },
      { text: 'The workspace holds `notes.md`.' },
    ],
  },
  {
    // `exec` is `ask`. Approve and deny both land on the second turn — the
    // difference is what the tool result says, which is the model's problem and
    // not the transport's.
    match: /\brun\b/i,
    turns: [
      {
        toolCalls: [
          toolCall('call-exec', 'exec', { argv: ['node', '--version'] }),
        ],
      },
      { text: 'That is the runtime version.' },
    ],
  },
  {
    // In flight until Stop, or until the reload spec has finished reloading.
    match: /\bwait\b/i,
    turns: [
      { toolCalls: [toolCall('call-wait', 'e2e_wait', { ms: 60_000 })] },
      { text: 'The wait finished.' },
    ],
  },
  {
    // No tool at all — the turn stalls in the provider itself, which is the
    // shortest path to "a turn is running" for a spec that only needs the
    // composer to be showing Stop.
    match: /\bstall\b/i,
    turns: [{ hold: true }],
  },
  {
    // The caller's half of a delegation. It hands the researcher a task and
    // then answers from whatever comes back.
    match: /\bdelegate\b/i,
    turns: [
      {
        toolCalls: [
          toolCall('call-sub', 'ask_researcher', { task: SUBAGENT_TASK }),
        ],
      },
      { text: 'The researcher found `notes.md`.' },
    ],
  },
  {
    // The subagent's half, and it needs no new harness concept: a subagent's
    // first user message *is* the task string, so it routes here exactly as the
    // caller's message routes above. The word is chosen not to collide with any
    // route before it — a task containing "list" would take the `list_dir`
    // route and the delegation would silently test something else.
    match: /\bfind\b/i,
    turns: [
      { toolCalls: [toolCall('call-nested', 'list_dir', { path: '.' })] },
      { text: 'There is one file: `notes.md`.' },
    ],
  },
  {
    // The caller's half of a delegation that is still running when the page
    // reloads. Same shape as `delegate` above; a separate route so the two
    // specs cannot end up asserting each other's transcript.
    match: /\bhandover\b/i,
    turns: [
      {
        toolCalls: [
          toolCall('call-held', 'ask_researcher', { task: SUBAGENT_HELD_TASK }),
        ],
      },
      { text: 'The researcher eventually answered.' },
    ],
  },
  {
    // The subagent's half of it: do some visible work, then stop in a tool that
    // does not finish. That is what makes "reload while a delegation is in
    // flight" a durable state rather than a race — the run is held open for as
    // long as the spec needs, and everything asserted after the reload happened
    // before it.
    match: /\binspect\b/i,
    turns: [
      { toolCalls: [toolCall('call-held-list', 'list_dir', { path: '.' })] },
      {
        text: 'I checked the folder.',
        toolCalls: [toolCall('call-held-wait', 'e2e_wait', { ms: 60_000 })],
      },
      { text: 'Unreachable unless the wait ends.' },
    ],
  },
];
