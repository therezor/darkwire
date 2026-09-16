/**
 * The canonical message shapes.
 *
 * `darkwire-core` owns the *behaviour* over these (`SessionStore`,
 * `findLegalStart`, `historyForLLM`); the schemas live here because they cross
 * the wire in both the WS protocol and the REST session endpoints, and this
 * package is the single source of truth for anything shaped by more than one
 * consumer.
 *
 * Two properties matter downstream:
 *
 *  - `assistant.toolCalls[].id` and `tool.toolCallId` are the pairing keys
 *    `findLegalStart` walks to guarantee no `tool` message reaches a provider
 *    without its originating `assistant` turn. Orphaned tool results are a
 *    provider 400.
 *  - Tool-call arguments stay a verbatim JSON *string*. Models emit malformed
 *    JSON often enough that parsing must be the tool registry's job, where a
 *    failure becomes a typed tool error the model can retry against, rather
 *    than a parse exception in the transport layer.
 */

import { z } from 'zod';

export const ChatRoleSchema = z.enum(['system', 'user', 'assistant', 'tool']);

export const TextPartSchema = z.object({
  type: z.literal('text'),
  text: z.string(),
});
export type TextPart = z.infer<typeof TextPartSchema>;

/**
 * An image as a *provider* takes it: inline base64 (`data`) or a URL it can
 * fetch itself (`url`).
 *
 * `url` must be absolute and reachable from wherever the model runs. A
 * workspace file is not that — it is a `FilePart`, and it becomes one of these
 * only at request time, when `materialiseAttachments` reads the bytes. Putting
 * a relative signed URL here is what silently sent every attachment nowhere.
 */
export const ImagePartSchema = z.object({
  type: z.literal('image'),
  mimeType: z.string().min(1),
  data: z.string().optional(),
  url: z.string().optional(),
});
export type ImagePart = z.infer<typeof ImagePartSchema>;

/**
 * A workspace file somebody attached to a message.
 *
 * A reference, never bytes. The path outlives any signed URL, so history
 * replayed a month later still resolves to the same file — and the same part
 * describes a screenshot, a CSV and a 200 MB archive, so nothing upstream has
 * to branch on the MIME type to decide what an attachment *is*. Turning it
 * into something a provider accepts happens once, at request time, in
 * `materialiseAttachments`.
 */
export const FilePartSchema = z.object({
  type: z.literal('file'),
  mimeType: z.string().min(1),
  /** Workspace-relative, as returned by the upload endpoint. */
  path: z.string().min(1),
  /** What the user called it. The path is mangled for safety; this is not. */
  name: z.string().optional(),
  sizeBytes: z.number().int().nonnegative().optional(),
});
export type FilePart = z.infer<typeof FilePartSchema>;

export const ContentPartSchema = z.discriminatedUnion('type', [
  TextPartSchema,
  ImagePartSchema,
  FilePartSchema,
]);
export type ContentPart = z.infer<typeof ContentPartSchema>;

/**
 * Flattened relative to OpenAI's `{id, type:'function', function:{name,
 * arguments}}`. The nesting carries no information — `type` has only ever had
 * one value — and flattening removes a layer of optional chaining from every
 * wire adapter and from `findLegalStart`.
 */
export const ToolCallSchema = z.object({
  id: z.string().min(1),
  name: z.string().min(1),
  argumentsJson: z.string(),
});
export type ToolCall = z.infer<typeof ToolCallSchema>;

export const SystemMessageSchema = z.object({
  role: z.literal('system'),
  content: z.string(),
});

export const UserMessageSchema = z.object({
  role: z.literal('user'),
  content: z.array(ContentPartSchema),
});

export const AssistantMessageSchema = z.object({
  role: z.literal('assistant'),
  content: z.array(ContentPartSchema),
  toolCalls: z.array(ToolCallSchema).default([]),
  /**
   * Reasoning/thinking text, kept beside the answer rather than inside
   * `content` so it can be surfaced in a collapsible UI block and excluded
   * from history replay without re-parsing the content parts.
   */
  reasoning: z.string().optional(),
});

export const ToolMessageSchema = z.object({
  role: z.literal('tool'),
  toolCallId: z.string().min(1),
  name: z.string().min(1),
  content: z.string(),
  /**
   * A failed tool call is still a legal history entry — the model needs to see
   * the error to recover. An explicit flag, because the alternative of
   * inspecting the content for an `Error` prefix misfires on any tool whose
   * legitimate output begins with that word.
   */
  isError: z.boolean().default(false),
  /** Set when the result was head+tail truncated to fit the tool-output cap. */
  truncated: z.boolean().default(false),
});

export const ChatMessageSchema = z.discriminatedUnion('role', [
  SystemMessageSchema,
  UserMessageSchema,
  AssistantMessageSchema,
  ToolMessageSchema,
]);
export type ChatMessage = z.infer<typeof ChatMessageSchema>;

export type SystemMessage = z.infer<typeof SystemMessageSchema>;
export type UserMessage = z.infer<typeof UserMessageSchema>;
export type AssistantMessage = z.infer<typeof AssistantMessageSchema>;
export type ToolMessage = z.infer<typeof ToolMessageSchema>;

