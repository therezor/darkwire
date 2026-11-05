/**
 * A posted result, drawn from its typed source rather than its header text.
 */

import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { DeliveredCard } from '@/chat/delivered-card.js';
import { renderWithProviders } from '@testkit/render.js';

describe('a posted result', () => {
  it('names a background job and how it ended, with its output behind a click', async () => {
    renderWithProviders(
      <DeliveredCard
        from={{ kind: 'exec', id: '2', label: 'make test', status: 'error' }}
        text={'[Background job 2 failed: make test]\n\nExit code: 1'}
      />,
    );

    const header = screen.getByRole('button', { expanded: false });
    expect(header).toHaveTextContent('Background job 2');
    expect(header).toHaveTextContent('make test');
    expect(header).toHaveTextContent('failed');
    expect(screen.getByLabelText('Failed')).toBeInTheDocument();
    expect(screen.queryByText(/Exit code: 1/)).not.toBeInTheDocument();

    await userEvent.click(header);

    expect(screen.getByText(/Exit code: 1/)).toBeInTheDocument();
  });

  it('names a scheduled job by its name', () => {
    renderWithProviders(
      <DeliveredCard
        from={{ kind: 'automation', id: 'j1', label: 'nightly', status: 'ok' }}
        text='[Scheduled job "nightly" finished]'
      />,
    );

    const header = screen.getByRole('button', { expanded: false });
    expect(header).toHaveTextContent('Scheduled job');
    expect(header).toHaveTextContent('nightly');
    expect(screen.getByLabelText('Succeeded')).toBeInTheDocument();
  });
});
