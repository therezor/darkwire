/**
 * The socket's events, reduced into a transcript.
 *
 * `applyServerMessage` takes a transcript and one frame and returns the next
 * one. Pure — no store, no React, no clock — which is what lets a delta landing
 * on the right part, a tool result finding its card, and an approval being
 * answered all be tested without mounting anything.
 *
 * Two rules are load-bearing:
 *
 *  - **A notice carrying a `callId` belongs inside that tool's card.**
 *    `prompt_injection` and `approval_denied` are statements *about a call*, and
 *    floating them to the bottom of the turn separates the warning from the
 *    output it is warning about.
 *  - **A subagent's parts are reduced by the same `applyPartEvent`** as a
 *    top-level turn's, which is what makes a nested `exec` card identical to a
 *    top-level one and a subagent of a subagent free.
 */

import type { Attachment, ServerMessage } from '@darkwire/protocol';

import {
  EMPTY_TRANSCRIPT,
  type NoticePart,
  type PartEvent,
  type SubagentPart,
  type ToolPart,
  type Transcript,
  type TranscriptItem,
  type TurnItem,
  type TurnPart,
} from './shapes.js';
import { findTool, orphanTurn, seedTool, upsertTool } from './parts.js';
import { fromStoredMessages } from './stored.js';

/**
 * The bubble that appears the instant Send is pressed.
 *
 * Optimistic on purpose: the round trip to `message.ack` is short but not zero,
 * and a composer that clears while the transcript stays still for 80 ms reads as
 * a message that went nowhere. `pending` is what the ack clears.
 */
export function appendPendingUserMessage(
  items: Transcript,
  input: {
    readonly clientMessageId: string;
    readonly text: string;
    readonly attachments?: readonly Attachment[];
  },
): Transcript {
  return [
    ...items,
    {
      kind: 'user',
      id: input.clientMessageId,
      clientMessageId: input.clientMessageId,
      turnId: undefined,
      seq: undefined,
      text: input.text,
      attachments: input.attachments ?? [],
      pending: true,
    },
  ];
}

/**
 * One frame onto the transcript.
 *
 * Exhaustive over `ServerMessage`, so a protocol event added without a decision
 * here is a type error rather than a frame that silently does nothing. The
 * events that are not transcript events — `connected`, `pong`, `session.status`
 * — return the transcript unchanged; the store reads those for connection and
 * busy state.
 */
