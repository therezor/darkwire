/**
 * A stored history, rebuilt into the same transcript the socket produces.
 *
 * The second of the two constructors, and the reason `merge` exists: a REST or
 * replay history describes the same conversation the socket does, in a
 * different shape, with no id in common. `mergeStoredHistory` reconciles them
 * on two keys because neither alone identifies an item across both sources.
 *
 * **The nonce envelope is never rendered.** `tool.result` carries the tool's
 * own output, but stored history carries the wrapped form, so this unwraps it.
 * The delimiters are a defence aimed at the model; showing them to a user would
 * be showing them the machinery.
 */

import type { StoredMessage, SubagentRunRef } from '@darkwire/protocol';

import {
  type ToolPart,
  type Transcript,
  type TranscriptItem,
  type TurnPart,
} from './shapes.js';
import {
  attachmentsOf,
  findTool,
  openTurn,
  parseArgs,
  replaceLast,
  seedTool,
  textOf,
  unloadedSubagent,
  unwrapToolOutput,
} from './parts.js';

/**
 * A fetched history under whatever the socket has already built on top of it.
 *
 * The two arrive in either order and neither can wait for the other: the REST
 * request and the WebSocket handshake are started in the same tick, and on a
 * reload the replay of an in-flight turn routinely beats the fetch of the
 * conversation it belongs to. Replacing the transcript would discard the turn
 * the user is watching; appending would render every completed message twice.
 *
 * So it is a merge, keyed on id: the stored history is the base, and anything
 * live that storage has not caught up with keeps its place after it.
 */
export function mergeStoredHistory(
  existing: Transcript,
  messages: readonly StoredMessage[],
  subagentRuns: Readonly<Record<string, SubagentRunRef>> = {},
  failures: Readonly<Record<string, string>> = {},
): Transcript {
  const base = withAuthoritativeTurns(
    withLiveRiskBands(
      fromStoredMessages(messages, subagentRuns, failures),
      existing,
    ),
    existing,
  );

  const ids = new Set(base.map((item) => item.id));
  // The second key, and the one that does the real work: a message sent a
  // moment ago is in the transcript under the turn id the ack gave it and in
  // storage under a row id nothing on the client has ever seen. The turn id is
  // all they share.
  const turns = new Set(
    base.flatMap((item) =>
      item.kind === 'user' && item.turnId !== undefined ? [item.turnId] : [],
    ),
  );

  const merged: TranscriptItem[] = [...base];
  for (const item of existing) {
    if (ids.has(item.id)) continue;
    if (
      item.kind === 'user' &&
      item.turnId !== undefined &&
      turns.has(item.turnId)
    ) {
      continue;
    }
    // Deduping against what has already been kept, not only against the base,
    // makes this idempotent — and the history query can resolve more than once.
    ids.add(item.id);
    if (item.kind === 'user' && item.turnId !== undefined) {
      turns.add(item.turnId);
    }
    merged.push(item);
  }

  return merged;
}

/**
 * The turns where the socket outranks storage, put back whole.
 *
 * Every other item in the merge takes storage as the base, and that is the right
 * default: a stored row is finished and a live one is whatever this tab has
 * watched, so preferring the live copy in general would trade history for a
 * tail. There are two exceptions, and both are cases where the live copy is the
 * *superset* rather than the tail.
 *
 * The first is a turn the server has just replayed in full — see
 * `TurnItem.authoritative`. It holds the iteration that has not been written
 * yet, and it holds the nested subagent runs that never enter this session's
 * history at all, since a delegation's steps are rows in the *child's* session.
 *
 * The second is a fetch that was started before the turn finished and answered
 * after it. That is not a hypothetical: the socket mints a session key on the
 * first message, the URL gains it, and the history request that fires on the
 * change is in flight while the turn is still streaming. Against a server on
 * the same machine it lands in a millisecond and holds everything; against one
 * a network away — or a busy one — it lands holding the rows that existed when
 * it was *asked*, which is a turn missing its last answer. Taking that as the
 * base deletes text this tab has already displayed, and no later fetch puts it
 * back: the invalidation that would have refetched fires while this request is
 * still open and is deduplicated into it. The screen keeps a truncated turn
 * until something makes it reload.
 *
 * So a stored rebuild that knows *less* about a turn than the socket does never
 * replaces it. Counting parts is the whole test, and it is the right one in
 * both directions: storage catching up produces at least as many, at which
 * point it wins and `withLiveRiskBands` restores the detail rows cannot carry.
 *
 * A whole-item replacement rather than a field-by-field merge, and that is the
 * point rather than a shortcut. The card a delegation renders under hangs off a
 * `ToolPart` that storage has no row for until the whole iteration finishes —
 * `withLiveRiskBands` above can only copy a live `subagent` onto a stored tool
 * part that already exists, so a merge would drop exactly the thing being
 * rescued. Replacing the item leaves that function inert for this turn, which is
 * the correct amount of work for it to do here.
 *
 * The stored item's *position* is kept: the tail it came from is what puts the
 * turn after the question that started it.
 */
