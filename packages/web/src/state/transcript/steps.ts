/**
 * A turn's messages, read back off its parts.
 *
 * Derived rather than kept beside the parts, so a live turn and one rebuilt from
 * storage are split by the same code and cannot disagree about the same run.
 */

import type { StepStats } from '@darkwire/protocol';

import type { SteerPart, StepPart, TurnPart } from './shapes.js';

export function stepPart(
  stats: StepStats,
  firstSeq: number,
  lastSeq: number,
): StepPart {
  return {
    kind: 'step',
    id: `step:${String(firstSeq)}`,
    stats,
    firstSeq,
    lastSeq,
  };
}

/** One piece of a turn as it renders. */
export type TurnPiece =
  | {
      /** A model call: what it wrote and the tool cards it asked for. */
      readonly kind: 'message';
      /** The first part's id, so a message keeps its key when it ends. */
      readonly key: string;
      /** From 1, among the turn's messages. */
      readonly number: number;
      readonly parts: readonly TurnPart[];
      /** Its end, once the loop wrote it: the seqs and the figures. */
      readonly end: StepPart | undefined;
      /** The answer text it wrote, for Copy. */
      readonly text: string;
    }
  | {
      /** What the user said to the running turn, where the loop read it. */
      readonly kind: 'steer';
      readonly part: SteerPart;
    };

/**
 * A turn's parts as the messages it is made of, with each steer between the
 * two it came between.
 *
 * A message ends at its step part, which is where `step.end` put it. One with
 * no step part yet is the one still being written, or a row stored before rows
 * carried their figures.
 */
export function messagesOf(parts: readonly TurnPart[]): readonly TurnPiece[] {
  const out: TurnPiece[] = [];
  let current: TurnPart[] = [];
  let number = 0;
  const close = (end: StepPart | undefined): void => {
    const first = current[0] ?? end;
    if (first === undefined) return;
    number += 1;
    out.push({
      kind: 'message',
      key: first.id,
      number,
      parts: current,
      end,
      text: current
        .filter((part) => part.kind === 'text')
        .map((part) => part.text)
        .join('\n\n'),
    });
    current = [];
  };
  for (const part of parts) {
    if (part.kind === 'step') {
      close(part);
    } else if (part.kind === 'steer') {
      close(undefined);
      out.push({ kind: 'steer', part });
    } else {
      current.push(part);
    }
  }
  close(undefined);
  return out;
}