export function applyServerMessage(
  items: Transcript,
  message: ServerMessage,
): Transcript {
  // A turn that has already finished never grows again. The case this exists
  // for is a reload: the client resumes from its cursor and the ring re-sends
  // the tail of a turn that was persisted in the meantime, so the same text
  // would arrive once from storage and once as deltas.
  if (growsATurn(message) && isFinished(items, message.turnId)) return items;

  switch (message.type) {
    case 'message.ack':
      return acknowledge(items, message.messageId, message.clientMessageId);

    case 'turn.start': {
      // A `turn.start` for a turn already in the transcript is a replayed
      // frame, not a second turn — the ring re-sends by design, and appending
      // would leave an empty turn above the real one for every resume. It is
      // still worth everything it names that the existing item lacks: the seat a
      // `resumingTurnId` replay puts down knows only the id, and a turn
      // reconstructed by `orphanTurn` knows no more. Filled rather than
      // overwritten, so a replayed frame cannot blank a turn that has the real
      // values already.
      if (
        items.some((item) => item.kind === 'turn' && item.id === message.turnId)
      ) {
        const filled = updateTurn(items, message.turnId, (turn) =>
          turn.sessionKey === '' || turn.model === '' || turn.provider === ''
            ? {
                ...turn,
                sessionKey:
                  turn.sessionKey === '' ? message.sessionKey : turn.sessionKey,
                model: turn.model === '' ? message.model : turn.model,
                provider:
                  turn.provider === '' ? message.provider : turn.provider,
                firstSeq: turn.firstSeq ?? message.firstSeq,
              }
            : turn,
        );
        return stampUserSeq(filled, message.turnId, message.firstSeq);
      }
      // The optimistic bubble this tab drew has no storage address until the
      // server names one. Stamping it here rather than only at `turn.end` is
      // what lets a turn that *fails* still be re-run.
      const withSeq = stampUserSeq(items, message.turnId, message.firstSeq);
      return [
        ...withSeq,
        {
          kind: 'turn',
          id: message.turnId,
          sessionKey: message.sessionKey,
          model: message.model,
          provider: message.provider,
          parts: [],
          stopReason: undefined,
          usage: undefined,
          iterations: 0,
          elapsedMs: undefined,
          generationMs: undefined,
          generationTokens: undefined,
          firstTokenMs: undefined,
          // From `turn.start`, not only from `turn.end`: a turn that fails
          // never reaches its end, and without an address here the failure
          // could offer nothing to re-run.
          firstSeq: message.firstSeq,
          lastSeq: undefined,
          done: false,
          failure: undefined,
          // A turn this client watched start is one it has all of, but nothing
          // has promised that — the flag is a statement about a replay, and the
          // path that makes it is the one that sets it.
          authoritative: false,
        },
      ];
    }

    case 'assistant.delta':
    case 'reasoning.delta':
    case 'tool.call':
    case 'tool.progress':
    case 'tool.approvalRequest':
    case 'tool.result':
      return updateTurn(items, message.turnId, (turn) => ({
        ...turn,
        parts: applyPartEvent(turn.parts, turn.id, message),
      }));

    case 'notice':
      return applyNotice(items, message);

    case 'subagent.event':
      return applySubagentEvent(items, message);

    case 'turn.end': {
      const ended = updateTurn(items, message.turnId, (turn) => ({
        ...turn,
        done: true,
        stopReason: message.stopReason,
        usage: message.usage,
        iterations: message.iterations,
        elapsedMs: message.elapsedMs,
        generationMs: message.generationMs,
        generationTokens: message.generationTokens,
        firstTokenMs: message.firstTokenMs,
        // Kept if the end does not restate it. `turn.start` already carried it,
        // and overwriting with `undefined` would take the address back off a
        // turn that had one — which is the whole thing this is here to preserve.
        firstSeq: message.firstSeq ?? turn.firstSeq,
        lastSeq: message.lastSeq ?? turn.lastSeq,
      }));

      // The second half of the job, and the reason `firstSeq` is on the wire at
      // all: the bubble this tab drew optimistically has no storage address, so
      // until the turn ends it cannot be edited, regenerated or branched. One
      // number here is what a refetch would otherwise have to supply — and a
      // refetch would also replace the live turn's tool timings with stored
      // rows that do not carry them.
      return stampUserSeq(ended, message.turnId, message.firstSeq);
    }

    case 'error':
      // A connection-scoped error is not a transcript entry — `connection.ts`
      // raises a toast for it. A turn-scoped one belongs on the turn it killed.
      return message.turnId === undefined
        ? items
        : updateTurn(items, message.turnId, (turn) => ({
            ...turn,
            done: true,
            failure: { message: message.message, retryable: message.retryable },
          }));

    case 'steer':
      return [
        ...items,
        {
          kind: 'steer',
          id: `steer:${String(message.seq)}`,
          text: message.content,
        },
      ];

    case 'session.reset':
      return EMPTY_TRANSCRIPT;

    case 'session.replay': {
      // Two answers, and `complete` is which one this is. Covered by the ring:
      // the frames themselves follow, so there is nothing to rebuild. Past it:
      // the stored tail is the transcript, and the live frames resume on top.
      if (message.complete) return items;

      // Past the ring, storage is the base — but it cannot hold a turn that has
      // not finished, and this frame also arrives on a *reconnect*, where the
      // page never went away and that turn is on screen. Rebuilding over it
      // deleted the answer the user was watching, in front of them, mid-stream.
      //
      // So the unfinished tail is carried across, exactly as `session.truncated`
      // below carries pending bubbles: anything storage cannot describe yet, and
      // nothing storage can. On a reload there is no such tail and this is the
      // plain rebuild it was before.
      const resuming = message.resumingTurnId;
      const stored = fromStoredMessages(message.messages).filter(
        // The one turn the two sources overlap on. Its finished iterations are
        // in the tail *and* in the frames about to arrive, and the frames are
        // the fuller account — they carry the iteration still running and the
        // nested subagent runs that never enter this session's history at all.
        // The user bubble that opened it is not dropped: it is storage's alone,
        // and no frame recreates it.
        (item) => item.kind !== 'turn' || item.id !== resuming,
      );
      const known = new Set(stored.map((item) => item.id));
      const live = items.filter(
        (item) =>
          !known.has(item.id) &&
          item.id !== resuming &&
          ((item.kind === 'turn' && !item.done) ||
            (item.kind === 'user' && item.pending)),
      );
      // A seat for the frames, rather than letting them land on an `orphanTurn`
      // built by the first one that arrives. Two things need it: the flag, which
      // is a statement about this replay and not about any single frame, and the
      // ordering — the turn belongs after the question storage just supplied,
      // not after whatever else was still live.
      const seat: readonly TranscriptItem[] =
        resuming === undefined
          ? []
          : [{ ...orphanTurn(resuming), authoritative: true }];
      return [...stored, ...seat, ...live];
    }

    case 'session.truncated': {
      // The frame carries the surviving tail, so this is a rebuild rather than
      // a splice — and it is the same rebuild `session.replay` does past the
      // ring. A tab that did not initiate the regenerate corrects itself here.
      //
      // Pending bubbles survive it. An edit or a regenerate deletes the question
      // and re-runs from it, so the tail this frame carries does *not* contain
      // it — the loop has not written it back yet. The tab that asked put an
      // optimistic bubble up for exactly that gap, and rebuilding over it is
      // what made the message look lost. A tab that did not ask has no pending
      // items, so this is a no-op there.
      const pending = items.filter(
        (item) => item.kind === 'user' && item.pending,
      );
      return [...fromStoredMessages(message.messages), ...pending];
    }

    // `context.usage` is not a transcript entry: it describes the conversation
    // rather than adding to it, and the bar it moves lives under the composer.
    // `use-connection.ts` is where it lands, in the query cache the strip and
    // the inspector both read.
    case 'connected':
    case 'pong':
    case 'message.queued':
    case 'session.status':
    case 'context.usage':
    case 'notification':
    case 'tools.changed':
      return items;
  }
}

