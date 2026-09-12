import { describe, expect, it } from 'vitest';

import {
  ClientMessageSchema,
  MAX_ATTACHMENTS,
  PROTOCOL_VERSION,
  ServerMessageSchema,
  UNSEQUENCED_SERVER_EVENTS,
  UserMessageRequestSchema,
  isSequencedServerMessage,
} from '#src/ws.js';
import { ChatMessageSchema } from '#src/messages.js';

/** Reads the `type` literal off a discriminated-union variant. */
function variantTypes(
  union: typeof ClientMessageSchema | typeof ServerMessageSchema,
): string[] {
  return union.options.map((option) => option.shape.type.value);
}

describe('protocol version', () => {
  it('pins the wire protocol version', () => {
    expect(PROTOCOL_VERSION).toBe(2);
  });
});

describe('ClientMessageSchema', () => {
  it('parses a user message and defaults its attachment list', () => {
    const parsed = ClientMessageSchema.parse({
      type: 'user.message',
      sessionKey: 'web:abc',
      content: 'hello',
    });
    expect(parsed).toMatchObject({ type: 'user.message', attachments: [] });
  });

  it('rejects an unknown message type', () => {
    expect(ClientMessageSchema.safeParse({ type: 'nope' }).success).toBe(false);
  });

  it('rejects a known type with the wrong payload', () => {
    expect(ClientMessageSchema.safeParse({ type: 'turn.stop' }).success).toBe(
      false,
    );
    expect(
      ClientMessageSchema.safeParse({ type: 'turn.stop', sessionKey: '' })
        .success,
    ).toBe(false);
  });

  it('defaults an approval to the narrowest scope', () => {
    // Anything broader has to be chosen deliberately — a client that omits the
    // field must not silently grant blanket exec approval.
    const parsed = ClientMessageSchema.parse({
      type: 'tool.approve',
      callId: 'c1',
      approved: true,
    });
    expect(parsed).toMatchObject({ scope: 'once' });
  });

  it('requires a cursor on resume so replay has a lower bound', () => {
    expect(
      ClientMessageSchema.safeParse({ type: 'session.resume', sessionKey: 's' })
        .success,
    ).toBe(false);
    expect(
      ClientMessageSchema.safeParse({
        type: 'session.resume',
        sessionKey: 's',
        lastSeq: 0,
      }).success,
    ).toBe(true);
  });

  it('lets session.new omit a key for the server to generate', () => {
    expect(ClientMessageSchema.safeParse({ type: 'session.new' }).success).toBe(
      true,
    );
  });

  it('covers every declared client message type exactly once', () => {
    const types = variantTypes(ClientMessageSchema);
    expect(new Set(types).size).toBe(types.length);
    expect(types).toContain('turn.steer');
  });
});

