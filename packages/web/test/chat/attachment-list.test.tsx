/**
 * What an attachment looks like under a message.
 *
 * Asserted here rather than end-to-end because the interesting states settle
 * only after a signing round trip, and holding them still is the difference
 * between an assertion and a race. `chat.spec.ts` covers the durable outcome —
 * an image is on screen after a reload — and this covers which of the two forms
 * a given attachment takes and why.
 */

import { screen, waitFor } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import type { Attachment } from '@ghostwire/protocol';

import { renderWithProviders, stubFetch } from '@testkit/render.js';

import { AttachmentList } from '@/chat/attachment-list.js';

const SIGNED = { url: '/api/media/token', expiresAtMs: 2_000 };

function show(...attachments: readonly Attachment[]): void {
  stubFetch({ '/api/files/signed-url': [200, SIGNED] });
  renderWithProviders(
    <AttachmentList attachments={attachments} workspace="default" />,
  );
}

describe('an image attachment', () => {
  it('renders the picture, not a text chip', () => {
    // The whole visible half of the bug: every attachment used to be a neutral
    // badge with the filename in it, image or not.
    show({
      mimeType: 'image/png',
      path: 'uploads/ab12cd34-shot.png',
      name: 'shot.png',
    });

    return waitFor(() => {
      const image = screen.getByRole('img', { name: 'shot.png' });
      expect(image).toHaveAttribute('src', SIGNED.url);
      expect(image).toHaveAttribute('loading', 'lazy');
    });
  });

  it('falls back to the path when the attachment lost its name', () => {
    show({ mimeType: 'image/png', path: 'uploads/ab12cd34-shot.png' });

    return waitFor(() => {
      expect(
        screen.getByRole('img', { name: 'ab12cd34-shot.png' }),
      ).toBeInTheDocument();
    });
  });
});

describe('any other attachment', () => {
  it('renders a chip with the name, the size and a download link', async () => {
    show({
      mimeType: 'application/pdf',
      path: 'uploads/ef56ab78-scan.pdf',
      name: 'scan.pdf',
      sizeBytes: 2048,
    });

    expect(screen.getByText('scan.pdf')).toBeInTheDocument();
    expect(screen.getByText('2.0 kB')).toBeInTheDocument();
    expect(screen.queryByRole('img')).not.toBeInTheDocument();

    const download = await screen.findByRole('link', {
      name: 'Download scan.pdf',
    });
    expect(download).toHaveAttribute('href', SIGNED.url);
  });

  it('shows the name before the URL is signed, rather than a spinner', () => {
    // A thumbnail that appears a moment later is fine. A spinner per attachment
    // in a scrolling transcript is not.
    show({
      mimeType: 'application/pdf',
      path: 'uploads/ef56ab78-scan.pdf',
      name: 'scan.pdf',
    });

    expect(screen.getByText('scan.pdf')).toBeInTheDocument();
  });

  it('omits the size when the message did not record one', () => {
    show({
      mimeType: 'text/csv',
      path: 'uploads/ab12cd34-rows.csv',
      name: 'rows.csv',
    });

    expect(screen.getByText('rows.csv')).toBeInTheDocument();
    expect(screen.queryByText(/ B$/)).not.toBeInTheDocument();
  });
});

describe('no attachments', () => {
  it('renders nothing at all', () => {
    renderWithProviders(
      <AttachmentList attachments={[]} workspace="default" />,
    );

    // Not an empty `<ul>`: the list sits inside a `.stack`, which would give a
    // gap to a row that has no height.
    expect(document.querySelector('.message-user__attachments')).toBeNull();
  });
});