/**
 * Records this tab's answer, so the buttons go away before the round trip.
 *
 * Reaches nested cards too: a subagent's `exec` prompt renders inside the
 * delegating call, and it is answered by the same `tool.approve` frame — the
 * wire carries only the `callId`, so this has to find it wherever it is.
 */
export function markApprovalAnswered(
  items: Transcript,
  callId: string,
  answered: 'approved' | 'denied',
): Transcript {
  return items.map((item) =>
    item.kind === 'turn'
      ? { ...item, parts: answerIn(item.parts, callId, answered) }
      : item,
  );
}

function answerIn(
  parts: readonly TurnPart[],
  callId: string,
  answered: 'approved' | 'denied',
): readonly TurnPart[] {
  return parts.map((part) => {
    if (part.kind !== 'tool') return part;

    if (part.id === callId && part.approval !== undefined) {
      return {
        ...part,
        approval: { expiresAtMs: part.approval.expiresAtMs, answered },
      };
    }

    const subagent = part.subagent;
    if (subagent === undefined) return part;
    const nested = answerIn(subagent.parts, callId, answered);
    return nested === subagent.parts
      ? part
      : { ...part, subagent: { ...subagent, parts: nested } };
  });
}

/**
 * Events that add to a turn's parts, as opposed to describing or closing it.
 *
 * `turn.end` is deliberately absent: replaying it onto a turn that is already
 * closed is idempotent, and refusing it would leave a turn open forever if the
 * only copy of its ending arrived on a replay.
 */