describe('ServerMessageSchema', () => {
  it('parses a streaming delta', () => {
    const parsed = ServerMessageSchema.parse({
      type: 'assistant.delta',
      seq: 7,
      turnId: 't1',
      text: 'chunk',
    });
    expect(parsed).toMatchObject({ type: 'assistant.delta', seq: 7 });
  });

  it('pins the version literal on the connected event', () => {
    const base = {
      type: 'connected',
      sessionKey: 's',
      serverTimeMs: 0,
      lastSeq: 0,
    };
    expect(
      ServerMessageSchema.safeParse({ ...base, protocolVersion: 2 }).success,
    ).toBe(true);
    expect(
      ServerMessageSchema.safeParse({ ...base, protocolVersion: 1 }).success,
    ).toBe(false);
  });

  it('requires seq on every session-scoped event', () => {
    // The reconnect contract: anything a resuming client must be able to replay
    // has to be addressable by a cursor, so a new event without `seq` is a bug
    // that would silently drop from replay.
    const missing = ServerMessageSchema.options
      .filter(
        (option) =>
          !(UNSEQUENCED_SERVER_EVENTS as readonly string[]).includes(
            option.shape.type.value,
          ),
      )
      .filter((option) => !('seq' in option.shape))
      .map((option) => option.shape.type.value);

    expect(missing).toEqual([]);
  });

  it('omits seq on connection-level events', () => {
    const withSeq = ServerMessageSchema.options
      .filter((option) =>
        (UNSEQUENCED_SERVER_EVENTS as readonly string[]).includes(
          option.shape.type.value,
        ),
      )
      .filter((option) => 'seq' in option.shape)
      .map((option) => option.shape.type.value);

    expect(withSeq).toEqual([]);
  });

  it('rejects a negative sequence number', () => {
    expect(
      ServerMessageSchema.safeParse({
        type: 'assistant.delta',
        seq: -1,
        turnId: 't',
        text: '',
      }).success,
    ).toBe(false);
  });

  it('accepts an error event without a turn, for connection-level failures', () => {
    const parsed = ServerMessageSchema.parse({
      type: 'error',
      code: 'unauthorized',
      message: 'no',
    });
    expect(parsed).toMatchObject({ retryable: false });
  });

  it('rejects an untyped error code', () => {
    expect(
      ServerMessageSchema.safeParse({
        type: 'error',
        code: 'kaboom',
        message: 'x',
      }).success,
    ).toBe(false);
  });

  it('carries the injection notice without touching the tool result', () => {
    // Detection is non-destructive: the notice is a separate event, so the
    // content the model sees is unchanged.
    const parsed = ServerMessageSchema.parse({
      type: 'notice',
      seq: 3,
      kind: 'prompt_injection',
      message: 'suspicious content in tool output',
      callId: 'c1',
    });
    expect(parsed).toMatchObject({ kind: 'prompt_injection', callId: 'c1' });
  });

  it('covers every declared server message type exactly once', () => {
    const types = variantTypes(ServerMessageSchema);
    expect(new Set(types).size).toBe(types.length);
    expect(types).toContain('tool.approvalRequest');
    expect(types).toContain('session.replay');
  });
});

describe('isSequencedServerMessage', () => {
  it('accepts a sequenced event', () => {
    const message = ServerMessageSchema.parse({
      type: 'assistant.delta',
      seq: 1,
      turnId: 't',
      text: 'x',
    });
    expect(isSequencedServerMessage(message)).toBe(true);
  });

  it.each([...UNSEQUENCED_SERVER_EVENTS])('rejects %s', (type) => {
    const samples: Record<string, unknown> = {
      connected: {
        type: 'connected',
        protocolVersion: PROTOCOL_VERSION,
        sessionKey: 's',
        serverTimeMs: 0,
        lastSeq: 0,
      },
      pong: { type: 'pong', serverTimeMs: 0 },
      error: { type: 'error', code: 'internal', message: 'x' },
    };
    const message = ServerMessageSchema.parse(samples[type]);
    expect(isSequencedServerMessage(message)).toBe(false);
  });
});

describe('session replay', () => {
  it('reports incomplete when history fell out of the ring buffer', () => {
    const parsed = ServerMessageSchema.parse({
      type: 'session.replay',
      seq: 9,
      sessionKey: 's',
      messages: [],
      complete: false,
    });
    expect(parsed).toMatchObject({ complete: false });
  });

  it('defaults to complete', () => {
    const parsed = ServerMessageSchema.parse({
      type: 'session.replay',
      seq: 9,
      sessionKey: 's',
      messages: [],
    });
    expect(parsed).toMatchObject({ complete: true });
    // Absent, rather than defaulted: no key at all is what every replay before
    // the in-flight turn log carried, and it means the tail is history that
    // nothing about to arrive overlaps.
    expect(parsed).not.toHaveProperty('resumingTurnId');
  });

  it('names the turn whose frames follow and redefine it', () => {
    const parsed = ServerMessageSchema.parse({
      type: 'session.replay',
      seq: 9,
      sessionKey: 's',
      messages: [],
      complete: false,
      resumingTurnId: 't1',
    });
    expect(parsed).toMatchObject({ resumingTurnId: 't1' });
  });

  it('carries stored messages with their tool-call pairing intact', () => {
    const assistant = ChatMessageSchema.parse({
      role: 'assistant',
      content: [{ type: 'text', text: 'calling' }],
      toolCalls: [
        { id: 'call_1', name: 'read_file', argumentsJson: '{"path":"a.txt"}' },
      ],
    });
    const tool = ChatMessageSchema.parse({
      role: 'tool',
      toolCallId: 'call_1',
      name: 'read_file',
      content: 'contents',
    });

    const parsed = ServerMessageSchema.parse({
      type: 'session.replay',
      seq: 1,
      sessionKey: 's',
      messages: [
        {
          id: 'm1',
          sessionKey: 's',
          seq: 1,
          createdAtMs: 1,
          message: assistant,
        },
        { id: 'm2', sessionKey: 's', seq: 2, createdAtMs: 2, message: tool },
      ],
    });

    expect(parsed.type).toBe('session.replay');
    if (parsed.type !== 'session.replay') throw new Error('unreachable');
    const [first, second] = parsed.messages;
    expect(
      first!.message.role === 'assistant' && first!.message.toolCalls[0]?.id,
    ).toBe('call_1');
    expect(second!.message.role === 'tool' && second!.message.toolCallId).toBe(
      'call_1',
    );
  });
});

