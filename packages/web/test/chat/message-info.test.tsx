/**
 * One message's details, behind the info button on its line.
 *
 * The line already says how long the message took, what it wrote and how fast,
 * so this holds only what the line does not: the model, what it read, how much
 * of that was cached, and the wait before the first token.
 */

import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import type { StepStats } from '@darkwire/protocol';

import { renderWithProviders } from '@testkit/render.js';

import { MessageInfo } from '@/chat/message-info.js';

const STATS: StepStats = {
  usage: {
    promptTokens: 900,
    completionTokens: 45,
    totalTokens: 945,
    cachedTokens: 600,
  },
  model: 'qwen3:8b',
  elapsedMs: 1500,
  generationMs: 900,
  generationTokens: 45,
  firstTokenMs: 200,
};

async function open(stats: StepStats): Promise<void> {
  renderWithProviders(<MessageInfo number={3} stats={stats} />);
  await userEvent.click(
    screen.getByRole('button', { name: 'Message 3 details' }),
  );
}

describe("a message's details", () => {
  it('names the model, what it read, the cache and the first token', async () => {
    await open(STATS);

    expect(await screen.findByText('qwen3:8b')).toBeInTheDocument();
    expect(screen.getByText('900')).toBeInTheDocument();
    expect(screen.getByText('600')).toBeInTheDocument();
    expect(screen.getByText('200ms')).toBeInTheDocument();
  });

  it('leaves out what the provider did not report', async () => {
    await open({
      usage: { promptTokens: 900, completionTokens: 45, totalTokens: 945 },
      elapsedMs: 1500,
    });

    expect(await screen.findByText('900')).toBeInTheDocument();
    expect(screen.queryByText('Model')).not.toBeInTheDocument();
    expect(screen.queryByText('Cached')).not.toBeInTheDocument();
    expect(screen.queryByText('First token')).not.toBeInTheDocument();
  });

  it('repeats nothing the line under the message already says', async () => {
    await open(STATS);

    expect(await screen.findByText('qwen3:8b')).toBeInTheDocument();
    expect(screen.queryByText('45')).not.toBeInTheDocument();
    expect(screen.queryByText(/tok\/s/)).not.toBeInTheDocument();
  });
});