const GROWTH_EVENTS: ReadonlySet<string> = new Set([
  'assistant.delta',
  'reasoning.delta',
  'tool.call',
  'tool.progress',
  'tool.approvalRequest',
  'tool.result',
  'notice',
  // A delegation's events grow the turn they hang off, so a replayed one has to
  // be dropped for the same reason: a finished turn's nested cards are already
  // whatever the run made them.
  'subagent.event',
]);

function growsATurn(
  message: ServerMessage,
): message is Extract<ServerMessage, { turnId?: string }> & { turnId: string } {
  return (
    GROWTH_EVENTS.has(message.type) &&
    'turnId' in message &&
    typeof message.turnId === 'string'
  );
}

function isFinished(items: Transcript, turnId: string): boolean {
  const turn = items.findLast(
    (item) => item.kind === 'turn' && item.id === turnId,
  );
  return turn?.kind === 'turn' && turn.done;
}

/**
 * Drops every item after `seq`, for the moment between asking and being told.
 *
 * Purely a flicker guard. The `session.truncated` frame that follows a
 * regenerate or an edit is the truth and rebuilds the transcript from the
 * stored tail a few milliseconds later; without this, the answer being
 * discarded stays on screen until it does.
 *
 * Items with no seq — an optimistic bubble, a steer, a connection notice — are
 * kept only while they precede the cut, because an item that has no address
 * cannot be shown to be on the surviving side of one.
 */
export function truncateTranscriptAfter(
  items: Transcript,
  seq: number,
): Transcript {
  const kept: TranscriptItem[] = [];

  for (const item of items) {
    const address =
      item.kind === 'user'
        ? item.seq
        : item.kind === 'turn'
          ? item.firstSeq
          : undefined;
    if (address !== undefined && address > seq) break;
    kept.push(item);
  }

  return kept;
}

/**
 * The ack, which is also the reconciliation.
 *
 * Two sources describe the same sentence: the optimistic bubble this client
 * created, keyed on a `clientMessageId` only it has ever seen, and the stored
 * row the REST history returns, keyed on a row id only the server has ever
 * seen. `messageId` — the turn id — is the one value both carry, so this is
 * where they are joined.
 *
 * And it has to work in both orders, because both happen: the ack usually beats
 * the history fetch, and on a reload it does not. If storage has already
 * supplied the message, the optimistic bubble is dropped; otherwise it is
 * relabelled with the turn id so a fetch that lands later recognises it.
 */
function acknowledge(
  items: Transcript,
  messageId: string,
  clientMessageId: string | undefined,
): Transcript {
  const pending = items.findIndex(
    (item) =>
      item.kind === 'user' &&
      item.pending &&
      item.clientMessageId === clientMessageId,
  );
  if (pending === -1) return items;

  const alreadyStored = items.some(
    (item, index) =>
      index !== pending && item.kind === 'user' && item.turnId === messageId,
  );

  if (alreadyStored) return items.filter((item, index) => index !== pending);

  const copy = [...items];
  const item = copy[pending];
  if (item?.kind !== 'user') return items;
  // The id is left alone: it is a React key, and the turn id already belongs to
  // the turn item this message started. Only the turn id and `pending` are news.
  copy[pending] = { ...item, turnId: messageId, pending: false };
  return copy;
}

function applyNotice(
  items: Transcript,
  message: Extract<ServerMessage, { type: 'notice' }>,
): Transcript {
  const id = `notice:${String(message.seq)}`;
  const notice: NoticePart = {
    kind: 'notice',
    id,
    notice: message.kind,
    message: message.message,
  };

  if (message.turnId === undefined) {
    return [
      ...items,
      { kind: 'notice', id, notice: message.kind, message: message.message },
    ];
  }

  return updateTurn(items, message.turnId, (turn) => ({
    ...turn,
    parts: appendNotice(turn.parts, turn.id, notice, message.callId),
  }));
}