describe('regenerate and edit', () => {
  it('accepts a regenerate that names no message', () => {
    const parsed = ClientMessageSchema.parse({
      type: 'turn.regenerate',
      sessionKey: 's',
    });
    expect(parsed.type).toBe('turn.regenerate');
    if (parsed.type !== 'turn.regenerate') throw new Error('unreachable');
    expect(parsed.seq).toBeUndefined();
  });

  it('accepts a regenerate that names one', () => {
    const parsed = ClientMessageSchema.parse({
      type: 'turn.regenerate',
      sessionKey: 's',
      seq: 7,
    });
    if (parsed.type !== 'turn.regenerate') throw new Error('unreachable');
    expect(parsed.seq).toBe(7);
  });

  it('rejects a seq that cannot address a message', () => {
    expect(() =>
      ClientMessageSchema.parse({
        type: 'turn.regenerate',
        sessionKey: 's',
        seq: 0,
      }),
    ).toThrow();
  });

  it('defaults an edit to no attachments', () => {
    const parsed = ClientMessageSchema.parse({
      type: 'user.edit',
      sessionKey: 's',
      seq: 3,
      content: 'rewritten',
    });
    if (parsed.type !== 'user.edit') throw new Error('unreachable');
    expect(parsed.attachments).toEqual([]);
    expect(parsed.content).toBe('rewritten');
  });

  it('requires an edit to name the message it replaces', () => {
    expect(() =>
      ClientMessageSchema.parse({
        type: 'user.edit',
        sessionKey: 's',
        content: 'x',
      }),
    ).toThrow();
  });
});

describe('session.truncated', () => {
  const frame = {
    type: 'session.truncated' as const,
    seq: 4,
    sessionKey: 's',
    upToSeq: 2,
    messages: [
      {
        id: 'm1',
        sessionKey: 's',
        seq: 1,
        createdAtMs: 1,
        message: {
          role: 'user' as const,
          content: [{ type: 'text' as const, text: 'hi' }],
        },
      },
    ],
  };

  it('carries the surviving tail', () => {
    const parsed = ServerMessageSchema.parse(frame);
    if (parsed.type !== 'session.truncated') throw new Error('unreachable');
    expect(parsed.upToSeq).toBe(2);
    expect(parsed.messages).toHaveLength(1);
  });

  it('accepts a cut to zero', () => {
    const parsed = ServerMessageSchema.parse({
      ...frame,
      upToSeq: 0,
      messages: [],
    });
    if (parsed.type !== 'session.truncated') throw new Error('unreachable');
    expect(parsed.upToSeq).toBe(0);
  });

  it('is sequenced, so it replays to a reconnecting tab', () => {
    expect(isSequencedServerMessage(ServerMessageSchema.parse(frame))).toBe(
      true,
    );
  });
});