function withAuthoritativeTurns(
  base: Transcript,
  existing: Transcript,
): Transcript {
  const live = new Map<string, TranscriptItem>();
  for (const item of existing) {
    if (item.kind === 'turn') live.set(item.id, item);
  }
  if (live.size === 0) return base;

  return base.map((item) => {
    if (item.kind !== 'turn') return item;
    const watched = live.get(item.id);
    if (watched?.kind !== 'turn') return item;
    const outranks =
      watched.authoritative || watched.parts.length > item.parts.length;
    return outranks ? watched : item;
  });
}

/**
 * Puts back the things storage cannot remember.
 *
 * Two of them, and they fail the same way — a call this tab *watched happen* is
 * rebuilt from a row that never held the detail, so the screen quietly loses
 * something it was already showing:
 *
 *  - **The risk band.** It was a property of the registry at call time; the row
 *    holds the model's arguments and the result. `fromStoredMessages` fills in
 *    `safe`, which is honest for a conversation loaded fresh and wrong for one
 *    being watched — an `exec` the user was asked to approve would be
 *    relabelled `read` the instant its turn landed in the database.
 *  - **The subagent's run.** A delegation's nested transcript lives in the
 *    child's own session, not in the parent's history, so a rebuild would drop
 *    it — and the history query is invalidated on `turn.end`, which means the
 *    run vanished a moment after it finished, in front of the operator.
 *
 * Keyed on the call id, which the socket and the row genuinely share — unlike
 * the message id, which is why `mergeStoredHistory` needs a second key at all.
 *
 * The subagent case has one exception, and it is the whole reason
 * `SubagentPart.partial` exists. A client that reloaded mid-delegation holds the
 * *tail* of a run — right to render while it streams, wrong to preserve. Storage
 * can supply the whole thing by then, so a partial card yields to the rebuild's
 * `unloadedSubagent` ref and the card goes back to offering the full fetch.
 * Carrying it over instead would freeze the half this tab happened to catch.
 */
function withLiveRiskBands(base: Transcript, existing: Transcript): Transcript {
  const live = new Map<string, ToolPart>();
  for (const item of existing) {
    if (item.kind !== 'turn') continue;
    for (const part of item.parts) {
      if (part.kind === 'tool') live.set(part.id, part);
    }
  }
  if (live.size === 0) return base;

  return base.map((item) => {
    if (item.kind !== 'turn') return item;
    return {
      ...item,
      parts: item.parts.map((part) => {
        if (part.kind !== 'tool') return part;
        const watched = live.get(part.id);
        if (watched === undefined) return part;
        // Yielding to storage is only an improvement when storage has something
        // to yield to. `rememberSubagentRun` writes the parent's pointer
        // best-effort and swallows its failures, so `subagentRuns` can be
        // missing an entry for a run that genuinely happened — and swapping a
        // half card for nothing at all would make the run vanish at the moment
        // it finished.
        const stored = part.subagent;
        const subagent =
          watched.subagent?.partial === true && stored !== undefined
            ? stored
            : watched.subagent;
        return { ...part, risk: watched.risk, subagent };
      }),
    };
  });
}

/**
 * A stored history as a transcript.
 *
 * The grouping key is `turnId`: one user message and everything the turn
 * produced, however many provider iterations it took. A stored message with no
 * `turnId` — written before turns were grouped, or by a channel that did not
 * set one — falls back to its own id, which renders it as a turn of one.
 */