/**
 * One event from a subagent, into the card of the call that started it.
 *
 * The address is `parentSessionKey` + `parentCallId`, and both halves are
 * needed: a `callId` is the model's and is only unique within one assistant
 * message, so a subagent can mint the one its caller just used. Composing it
 * with the session that emitted it is unique at any depth and needs no ids
 * rewritten on the way through.
 *
 * The search is over the whole part tree of the turn rather than only its top
 * level, which is what makes a subagent of a subagent free: a depth-2 event
 * names its parent's call in its parent's session, and that call is a `ToolPart`
 * sitting inside a `SubagentPart` — findable by the same walk.
 */
function applySubagentEvent(
  items: Transcript,
  message: Extract<ServerMessage, { type: 'subagent.event' }>,
): Transcript {
  return updateTurn(items, message.turnId, (opened) => {
    // A turn rebuilt by `orphanTurn` has no session key, and the address below
    // is half made of one — so without this the match fails on every frame and
    // a delegation the user reloaded into is invisible for the rest of the
    // turn. At depth 1 the parent session *is* this turn's, which is the only
    // depth that can say so: deeper frames name a child's session. Filled only
    // when it is missing, so a `turn.start` that did arrive still wins.
    const turn =
      opened.sessionKey === '' && message.depth === 1
        ? { ...opened, sessionKey: message.parentSessionKey }
        : opened;

    const parts = updateNestedTool(
      turn.parts,
      turn.sessionKey,
      message,
      (tool) => {
        const subagent = tool.subagent ?? seedSubagent(message);
        return {
          ...tool,
          subagent: applySubagentPart(subagent, message.event),
        };
      },
    );
    return parts === turn.parts ? turn : { ...turn, parts };
  });
}

/**
 * The tool name a delegation to `agentId` runs under.
 *
 * Restated from `subagent_definition` in `darkwire-agent` rather than imported,
 * for the same reason the nonce envelope in `parts.ts` is: that crate is Rust,
 * so the browser cannot reach it, and four characters of duplication is the
 * cheaper half of the trade. Only ever used to label a card rebuilt after a
 * reload — when the `tool.call` arrived, its own name is the one on screen.
 */
function delegationToolName(agentId: string): string {
  return `ask_${agentId.replaceAll('-', '_')}`;
}

function seedSubagent(
  message: Extract<ServerMessage, { type: 'subagent.event' }>,
): SubagentPart {
  return {
    agentId: message.agentId,
    label: message.label === '' ? message.agentId : message.label,
    sessionKey: message.sessionKey,
    parts: [],
    model: '',
    stopReason: undefined,
    usage: undefined,
    iterations: 0,
    elapsedMs: undefined,
    done: false,
    // Live events *are* the whole run, so nothing needs fetching.
    loaded: true,
    // The delegating call is on screen, so its run began where this client was
    // watching. The other seeding path — a card invented because the call was
    // never seen — sets this itself. See `updateNestedTool`.
    partial: false,
  };
}

function applySubagentPart(
  subagent: SubagentPart,
  event: Extract<ServerMessage, { type: 'subagent.event' }>['event'],
): SubagentPart {
  switch (event.type) {
    case 'turn.start':
      return { ...subagent, model: event.model };

    case 'turn.end':
      return {
        ...subagent,
        done: true,
        stopReason: event.stopReason,
        usage: event.usage,
        iterations: event.iterations,
        elapsedMs: event.elapsedMs,
      };

    case 'error':
      // A subagent that failed is a finished subagent. Its message reaches the
      // reader through the delegating call's own error result, so there is
      // nothing to render twice.
      return { ...subagent, done: true };

    case 'notice':
      return {
        ...subagent,
        parts: appendNotice(
          subagent.parts,
          subagent.sessionKey,
          {
            kind: 'notice',
            id: `${subagent.sessionKey}#${String(subagent.parts.length)}`,
            notice: event.kind,
            message: event.message,
          },
          event.callId,
        ),
      };

    default:
      return {
        ...subagent,
        parts: applyPartEvent(subagent.parts, subagent.sessionKey, event),
      };
  }
}

