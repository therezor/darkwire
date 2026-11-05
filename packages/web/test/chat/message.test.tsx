/**
 * A transcript item as the transcript draws it.
 */

import { screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { Message } from '@/chat/message.js';
import { renderWithProviders } from '@testkit/render.js';

describe('a message', () => {
  it('draws a posted result as a result card, with none of a message’s actions', () => {
    renderWithProviders(
      <Message
        item={{
          kind: 'user',
          id: 'm1',
          clientMessageId: undefined,
          turnId: 't1',
          seq: 1,
          text: '[Background job 1 finished: make]',
          attachments: [],
          pending: false,
          refused: undefined,
          steering: false,
          from: { kind: 'exec', id: '1', label: 'make', status: 'ok' },
        }}
        streaming={false}
        onApprove={vi.fn()}
        onAction={vi.fn()}
      />,
    );

    expect(
      screen.getByRole('region', { name: 'Background job 1' }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole('button', { name: /edit/i }),
    ).not.toBeInTheDocument();
  });
});