/**
 * A persisted message. An envelope rather than a flattened intersection: the
 * message is a discriminated union, and `allOf`-ing storage fields onto it
 * would cost the discriminator on both the TS and JSON Schema sides.
 */
export const StoredMessageSchema = z.object({
  id: z.string().min(1),
  sessionKey: z.string().min(1),
  /**
   * Storage's per-session ordering, and the only stable way to *address* a
   * message.
   *
   * On the wire rather than internal because a client needs it: editing,
   * regenerating and branching all name a point in the conversation, and an id
   * cannot express "and everything after this". It is already the REST
   * pagination cursor, so publishing it reveals nothing new.
   */
  seq: z.number().int().positive(),
  createdAtMs: z.number().int().nonnegative(),
  /** Groups every message produced by one user turn, including tool traffic. */
  turnId: z.string().optional(),
  message: ChatMessageSchema,
});
export type StoredMessage = z.infer<typeof StoredMessageSchema>;

/** Token accounting as reported by the provider. */
export const UsageSchema = z.object({
  promptTokens: z.number().int().nonnegative().default(0),
  completionTokens: z.number().int().nonnegative().default(0),
  totalTokens: z.number().int().nonnegative().default(0),
  /** Prompt-cache hits, where the provider reports them. */
  cachedTokens: z.number().int().nonnegative().optional(),
  reasoningTokens: z.number().int().nonnegative().optional(),
});
export type Usage = z.infer<typeof UsageSchema>;

/**
 * Completion tokens per second, or `undefined` when the figure would be a lie.
 *
 * Here, in the one package the browser can import, rather than beside the
 * stored record it describes: that lives in `darkwire-core`, which the web
 * bundle cannot reach at all, and a second copy in it is a second copy that can
 * disagree with the server's.
 *
 * Zero elapsed or zero completion tokens returns nothing rather than zero or
 * infinity. A turn that produced no tokens has no rate, and a turn measured at
 * zero milliseconds was not measured.
 */
export function tokensPerSecond(
  usage: Usage,
  elapsedMs: number,
): number | undefined {
  if (elapsedMs <= 0 || usage.completionTokens <= 0) return undefined;
  return (usage.completionTokens * 1000) / elapsedMs;
}

/**
 * The timings a rate can be derived from.
 *
 * Spelled `?: number | undefined` rather than a bare `?` because every caller
 * holds variables that are already `number | undefined`, and under
 * `exactOptionalPropertyTypes` a bare `?` would make each of them write a
 * conditional spread to say nothing.
 */
export interface TurnTiming {
  /** Time the model actually spent emitting tokens, where that was measured. */
  readonly generationMs?: number | undefined;
  /**
   * The completion tokens produced inside `generationMs`, and only those.
   *
   * Not the turn's `completionTokens`, and the difference is the point: a
   * request whose whole reply arrived in one frame — which is how Ollama sends
   * a bare tool call, at any length — is charged for its tokens and measured at
   * zero. Pairing them keeps a request out of both sides at once.
   */
  readonly generationTokens?: number | undefined;
  /** Wall time across the whole turn, including everything that is not decoding. */
  readonly elapsedMs?: number | undefined;
}

/**
 * The rate to show a person, divided by generation time where there is any.
 *
 * `tokensPerSecond` divides by whatever it is handed; this decides what to hand
 * it, and the whole-turn wall clock is the wrong answer. A local model loading
 * its weights for forty seconds, a tool that runs for ten and an approval
 * nobody answered for a minute all land in that clock, and the figure that
 * comes out reports a model as slow when the model was not running.
 *
 * `generationMs` and `generationTokens` are one measurement taken twice — the
 * time a provider spent streaming, and the tokens it reported for exactly the
 * requests that were timed. Both are used or neither is, because a reply that
 * arrived in a single frame has tokens and a window of zero, and pairing its
 * tokens with another request's window is how a rate gets overstated. Measured
 * against a real Ollama, a turn with two tool calls read 50.8 tok/s for a model
 * that decodes at 19.
 *
 * The fallback is not a nicety. Every turn recorded before this was measured
 * has only the wall clock, and blanking those rows would be a regression
 * dressed as accuracy — so would blanking a turn whose every reply came in one
 * frame. Both take the same branch, which is why this tests for zero and not
 * merely for absence.
 */
export function turnRate(usage: Usage, timing: TurnTiming): number | undefined {
  const { generationMs, generationTokens } = timing;
  if (
    generationMs !== undefined &&
    generationMs > 0 &&
    generationTokens !== undefined &&
    generationTokens > 0
  ) {
    return (generationTokens * 1000) / generationMs;
  }
  return timing.elapsedMs === undefined
    ? undefined
    : tokensPerSecond(usage, timing.elapsedMs);
}

/** Why a turn stopped. Every value is terminal. */
export const StopReasonSchema = z.enum([
  'complete',
  'aborted',
  'max_iterations',
  'wall_timeout',
  'error',
]);
export type StopReason = z.infer<typeof StopReasonSchema>;
