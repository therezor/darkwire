import { describe, expect, it } from 'vitest';

import {
  ChatMessageSchema,
  StoredMessageSchema,
  UsageSchema,
  tokensPerSecond,
  turnRate,
} from '#src/messages.js';

describe('ChatMessageSchema', () => {
  it('parses a system message', () => {
    expect(
      ChatMessageSchema.parse({ role: 'system', content: 'be brief' }),
    ).toMatchObject({
      role: 'system',
    });
  });

  it('parses a user message with mixed content parts', () => {
    const parsed = ChatMessageSchema.parse({
      role: 'user',
      content: [
        { type: 'text', text: 'what is this?' },
        { type: 'image', mimeType: 'image/png', url: '/api/files/signed?x=1' },
      ],
    });
    expect(parsed.role).toBe('user');
    if (parsed.role !== 'user') throw new Error('unreachable');
    expect(parsed.content).toHaveLength(2);
  });

  it('parses a file part, keeping the name apart from the path', () => {
    // The path is mangled for safety and the name is not, so a chip can show
    // "Q3 report.csv" while the model is told `uploads/ab12cd34-Q3-report.csv`.
    const parsed = ChatMessageSchema.parse({
      role: 'user',
      content: [
        { type: 'text', text: 'summarise this' },
        {
          type: 'file',
          mimeType: 'text/csv',
          path: 'uploads/ab12cd34-Q3-report.csv',
          name: 'Q3 report.csv',
          sizeBytes: 4096,
        },
      ],
    });
    if (parsed.role !== 'user') throw new Error('unreachable');
    expect(parsed.content[1]).toMatchObject({
      type: 'file',
      path: 'uploads/ab12cd34-Q3-report.csv',
      name: 'Q3 report.csv',
    });
  });

  it('still parses an image part written before file parts existed', () => {
    // Sessions on disk hold these. The union only gained a member, so old
    // `payload_json` has to keep loading — a parse failure here would be a
    // conversation that no longer opens.
    const parsed = ChatMessageSchema.parse({
      role: 'user',
      content: [
        { type: 'image', mimeType: 'image/png', url: '/api/media/legacy' },
      ],
    });
    if (parsed.role !== 'user') throw new Error('unreachable');
    expect(parsed.content[0]).toMatchObject({ type: 'image' });
  });

  it('requires a path on a file part', () => {
    expect(
      ChatMessageSchema.safeParse({
        role: 'user',
        content: [{ type: 'file', mimeType: 'text/csv' }],
      }).success,
    ).toBe(false);
  });

  it('rejects an unknown content part type', () => {
    expect(
      ChatMessageSchema.safeParse({
        role: 'user',
        content: [{ type: 'audio', data: 'x' }],
      }).success,
    ).toBe(false);
  });

  it('defaults an assistant message to no tool calls', () => {
    const parsed = ChatMessageSchema.parse({ role: 'assistant', content: [] });
    expect(parsed).toMatchObject({ toolCalls: [] });
  });

  it('keeps tool-call arguments as a verbatim string', () => {
    // Models emit malformed JSON often enough that parsing must happen in the
    // tool registry, where a failure becomes a retryable tool error rather than
    // an exception in the transport layer.
    const parsed = ChatMessageSchema.parse({
      role: 'assistant',
      content: [],
      toolCalls: [{ id: 'c1', name: 'exec', argumentsJson: '{"argv":["ls",' }],
    });
    expect(parsed.role).toBe('assistant');
    if (parsed.role !== 'assistant') throw new Error('unreachable');
    expect(parsed.toolCalls[0]?.argumentsJson).toBe('{"argv":["ls",');
  });

  it('requires a tool message to name the call it answers', () => {
    // The pairing key `findLegalStart` walks: a tool result with no originating
    // call is a provider 400.
    expect(
      ChatMessageSchema.safeParse({
        role: 'tool',
        name: 'read_file',
        content: 'x',
      }).success,
    ).toBe(false);
    expect(
      ChatMessageSchema.safeParse({
        role: 'tool',
        toolCallId: '',
        name: 'read_file',
        content: 'x',
      }).success,
    ).toBe(false);
  });

  it('treats a failed tool result as a legal history entry', () => {
    const parsed = ChatMessageSchema.parse({
      role: 'tool',
      toolCallId: 'c1',
      name: 'exec',
      content: 'ENOENT',
      isError: true,
    });
    expect(parsed).toMatchObject({ isError: true, truncated: false });
  });

  it('rejects an unknown role', () => {
    expect(
      ChatMessageSchema.safeParse({ role: 'developer', content: 'x' }).success,
    ).toBe(false);
  });

  it('rejects a role mismatch in the payload', () => {
    expect(
      ChatMessageSchema.safeParse({ role: 'user', content: 'a plain string' })
        .success,
    ).toBe(false);
    expect(
      ChatMessageSchema.safeParse({ role: 'system', content: [] }).success,
    ).toBe(false);
  });
});

