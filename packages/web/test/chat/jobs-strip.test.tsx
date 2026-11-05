/**
 * The running-jobs strip above the composer.
 *
 * Its state holds still here, which is why the in-flight wording is asserted
 * in this file and never in an e2e spec.
 */

import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { JobsStrip } from '@/chat/jobs-strip.js';
import { stopJob } from '@/lib/connection.js';
import { useTurnStore } from '@/state/turn.js';
import { renderWithProviders } from '@testkit/render.js';

vi.mock('@/lib/connection.js', () => ({ stopJob: vi.fn() }));

function running(...labels: string[]): void {
  useTurnStore.setState({
    jobs: labels.map((label, index) => ({
      id: String(index + 1),
      label,
      startedAtMs: Date.now(),
    })),
  });
}

describe('the jobs strip', () => {
  beforeEach(() => {
    useTurnStore.getState().reset();
    vi.mocked(stopJob).mockClear();
  });

  it('renders nothing while no job runs', () => {
    renderWithProviders(<JobsStrip />);

    expect(
      screen.queryByRole('region', { name: 'Background jobs' }),
    ).not.toBeInTheDocument();
  });

  it('lists each running job by its command', () => {
    running('pnpm dev', 'cargo build');

    renderWithProviders(<JobsStrip />);

    expect(
      screen.getByRole('region', { name: 'Background jobs' }),
    ).toBeInTheDocument();
    expect(screen.getByText('pnpm dev')).toBeInTheDocument();
    expect(screen.getByText('cargo build')).toBeInTheDocument();
  });

  it('stops the job whose button was pressed', async () => {
    running('pnpm dev', 'cargo build');
    renderWithProviders(<JobsStrip />);

    await userEvent.click(
      screen.getByRole('button', { name: 'Stop cargo build' }),
    );

    expect(stopJob).toHaveBeenCalledWith('2');
  });
});