describe('turn.end reporting', () => {
  it('carries timing and the seqs the turn spanned', () => {
    const parsed = ServerMessageSchema.parse({
      type: 'turn.end',
      seq: 9,
      turnId: 't1',
      stopReason: 'complete',
      usage: { promptTokens: 10, completionTokens: 5, totalTokens: 15 },
      iterations: 2,
      elapsedMs: 1200,
      firstSeq: 3,
      lastSeq: 6,
    });
    if (parsed.type !== 'turn.end') throw new Error('unreachable');
    expect(parsed.elapsedMs).toBe(1200);
    expect(parsed.firstSeq).toBe(3);
    expect(parsed.lastSeq).toBe(6);
  });

  it('keeps all three optional, so an older server still parses', () => {
    const parsed = ServerMessageSchema.parse({
      type: 'turn.end',
      seq: 9,
      turnId: 't1',
      stopReason: 'complete',
    });
    if (parsed.type !== 'turn.end') throw new Error('unreachable');
    expect(parsed.elapsedMs).toBeUndefined();
    expect(parsed.firstSeq).toBeUndefined();
  });
});

describe('AttachmentSchema', () => {
  const attachment = {
    mimeType: 'image/png',
    path: 'uploads/ab12cd34-shot.png',
  };

  it('takes a workspace path and a mime type', () => {
    expect(
      UserMessageRequestSchema.parse({
        type: 'user.message',
        sessionKey: 's',
        content: 'look',
        attachments: [{ ...attachment, name: 'shot.png', sizeBytes: 2048 }],
      }).attachments,
    ).toHaveLength(1);
  });

  it('refuses more attachments than one message can carry', () => {
    // Not a byte limit -- a count. Every attachment is read and inlined on
    // every iteration of the turn, so a frame naming one small image a few
    // thousand times costs nothing to send and expands to thousands of base64
    // blocks in a single provider request.
    const many = Array.from({ length: MAX_ATTACHMENTS + 1 }, () => attachment);
    expect(
      UserMessageRequestSchema.safeParse({
        type: 'user.message',
        sessionKey: 's',
        content: 'look',
        attachments: many,
      }).success,
    ).toBe(false);
  });

  it('defaults to none', () => {
    expect(
      UserMessageRequestSchema.parse({
        type: 'user.message',
        sessionKey: 's',
        content: 'hi',
      }).attachments,
    ).toEqual([]);
  });
});

describe('context.usage', () => {
  const frame = {
    type: 'context.usage',
    seq: 4,
    sessionKey: 'web:1',
    estimatedTokens: 2631,
    contextWindowTokens: 65_536,
    breakdown: {
      systemPrompt: 1840,
      tools: 560,
      messages: 201,
      runtimeBlock: 30,
    },
  };

  it('carries the same breakdown shape the context route returns', () => {
    const parsed = ServerMessageSchema.parse(frame);
    if (parsed.type !== 'context.usage') throw new Error('unreachable');
    expect(parsed.estimatedTokens).toBe(2631);
    expect(parsed.breakdown).toEqual(frame.breakdown);
  });

  it('defaults the breakdown, so a total with no sections still parses', () => {
    const parsed = ServerMessageSchema.parse({
      type: 'context.usage',
      seq: 4,
      sessionKey: 'web:1',
      estimatedTokens: 10,
      contextWindowTokens: 100,
    });
    if (parsed.type !== 'context.usage') throw new Error('unreachable');
    expect(parsed.breakdown).toEqual({});
  });

  it('is sequenced, so a reconnecting tab is told the current figure', () => {
    // It enters the ring like every other session-scoped frame, and replaying
    // one is idempotent: it sets a number rather than appending to anything.
    expect(isSequencedServerMessage(ServerMessageSchema.parse(frame))).toBe(
      true,
    );
  });

  it('names the session and not the turn', () => {
    // Deliberate. It describes the conversation, not the turn that grew it —
    // which is what keeps it out of the client's set of turn-growing events,
    // where a replayed frame onto a finished turn would have to be dropped.
    expect(Object.keys(frame)).not.toContain('turnId');
  });
});