/**
 * Finds the delegating call anywhere in a part tree and updates it, creating it
 * when this client never saw it made.
 *
 * The creation path is the reload case, and it is not rare: the replay ring
 * holds 512 frames and a subagent emits one per token, so a tab that refreshes
 * mid-delegation comes back past the `tool.call` that started it. Returning
 * `parts` unchanged there would leave the run invisible until the turn ended.
 * The wrapper carries `agentId`, and the tool a delegation runs under is
 * `ask_<agentId>` with hyphens replaced, so the heading is derivable and
 * correct — this is not a subagent's transcript under a heading that guesses.
 *
 * Still returns `parts` unchanged when the address names a *different* session
 * — that is the recursion missing its subtree, not an absent card.
 */
function updateNestedTool(
  parts: readonly TurnPart[],
  sessionKey: string,
  message: Extract<ServerMessage, { type: 'subagent.event' }>,
  update: (tool: ToolPart) => ToolPart,
): readonly TurnPart[] {
  if (
    sessionKey === message.parentSessionKey &&
    findTool(parts, message.parentCallId) === undefined
  ) {
    return [
      ...parts,
      update(
        seedTool(
          message.parentCallId,
          delegationToolName(message.agentId),
          undefined,
          'safe',
          'running',
          // Marked here, and this is the only place that can know: a card
          // invented because its call was missed is by definition holding the
          // middle of a run. `session.replay` answers an incomplete resume by
          // sending the stored tail and delivering no frames at all, so the
          // absence of the `tool.call` is not a dropped frame — it is the whole
          // beginning of the delegation. See `SubagentPart.partial`.
          { ...seedSubagent(message), partial: true },
        ),
      ),
    ];
  }

  const next = parts.map((part) => {
    if (part.kind !== 'tool') return part;

    if (
      sessionKey === message.parentSessionKey &&
      part.id === message.parentCallId
    ) {
      return update(part);
    }

    // One level down: this call's own subagent, whose parts may hold the target.
    const subagent = part.subagent;
    if (subagent === undefined) return part;
    const nested = updateNestedTool(
      subagent.parts,
      subagent.sessionKey,
      message,
      update,
    );
    return nested === subagent.parts
      ? part
      : { ...part, subagent: { ...subagent, parts: nested } };
  });

  // Identity, not a flag: a `let` assigned inside the callback is not something
  // the compiler will narrow afterwards, and every branch above already returns
  // the original object when it changed nothing. Returning `parts` unchanged is
  // what tells `applySubagentEvent` the call was not in this subtree.
  return next.some((part, index) => part !== parts[index]) ? next : parts;
}

/**
 * A notice into a parts list — inside a call's card when it names one.
 *
 * A `prompt_injection` or `approval_denied` notice is a statement *about a
 * call*, and floating it to the bottom of the turn separates the warning from
 * the output it is warning about.
 */
function appendNotice(
  parts: readonly TurnPart[],
  scopeId: string,
  notice: NoticePart,
  callId: string | undefined,
): readonly TurnPart[] {
  if (callId !== undefined) {
    return upsertTool(parts, callId, (tool) => ({
      ...tool,
      notices: [...tool.notices, notice],
    }));
  }
  return [...parts, { ...notice, id: `${scopeId}#${String(parts.length)}` }];
}

/**
 * Gives this turn's optimistic user bubble its storage address.
 *
 * Idempotent, and only ever fills a gap: `seq === undefined` is the guard, so a
 * replayed frame or a second stamping cannot renumber a bubble that a fetch has
 * already placed. Called from both `turn.start` and `turn.end` because the two
 * cover different failures — the end never arrives for a turn that threw, and the
 * start does not know the seq on an install whose server predates carrying it.
 */
