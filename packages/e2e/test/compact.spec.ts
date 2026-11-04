/**
 * `/compact`, through the real server and model.
 *
 * Every assertion is on a durable state: the address the tab ends on, the
 * summary in the new transcript, the link back, and a later answer landing in
 * the new conversation.
 */

import { SUMMARY } from '../src/harness/script.js';
import { expect, test } from '../src/fixtures.js';

test.describe('compacting a conversation', () => {
  test('carries on in a new session that opens with the summary', async ({
    app,
  }) => {
    const message = app.getByRole('textbox', { name: 'Message' });
    const transcript = app.getByTestId('transcript');

    await message.fill('stream a long answer');
    await message.press('Enter');
    await expect(transcript.getByText('That is the whole of it.')).toBeVisible({
      timeout: 15_000,
    });
    await expect(app.getByRole('button', { name: 'Send' })).toBeVisible();
    const before = app.url();

    await message.fill('/compact');
    await message.press('Enter');

    await expect(app).not.toHaveURL(before, { timeout: 15_000 });
    await expect(transcript.getByText(SUMMARY)).toBeVisible({
      timeout: 15_000,
    });
    await expect(
      app.getByRole('link', { name: /^Continued from / }),
    ).toBeVisible();

    // The conversation goes on where the summary is.
    const after = app.url();
    await message.fill('stream it again');
    await message.press('Enter');
    await expect(transcript.getByText('That is the whole of it.')).toBeVisible({
      timeout: 15_000,
    });
    await expect(app).toHaveURL(after);
  });
});
