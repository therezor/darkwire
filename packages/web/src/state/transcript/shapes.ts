/**
 * What a transcript is made of.
 *
 * Types only — no behaviour, so both builders can depend on it and neither can
 * reach the other through it. `live.ts` reduces socket frames into these shapes
 * and `stored.ts` rebuilds the same shapes from a REST history; that two
 * constructors exist for one model is the fact `merge`'s reconciliation is
 * built on, and keeping the model itself in one file is what stops the two
 * drifting into two models.
 *
 * The model is:
 *
 *  - A **transcript** is a flat list of items in arrival order: what the user
 *    said, what a turn produced, a mid-turn steer, a notice that belongs to no
 *    turn.
 *  - A **turn** is a list of parts, also in arrival order: text, reasoning, a
 *    tool call, a notice. Parts are ordered because they interleave — a model
 *    that writes a sentence, calls a tool, and writes another sentence must
 *    render in that order, and a shape with `text` and `tools[]` as separate
 *    fields cannot express it.
 *  - A **delegating tool call carries a list of parts of its own** — a
 *    subagent's turn, rendered inside the card that started it. It is the same
 *    `TurnPart[]`, reduced by the same `applyPartEvent`, which is what makes a
 *    nested `exec` card identical to a top-level one and makes a subagent of a
 *    subagent free rather than a second implementation.
 */

import type {
  Attachment,
  NestedAgentEvent,
  NoticeKind,
  StopReason,
  ToolRisk,
  Usage,
} from '@darkwire/protocol';

// Shapes

export interface UserItem {
  readonly kind: 'user';
  /**
   * Stable for the life of the item, and unique across the transcript.
   *
   * The client's id for a message this tab sent, the stored row id for one it
   * fetched — and deliberately *not* the turn id, which the ack also carries.
   * Every item in the transcript is a React key, and a user bubble that renamed
   * itself to the turn id would collide with the turn that turn id belongs to.
   */
  readonly id: string;
  readonly clientMessageId: string | undefined;
  /**
   * The turn this message started, once it is known.
   *
   * This is how an optimistic bubble is recognised as the *same message* the
   * REST history later returns under a different row id — `message.ack` carries
   * the turn id, and every stored message the turn produced carries it too. It
   * is the only key the two sources share, and without it a reload after
   * sending shows the message twice.
   */
  readonly turnId: string | undefined;
  /**
   * Storage's address for this message, once it has one.
   *
   * What Edit, Regenerate and Branch name. `undefined` on an optimistic bubble
   * that storage has not answered for yet, which is exactly when those actions
   * must stay disabled — and `turn.end` fills it in the moment the turn
   * finishes, so a message becomes editable without a refetch.
   */
  readonly seq: number | undefined;
  readonly text: string;
  readonly attachments: readonly Attachment[];
  /** True until the ack lands — the bubble that has not been accepted yet. */
  readonly pending: boolean;
}

export interface TextPart {
  readonly kind: 'text';
  readonly id: string;
  readonly text: string;
}

export interface ReasoningPart {
  readonly kind: 'reasoning';
  readonly id: string;
  readonly text: string;
}

export interface NoticePart {
  readonly kind: 'notice';
  readonly id: string;
  readonly notice: NoticeKind;
  readonly message: string;
}

/**
 * `awaiting-approval` is a status rather than a flag on `running`, because it is
 * the one state where nothing is happening and the user is the reason.
 */
export type ToolStatus = 'running' | 'awaiting-approval' | 'ok' | 'error';

export interface ToolApprovalState {
  readonly expiresAtMs: number;
  /**
   * Set once *this tab* answered. The card stops offering buttons immediately
   * rather than waiting for the round trip, and a second tab that answered
   * first simply sees the result arrive.
   */
  readonly answered: 'approved' | 'denied' | undefined;
}

export interface ToolPart {
  readonly kind: 'tool';
  /** The `callId`. Unique per turn, and the key every later event pairs on. */
  readonly id: string;
  readonly name: string;
  readonly args: unknown;
  readonly risk: ToolRisk;
  readonly status: ToolStatus;
  /** Last figure from `tool.progress`; the card ticks past it locally. */
  readonly elapsedMs: number;
  readonly durationMs: number | undefined;
  /** The tool's own output, unwrapped. Undefined until the result arrives. */
  readonly content: string | undefined;
  readonly truncated: boolean;
  readonly approval: ToolApprovalState | undefined;
  /** Notices scoped to this call, rendered in the card. */
  readonly notices: readonly NoticePart[];
  /**
   * The subagent's run, when this call delegated to one.
   *
   * `undefined` on every ordinary tool call, which is what keeps a card that
   * has nothing nested inside it from rendering an empty container.
   */
  readonly subagent: SubagentPart | undefined;
}

/**
 * A subagent's turn, inside the card of the call that started it.
 *
 * Its own `parts`, of the same type as a turn's, because it *is* a turn: the
 * events arrive from a real `AgentLoop` and carry no less than the caller's.
 * Reduced by the same helper, so nothing about rendering a nested tool call
 * differs from rendering a top-level one.
 */
