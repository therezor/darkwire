/**
 * The token budget.
 *
 * Three properties carry the panel: the sections come back in a fixed order
 * whatever order the JSON had, the segments account for the total the server
 * reported, and a budget past the window says so instead of rendering as full.
 * The last one is the reason the panel exists — a bar that reads 100% for both
 * "exactly full" and "twice over" cannot answer the question it was opened for.
 */

import { describe, expect, it } from 'vitest';

import { createWebI18n } from '@ghostwire/i18n/web';

/**
 * A real instance rather than `(key) => key`: these assertions compare the
 * rendered label text, so a stub would prove the keys are wired and say nothing
 * about whether they resolve.
 */
const t = createWebI18n('en').getFixedT(null, 'web');

import { summariseContext } from '@/context/breakdown.js';

const input = {
  breakdown: { messages: 3000, systemPrompt: 1000, tools: 1000 },
  estimatedTokens: 5000,
  contextWindowTokens: 10_000,
};

describe('summariseContext', () => {
  it('orders the known sections regardless of the key order it received', () => {
    // JSON preserves insertion order, and the server's is not the reading order.
    expect(
      summariseContext(input, t).segments.map((segment) => segment.key),
    ).toEqual(['systemPrompt', 'tools', 'messages']);
  });

  it('measures each section against the window', () => {
    const { segments, usedPercent, freeTokens, over } = summariseContext(
      input,
      t,
    );

    expect(segments.map((segment) => segment.percent)).toEqual([10, 10, 30]);
    expect(usedPercent).toBe(50);
    expect(freeTokens).toBe(5000);
    expect(over).toBe(false);
  });

  it('labels the sections in words', () => {
    expect(
      summariseContext(input, t).segments.map((segment) => segment.label),
    ).toEqual(['System prompt', 'Tool definitions', 'Session']);
  });

  it('accounts for whatever the sections do not add up to', () => {
    // The server's total is authoritative; a bar that does not sum to the number
    // printed above it is a bar nobody trusts twice.
    const budget = summariseContext({ ...input, estimatedTokens: 6000 }, t);
    const other = budget.segments.at(-1);

    expect(other?.key).toBe('other');
    expect(other?.tokens).toBe(1000);
    expect(
      budget.segments.reduce((total, segment) => total + segment.tokens, 0),
    ).toBe(6000);
  });

  it('adds no remainder when the sections already add up', () => {
    expect(summariseContext(input, t).segments.map((s) => s.key)).not.toContain(
      'other',
    );
  });

  it('reports a budget past the window rather than clamping it', () => {
    const budget = summariseContext({ ...input, estimatedTokens: 12_000 }, t);

    expect(budget.over).toBe(true);
    expect(budget.usedPercent).toBe(120);
    // Not negative: "remaining" is a quantity, and there is none.
    expect(budget.freeTokens).toBe(0);
  });

  it('sorts a section it has never seen after the known ones, and names it', () => {
    const budget = summariseContext(
      {
        ...input,
        breakdown: { ...input.breakdown, scratch_buffer: 500, memory: 200 },
        estimatedTokens: 5700,
      },
      t,
    );

    expect(budget.segments.map((segment) => segment.key)).toEqual([
      'systemPrompt',
      'tools',
      'messages',
      'memory',
      'scratch_buffer',
    ]);
    expect(budget.segments.at(4)?.label).toBe('Scratch buffer');
  });

  it('survives a window of zero rather than dividing by it', () => {
    const budget = summariseContext({ ...input, contextWindowTokens: 0 }, t);

    expect(budget.segments.every((segment) => segment.percent === 0)).toBe(
      true,
    );
    expect(budget.usedPercent).toBe(0);
  });

  it('treats an empty breakdown as one unattributed block', () => {
    const budget = summariseContext(
      {
        breakdown: {},
        estimatedTokens: 400,
        contextWindowTokens: 800,
      },
      t,
    );

    // Unattributed by construction, so it counts as uncached — the honest side
    // to err on for a figure nobody can point at.
    expect(budget.segments).toEqual([
      {
        key: 'other',
        label: 'Unattributed',
        tokens: 400,
        percent: 50,
        cacheable: false,
      },
    ]);
    expect(budget.uncachedTokens).toBe(400);
  });

  it('separates what a prompt cache can serve from what every step pays again', () => {
    const budget = summariseContext(
      {
        breakdown: {
          systemPrompt: 1000,
          tools: 500,
          messages: 2000,
          runtimeBlock: 120,
        },
        estimatedTokens: 3620,
        contextWindowTokens: 10_000,
      },
      t,
    );

    // Request order, which is also cached-then-not.
    expect(budget.segments.map((segment) => segment.key)).toEqual([
      'systemPrompt',
      'tools',
      'messages',
      'runtimeBlock',
    ]);
    expect(budget.segments.map((segment) => segment.cacheable)).toEqual([
      true,
      true,
      true,
      false,
    ]);
    // The figure the split exists to move: the trailing turn, and nothing else.
    expect(budget.uncachedTokens).toBe(120);
  });
});