describe('StoredMessageSchema', () => {
  it('wraps a message with its storage identity', () => {
    const parsed = StoredMessageSchema.parse({
      id: 'm1',
      sessionKey: 'web:abc',
      seq: 1,
      createdAtMs: 1_700_000_000_000,
      turnId: 't1',
      message: { role: 'system', content: 'x' },
    });
    expect(parsed.message.role).toBe('system');
    expect(parsed.turnId).toBe('t1');
  });

  it('allows a message with no turn, for seeded system prompts', () => {
    const parsed = StoredMessageSchema.parse({
      id: 'm1',
      sessionKey: 's',
      seq: 1,
      createdAtMs: 0,
      message: { role: 'system', content: 'x' },
    });
    expect(parsed).not.toHaveProperty('turnId');
  });

  it('rejects a negative timestamp', () => {
    expect(
      StoredMessageSchema.safeParse({
        id: 'm1',
        sessionKey: 's',
        createdAtMs: -1,
        message: { role: 'system', content: 'x' },
      }).success,
    ).toBe(false);
  });
});

describe('UsageSchema', () => {
  it('defaults every counter to zero', () => {
    expect(UsageSchema.parse({})).toMatchObject({
      promptTokens: 0,
      completionTokens: 0,
      totalTokens: 0,
    });
  });

  it('omits cache and reasoning counters when the provider does not report them', () => {
    const parsed = UsageSchema.parse({});
    expect(parsed).not.toHaveProperty('cachedTokens');
    expect(parsed).not.toHaveProperty('reasoningTokens');
  });

  it('rejects negative counts', () => {
    expect(UsageSchema.safeParse({ promptTokens: -5 }).success).toBe(false);
  });
});

describe('tokensPerSecond', () => {
  const usage = { promptTokens: 100, completionTokens: 250, totalTokens: 350 };

  it('divides completion tokens by wall time', () => {
    expect(tokensPerSecond(usage, 1000)).toBe(250);
    expect(tokensPerSecond(usage, 2000)).toBe(125);
  });

  it('reports nothing rather than infinity for an unmeasured turn', () => {
    expect(tokensPerSecond(usage, 0)).toBeUndefined();
    expect(tokensPerSecond(usage, -1)).toBeUndefined();
  });

  it('reports nothing rather than zero for a turn that produced nothing', () => {
    expect(
      tokensPerSecond(
        { promptTokens: 10, completionTokens: 0, totalTokens: 10 },
        1000,
      ),
    ).toBeUndefined();
  });
});

describe('turnRate', () => {
  const usage = { promptTokens: 100, completionTokens: 250, totalTokens: 350 };

  it('divides by generation time, not by the whole turn', () => {
    // The bug this exists to fix, as one assertion. The turn took ten seconds
    // because a local model spent nine of them loading its weights; it
    // generated for one. The wall clock calls that 25 tok/s. It is 250.
    expect(
      turnRate(usage, {
        generationMs: 1000,
        generationTokens: 250,
        elapsedMs: 10_000,
      }),
    ).toBe(250);
  });

  it('divides the timed tokens, not every token the turn was charged for', () => {
    // The correction real Ollama forced. A turn that also made two bare tool
    // calls is charged for their JSON, and those replies arrive in a single
    // frame each — so they are measured at zero and must sit out of both
    // sides. Here `usage` says 250 tokens and only 150 of them were timed:
    // 150 over one second is 150 tok/s, not 250.
    expect(
      turnRate(usage, {
        generationMs: 1000,
        generationTokens: 150,
        elapsedMs: 10_000,
      }),
    ).toBe(150);
  });

  it('falls back to the wall clock when nothing was measured', () => {
    // Every turn recorded before generation time existed. Blanking their rate
    // would be a regression wearing accuracy as a costume.
    expect(turnRate(usage, { elapsedMs: 2000 })).toBe(125);
    expect(turnRate(usage, { generationMs: undefined, elapsedMs: 2000 })).toBe(
      125,
    );
  });

  it('treats a zero window as unmeasured rather than as instant', () => {
    // A reply whose content arrived in a single frame. `0` is a real reading
    // and still not a divisor, so it takes the same branch as absence — which
    // is why the guard tests for zero and not merely for undefined.
    expect(
      turnRate(usage, {
        generationMs: 0,
        generationTokens: 0,
        elapsedMs: 2000,
      }),
    ).toBe(125);
  });

  it('needs both halves of the pair before it will use either', () => {
    // Half a measurement is not a measurement. A window with no token count
    // beside it would otherwise divide the turn's whole `completionTokens` by
    // it — which is the overstatement this pairing exists to prevent.
    expect(turnRate(usage, { generationMs: 1000, elapsedMs: 2000 })).toBe(125);
    expect(
      turnRate(usage, {
        generationMs: 1000,
        generationTokens: 0,
        elapsedMs: 2000,
      }),
    ).toBe(125);
    expect(turnRate(usage, { generationTokens: 150, elapsedMs: 2000 })).toBe(
      125,
    );
  });

  it('reports nothing when there is no divisor at all', () => {
    expect(turnRate(usage, {})).toBeUndefined();
    expect(turnRate(usage, { generationMs: 0 })).toBeUndefined();
    expect(turnRate(usage, { generationMs: 1000 })).toBeUndefined();
  });

  it('reports nothing for a turn that produced no tokens', () => {
    expect(
      turnRate(
        { promptTokens: 10, completionTokens: 0, totalTokens: 10 },
        { generationMs: 1000 },
      ),
    ).toBeUndefined();
  });
});