export interface SubagentPart {
  readonly agentId: string;
  /** Never empty in practice — the wrapper falls back to the id. */
  readonly label: string;
  /**
   * The subagent's own session.
   *
   * Two jobs. It disambiguates a nested `callId` from its caller's, which are
   * both the model's and only unique within one assistant message. And it is
   * the address a transcript rebuilt from storage fetches, when the parent's
   * history holds only the delegating tool result.
   */
  readonly sessionKey: string;
  readonly parts: readonly TurnPart[];
  readonly model: string;
  readonly stopReason: StopReason | undefined;
  readonly usage: Usage | undefined;
  readonly iterations: number;
  readonly elapsedMs: number | undefined;
  readonly done: boolean;
  /**
   * Whether `parts` is the whole run.
   *
   * False on a transcript rebuilt from storage, where the run is known to have
   * happened — the parent's metadata names its session — but nothing has
   * fetched it. The card offers to, rather than showing an empty box as though
   * the subagent had done nothing.
   */
  readonly loaded: boolean;
  /**
   * True when this client joined the run after it had already started.
   *
   * The case is a reload during a delegation. The replay ring holds 512 frames
   * and every nested delta is one of them, so a subagent of any length falls
   * outside it; the tab comes back, the stored tail has no row for the
   * iteration still running, and the frames that arrive from then on are the
   * *middle* of a run whose beginning nothing has.
   *
   * Rendering that tail is right — it is what is happening, and the alternative
   * is a blank card — but it must not be mistaken for the run. `withLiveRiskBands`
   * carries a live card across the refetch at `turn.end` precisely so a finished
   * run does not vanish, and without this flag the half a client happened to
   * catch would win over the whole one storage can supply, permanently.
   */
  readonly partial: boolean;
}

export type TurnPart = TextPart | ReasoningPart | NoticePart | ToolPart;

/**
 * Why a turn failed.
 *
 * Deliberately carries no error code. Nothing renders one — the failure line is
 * the message and whether retrying is worth it — and leaving it out is what
 * makes a failure that arrived over the socket and one rebuilt from storage the
 * same value. Storage records the sentence, not the code.
 */
export interface TurnFailure {
  readonly message: string;
  readonly retryable: boolean;
}

export interface TurnItem {
  readonly kind: 'turn';
  /** The `turnId`. */
  readonly id: string;
  /**
   * The session this turn ran on, from `turn.start`.
   *
   * Here so a top-level tool call can be told apart from a subagent's call of
   * the same id — see `SubagentPart.sessionKey`. Empty on a turn rebuilt from
   * storage or reconstructed from a mid-turn resume, neither of which can
   * contain a nested call anyway.
   */
  readonly sessionKey: string;
  readonly model: string;
  readonly provider: string;
  readonly parts: readonly TurnPart[];
  readonly stopReason: StopReason | undefined;
  readonly usage: Usage | undefined;
  readonly iterations: number;
  /** Wall time for the whole turn, load and tools and approvals included. */
  readonly elapsedMs: number | undefined;
  /**
   * Time the model spent emitting tokens, and how long it waited to start.
   *
   * `generationMs` is the divisor behind the tokens/s figure — `elapsedMs` was,
   * and reported a cold local model at a fraction of its real speed because the
   * weight load sat in it. Both undefined on a turn nothing could measure.
   */
  readonly generationMs: number | undefined;
  /** The tokens produced inside `generationMs`, and only those. */
  readonly generationTokens: number | undefined;
  readonly firstTokenMs: number | undefined;
  /**
   * The seqs this turn spans: the user message that started it, and the last
   * message it wrote. `firstSeq` is what Regenerate re-runs from and what
   * Branch forks at.
   */
  readonly firstSeq: number | undefined;
  readonly lastSeq: number | undefined;
  /** False while the turn is streaming — what drives the caret and the spinner. */
  readonly done: boolean;
  readonly failure: TurnFailure | undefined;
  /**
   * True when the server has replayed this turn in full and it therefore
   * outranks the stored copy of itself.
   *
   * Storage and the socket describe the same turn from different ends. Storage
   * has every iteration that finished and nothing of the one that is running;
   * the socket has whatever this tab has watched. Ordinarily storage is the
   * safer base, which is why `mergeStoredHistory` builds on it — a live turn is
   * a *tail* and losing it costs less than losing the history under it.
   *
   * `session.replay { resumingTurnId }` inverts that for one turn, and only for
   * it: the frames that follow are the whole turn from its `turn.start`, so the
   * item they build is a superset of the stored one — it has the iteration
   * storage cannot hold, and the nested subagent runs storage never holds. Set
   * on the seat the replay puts down for those frames, read by
   * `mergeStoredHistory`, and false everywhere else, so every path the server
   * cannot make that promise on keeps the behaviour it had.
   */
  readonly authoritative: boolean;
}

export interface SteerItem {
  readonly kind: 'steer';
  readonly id: string;
  readonly text: string;
}

export interface NoticeItem {
  readonly kind: 'notice';
  readonly id: string;
  readonly notice: NoticeKind;
  readonly message: string;
}

export type TranscriptItem = UserItem | TurnItem | SteerItem | NoticeItem;

export type Transcript = readonly TranscriptItem[];

export const EMPTY_TRANSCRIPT: Transcript = [];

/**
 * The frames that land on a list of parts rather than on a turn or a session.
 *
 * `notice` is not among them even though it produces a part: it can also carry
 * no turn at all, which makes it a transcript-level decision that `applyNotice`
 * takes before any list of parts is chosen.
 */
export type PartEvent = Extract<
  NestedAgentEvent,
  {
    type:
      | 'assistant.delta'
      | 'reasoning.delta'
      | 'tool.call'
      | 'tool.progress'
      | 'tool.approvalRequest'
      | 'tool.result';
  }
>;
