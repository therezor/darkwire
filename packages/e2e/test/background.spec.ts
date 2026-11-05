/**
 * A background job, end to end: `exec` hands it off, and its result comes back
 * into the session as a card and wakes the agent.
 *
 * Asserts only what the transcript settles into. Whether the job is still
 * running when the first turn ends depends on the machine, and the strip that
 * shows it running is a component test's.
 */

import { expect, test } from '../src/fixtures.js';

test.describe('a background job', () => {
  test('reports back into the session as a result card', async ({ app }) => {
    const message = app.getByRole('textbox', { name: 'Message' });
    const transcript = app.getByTestId('transcript');
    await message.fill('build it in the background');
    await message.press('Enter');

    const call = app.getByRole('region', { name: 'Tool call: exec' });
    await call.getByRole('button', { name: 'Once', exact: true }).click();

    const result = transcript.getByRole('region', { name: 'Background job 1' });
    await expect(result).toBeVisible({ timeout: 15_000 });
    await expect(
      transcript.getByText('The background job finished.'),
    ).toBeVisible({ timeout: 15_000 });

    await result.getByRole('button').click();
    await expect(result).toContainText('Exit code: 0');
  });
});
