/**
 * The transcript: what the socket's events mean, as a shape React can render.
 *
 * Split into four files, and the split follows the one fact that shapes this
 * area: **there are two constructors for one model.** `live.ts` reduces socket
 * frames; `stored.ts` rebuilds the same shapes from a REST or replay history.
 * They share `shapes.ts` so there is only ever one model, and `parts.ts` so a
 * tool card does not look slightly different depending on whether you watched
 * it happen or reloaded the page. `mergeStoredHistory` exists precisely because
 * the two sources share no id, and it lives beside the builder it reconciles.
 *
 * This file is the surface those four present, unchanged — every consumer still
 * imports `@/state/transcript.js`, and so does the test file.
 */

export {
  EMPTY_TRANSCRIPT,
  type NoticeItem,
  type NoticePart,
  type ReasoningPart,
  type SteerItem,
  type SubagentPart,
  type TextPart,
  type ToolApprovalState,
  type ToolPart,
  type ToolStatus,
  type Transcript,
  type TranscriptItem,
  type TurnFailure,
  type TurnItem,
  type TurnPart,
  type UserItem,
} from './transcript/shapes.js';

export { unwrapToolOutput } from './transcript/parts.js';

export {
  appendPendingUserMessage,
  applyServerMessage,
  markApprovalAnswered,
  truncateTranscriptAfter,
} from './transcript/live.js';

export { fromStoredMessages, mergeStoredHistory } from './transcript/stored.js';