export function fromStoredMessages(
  messages: readonly StoredMessage[],
  subagentRuns: Readonly<Record<string, SubagentRunRef>> = {},
  failures: Readonly<Record<string, string>> = {},
): Transcript {
  const items: TranscriptItem[] = [];

  // The seqs each turn spans, keyed the same way `openTurn` keys the turn
  // itself. Collected up front because a turn's first seq belongs to the *user*
  // message that started it, which the loop below has already passed by the
  // time it opens the turn.
  const spans = new Map<string, { first: number; last: number }>();
  /** Turns that produced an answer, so the loop below knows which did not. */
  const answered = new Set<string>();
  for (const stored of messages) {
    const key = stored.turnId ?? stored.id;
    const span = spans.get(key);
    spans.set(
      key,
      span === undefined
        ? { first: stored.seq, last: stored.seq }
        : {
            first: Math.min(span.first, stored.seq),
            last: Math.max(span.last, stored.seq),
          },
    );
    if (stored.message.role === 'assistant') answered.add(key);
  }

  for (const stored of messages) {
    const { message } = stored;

    switch (message.role) {
      // Never rendered. It is the agent's instructions, not the conversation.
      case 'system':
        continue;

      case 'user':
        items.push({
          kind: 'user',
          id: stored.id,
          clientMessageId: undefined,
          turnId: stored.turnId,
          seq: stored.seq,
          text: textOf(message.content),
          attachments: attachmentsOf(message.content),
          pending: false,
        });
        // A turn that failed appended no answer, so nothing below would ever
        // open one — and the failure would have nowhere to render. Opened here,
        // directly after the question, which is where it belongs. Guarded on
        // there being no assistant row, so a turn that *did* answer is still
        // opened by the branch below and there is one turn rather than two.
        if (
          stored.turnId !== undefined &&
          failures[stored.turnId] !== undefined &&
          !answered.has(stored.turnId)
        ) {
          openTurn(items, stored.turnId);
        }
        continue;

      case 'assistant': {
        const turn = openTurn(items, stored.turnId ?? stored.id);
        const parts: TurnPart[] = [...turn.parts];

        if (message.reasoning !== undefined && message.reasoning !== '') {
          parts.push({
            kind: 'reasoning',
            id: `${turn.id}#${String(parts.length)}`,
            text: message.reasoning,
          });
        }

        const text = textOf(message.content);
        if (text !== '') {
          parts.push({
            kind: 'text',
            id: `${turn.id}#${String(parts.length)}`,
            text,
          });
        }

        for (const call of message.toolCalls) {
          // A delegation, if this call made one. The steps are in the child's
          // own session, so all that can be built here is the card and the
          // address to fetch them from — `loaded: false` is what makes the
          // difference between "did nothing" and "not fetched yet" visible.
          const run = subagentRuns[call.id];
          parts.push(
            seedTool(
              call.id,
              call.name,
              // The stored form is the verbatim string the model emitted, which
              // is not always JSON. The card renders whichever it turns out to be.
              parseArgs(call.argumentsJson),
              // Stored history does not carry the risk band — it was a property
              // of the registry at call time. `safe` is the honest answer, and
              // the result beside it is what the reader is actually looking at.
              'safe',
              'running',
              run === undefined ? undefined : unloadedSubagent(run),
            ),
          );
        }

        replaceLast(items, { ...turn, parts, done: true });
        continue;
      }

      case 'tool': {
        // The result pairs with the call by id, wherever that call ended up:
        // `findLegalStart` guarantees the assistant turn that made it is in the
        // same history, but not that it is in the last item.
        const index = items.findLastIndex(
          (item) =>
            item.kind === 'turn' &&
            findTool(item.parts, message.toolCallId) !== undefined,
        );
        const turn = items[index];
        if (turn?.kind !== 'turn') continue;

        items[index] = {
          ...turn,
          parts: turn.parts.map((part) =>
            part.kind === 'tool' && part.id === message.toolCallId
              ? {
                  ...part,
                  status: message.isError ? 'error' : 'ok',
                  content: unwrapToolOutput(message.content),
                  truncated: message.truncated,
                }
              : part,
          ),
        };
        continue;
      }
    }
  }

  return items.map((item) => {
    if (item.kind !== 'turn') return item;
    const failure = failures[item.id];
    return {
      ...item,
      firstSeq: spans.get(item.id)?.first,
      lastSeq: spans.get(item.id)?.last,
      // Same shape as a failure that arrived over the socket, so a turn read
      // back from storage and one watched live are indistinguishable.
      ...(failure === undefined
        ? {}
        : { failure: { message: failure, retryable: true } }),
    };
  });
}