function stampUserSeq(
  items: readonly TranscriptItem[],
  turnId: string,
  seq: number | undefined,
): readonly TranscriptItem[] {
  if (seq === undefined) return items;
  return items.map((item) =>
    item.kind === 'user' && item.turnId === turnId && item.seq === undefined
      ? { ...item, seq }
      : item,
  );
}

/**
 * Appends a delta to the trailing part of its kind, or opens a new one.
 *
 * "Trailing" and not "any": a model that writes, calls a tool, then writes again
 * produces two text parts, and merging the second into the first would render
 * the tool card after text it came before.
 */
function appendText(
  parts: readonly TurnPart[],
  scopeId: string,
  kind: 'text' | 'reasoning',
  text: string,
): readonly TurnPart[] {
  if (text === '') return parts;

  const last = parts.at(-1);
  if (last?.kind === kind) {
    return [...parts.slice(0, -1), { ...last, text: last.text + text }];
  }
  return [...parts, { kind, id: `${scopeId}#${String(parts.length)}`, text }];
}

/**
 * One part-level event onto a list of parts.
 *
 * Shared by a turn and by a subagent's run inside a tool card, which is the
 * whole reason it takes `parts` rather than a transcript: a nested `exec` card
 * is not a second rendering of a tool call, it is the same one at a different
 * place in the tree. `scopeId` only names the ids of parts created here, so it
 * can be a turn id or a subagent's session key without meaning anything else.
 */
function applyPartEvent(
  parts: readonly TurnPart[],
  scopeId: string,
  event: PartEvent,
): readonly TurnPart[] {
  switch (event.type) {
    case 'assistant.delta':
      return appendText(parts, scopeId, 'text', event.text);

    case 'reasoning.delta':
      return appendText(parts, scopeId, 'reasoning', event.text);

    case 'tool.call':
      // A `tool.call` for a call already here is a replayed frame, not a second
      // call: the ids are unique per turn and the ring re-sends.
      return findTool(parts, event.callId) !== undefined
        ? parts
        : [
            ...parts,
            seedTool(event.callId, event.name, event.args, event.risk),
          ];

    case 'tool.progress':
      return upsertTool(parts, event.callId, (tool) => ({
        ...tool,
        // Monotonic: a replayed heartbeat must not wind the counter back.
        elapsedMs: Math.max(tool.elapsedMs, event.elapsedMs),
      }));

    case 'tool.approvalRequest':
      return upsertTool(parts, event.callId, (tool) => ({
        ...tool,
        status: 'awaiting-approval',
        approval: { expiresAtMs: event.expiresAtMs, answered: undefined },
      }));

    case 'tool.result':
      return upsertTool(parts, event.callId, (tool) => ({
        ...tool,
        status: event.ok ? 'ok' : 'error',
        // The tool's own output. The stored copy is wrapped in the turn's nonce
        // envelope; this one deliberately is not.
        content: event.content,
        truncated: event.truncated,
        durationMs: event.durationMs,
        approval: undefined,
      }));
  }
}

/**
 * Applies `update` to a turn, creating it if this is the first frame seen of it.
 *
 * The creation path is not defensive padding: a tab that connects mid-turn and
 * resumes from the ring receives deltas for a `turn.start` that fell outside the
 * buffer. Dropping them would render an empty turn.
 */
function updateTurn(
  items: Transcript,
  turnId: string,
  update: (turn: TurnItem) => TurnItem,
): Transcript {
  const index = items.findLastIndex(
    (item) => item.kind === 'turn' && item.id === turnId,
  );

  if (index === -1) {
    return [...items, update(orphanTurn(turnId))];
  }

  const turn = items[index];
  if (turn?.kind !== 'turn') return items;

  const next = update(turn);
  if (next === turn) return items;

  const copy = [...items];
  copy[index] = next;
  return copy;
}
