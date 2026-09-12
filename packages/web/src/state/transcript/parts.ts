/**
 * The shapes both builders construct, and the lookups both need.
 *
 * `live.ts` reduces socket frames and `stored.ts` rebuilds from REST rows, and
 * they arrive at the same `TurnPart`s by different routes. These are the pieces
 * they genuinely share — seeding a tool card, finding one by `callId`, opening
 * a turn, reading text and attachments off a stored message. Shared rather than
 * written twice, because a tool card that looks slightly different depending on
 * whether you watched it happen or reloaded the page is exactly the bug the
 * merge in `stored.ts` then has to paper over.
 */

import type {
  Attachment,
  ContentPart,
  SubagentRunRef,
  ToolRisk,
} from '@ghostwire/protocol';

import {
  type SubagentPart,
  type ToolPart,
  type ToolStatus,
  type TranscriptItem,
  type TurnItem,
  type TurnPart,
} from './shapes.js';

/**
 * The nonce envelope, as the agent writes it.
 *
 * Duplicated from `nonce.rs` in `ghostai-security` rather than imported: that
 * crate is Rust, so the browser cannot reach it, and restating one delimiter is
 * the cheaper half of the trade. `transcript.test.ts` pins the exact shape.
 */
const TOOL_OUTPUT_ENVELOPE =
  /^<tool_output_([0-9a-f]{8,})\b[^>]*>\n([\s\S]*)\n<\/tool_output_\1>$/i;

/**
 * The tool's own output, out of the envelope the model saw it in.
 *
 * Content that is not an envelope is returned untouched — a tool message stored
 * before the wrapping existed, or one that failed before it was wrapped, is
 * still a tool message worth rendering.
 */
export function unwrapToolOutput(content: string): string {
  const match = TOOL_OUTPUT_ENVELOPE.exec(content);
  if (match?.[2] === undefined) return content;

  // `wrapToolOutput` escapes any delimiter the content itself contained, so
  // that a tool cannot appear to close the envelope early. Undoing it here is
  // what stops a backslash appearing in the rendered output.
  return match[2].replace(/<\\(\/?)tool_output_/gi, '<$1tool_output_');
}

export function unloadedSubagent(run: SubagentRunRef): SubagentPart {
  return {
    agentId: run.agentId,
    label: run.label === '' ? run.agentId : run.label,
    sessionKey: run.sessionKey,
    parts: [],
    model: '',
    stopReason: undefined,
    usage: undefined,
    iterations: 0,
    elapsedMs: undefined,
    done: true,
    loaded: false,
    // Nothing is being shown, so there is no half of anything on screen. The
    // card offers to fetch the whole run, which is the opposite of partial.
    partial: false,
  };
}

export function seedTool(
  callId: string,
  name: string,
  args: unknown,
  risk: ToolRisk,
  status: ToolStatus = 'running',
  subagent?: SubagentPart,
): ToolPart {
  return {
    kind: 'tool',
    id: callId,
    name,
    args,
    risk,
    status,
    elapsedMs: 0,
    durationMs: undefined,
    content: undefined,
    truncated: false,
    approval: undefined,
    notices: [],
    subagent,
  };
}

/**
 * Applies `update` to a call's part, creating it if this client never saw it.
 *
 * The creation path is not defensive padding: a resume can land between a call
 * and its result, and an empty card is better than an output with no name on it.
 */
export function upsertTool(
  parts: readonly TurnPart[],
  callId: string,
  update: (tool: ToolPart) => ToolPart,
): readonly TurnPart[] {
  const tool = findTool(parts, callId);
  if (tool === undefined) {
    return [...parts, update(seedTool(callId, 'tool', undefined, 'safe'))];
  }
  const next = update(tool);
  return next === tool
    ? parts
    : parts.map((part) => (part === tool ? next : part));
}

export function findTool(
  parts: readonly TurnPart[],
  callId: string,
): ToolPart | undefined {
  for (const part of parts) {
    if (part.kind === 'tool' && part.id === callId) return part;
  }
  return undefined;
}

export function orphanTurn(turnId: string): TurnItem {
  return {
    kind: 'turn',
    id: turnId,
    sessionKey: '',
    model: '',
    provider: '',
    parts: [],
    stopReason: undefined,
    usage: undefined,
    iterations: 0,
    elapsedMs: undefined,
    generationMs: undefined,
    generationTokens: undefined,
    firstTokenMs: undefined,
    firstSeq: undefined,
    lastSeq: undefined,
    done: false,
    failure: undefined,
    authoritative: false,
  };
}

export function openTurn(items: TranscriptItem[], turnId: string): TurnItem {
  const last = items.at(-1);
  if (last?.kind === 'turn' && last.id === turnId) return last;

  const turn: TurnItem = { ...orphanTurn(turnId), done: true };
  items.push(turn);
  return turn;
}

export function replaceLast(
  items: TranscriptItem[],
  item: TranscriptItem,
): void {
  items[items.length - 1] = item;
}

/**
 * The words in a message, which is what the bubble shows.
 *
 * File parts contribute nothing here on purpose — they are rendered as chips by
 * `attachmentsOf` below. It is also what keeps `[Attachment: notes.csv
 * (text/csv)]` out of a reloaded bubble as literal text: a non-image attachment
 * with nowhere else to go ends up synthesised into the words.
 */
export function textOf(content: readonly ContentPart[]): string {
  return content
    .map((part) => (part.type === 'text' ? part.text : ''))
    .join('');
}

/**
 * File parts become attachment chips.
 *
 * `name` and `sizeBytes` survive the round trip through storage, so a reloaded
 * transcript shows the same chip the composer did rather than falling back to a
 * MIME type.
 *
 * Image parts are deliberately skipped. A stored one is either inline base64 —
 * the same bytes the model got, and re-embedding a megabyte of them to draw a
 * chip that says "image" is a trade nobody wants — or a legacy `/api/media/`
 * URL whose token expired long ago, which would render as a broken image.
 * Neither is worth showing.
 */
export function attachmentsOf(
  content: readonly ContentPart[],
): readonly Attachment[] {
  return content.flatMap((part) =>
    part.type === 'file'
      ? [
          {
            mimeType: part.mimeType,
            path: part.path,
            ...(part.name === undefined ? {} : { name: part.name }),
            ...(part.sizeBytes === undefined
              ? {}
              : { sizeBytes: part.sizeBytes }),
          },
        ]
      : [],
  );
}

export function parseArgs(argumentsJson: string): unknown {
  try {
    return JSON.parse(argumentsJson);
  } catch {
    return argumentsJson;
  }
}
