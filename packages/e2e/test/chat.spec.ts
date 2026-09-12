/**
 * The turn, end to end and in a browser.
 *
 * Everything here has a unit test somewhere already — the hub's queue, the
 * block splitter, the tool card's states, the abort chain. What none of those
 * can say is that the pieces are wired to each other: that a keystroke reaches
 * the loop, that the loop's events reach the transcript, and that Stop reaches
 * a running child. That is the whole subject of this file, and it is why it
 * drives the real composition rather than a mocked socket.
 */

import { expect, test } from '../src/fixtures.js';

test.describe('a turn', () => {
  test('streams an answer, its reasoning and its code block', async ({
    app,
  }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('button', { name: 'Send' }).click();

    const transcript = app.getByTestId('transcript');
    await expect(transcript.getByText('Here is what I found.')).toBeVisible();
    await expect(
      transcript.getByText('That is the whole of it.'),
    ).toBeVisible();

    // The fence became a code block rather than a paragraph containing
    // backticks — which is the difference the block splitter exists to make.
    await expect(transcript.locator('pre code')).toContainText('const note =');

    // Reasoning is its own thing rather than folded into the answer — and it
    // is behind a disclosure that collapsed itself the moment the first answer
    // token arrived, which is the behaviour rather than an inconvenience.
    await expect(transcript.getByText(/Checking the workspace/)).toBeHidden();
    await transcript.getByRole('button', { name: 'Reasoning' }).click();
    await expect(transcript.getByText(/Checking the workspace/)).toBeVisible();

    // The turn finished: the composer is offering Send again, not Stop.
    await expect(app.getByRole('button', { name: 'Send' })).toBeVisible();
  });

  test('renders a tool card for a call that needs no approval', async ({
    app,
  }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('list the workspace');
    await app.getByRole('button', { name: 'Send' }).click();

    const card = app.getByRole('region', { name: 'Tool call: list_dir' });
    await expect(card).toBeVisible();
    // `list_dir` is in the `safe` band, so nothing stood between the call and
    // its execution.
    await expect(card.getByText('needs approval to run')).toHaveCount(0);

    // Expanding shows what the tool actually returned — the workspace fixture.
    await card.getByRole('button', { expanded: false }).click();
    await expect(card.getByText('notes.md')).toBeVisible();

    await expect(
      app.getByTestId('transcript').getByText('The workspace holds'),
    ).toBeVisible();
  });

  test('Stop aborts a turn while a tool is still running', async ({ app }) => {
    await app.getByRole('textbox', { name: 'Message' }).fill('wait for me');
    await app.getByRole('button', { name: 'Send' }).click();

    // The tool is running and will not finish on its own for a minute, so
    // anything observed after this point is observed mid-call.
    const card = app.getByRole('region', { name: 'Tool call: e2e_wait' });
    await expect(card).toBeVisible();
    await expect(card.getByLabel('Running')).toBeVisible();

    const stop = app.getByRole('button', { name: 'Stop the current turn' });
    await expect(stop).toBeVisible();
    await stop.click();

    // The composer went back to Send, which is driven by `session.status.busy`
    // — so the server agreed the turn is over, rather than the button merely
    // having been pressed.
    await expect(app.getByRole('button', { name: 'Send' })).toBeVisible();
    await expect(card.getByLabel('Running')).toHaveCount(0);
  });

  test('a queued message says so rather than disabling the composer', async ({
    app,
  }) => {
    await app.getByRole('textbox', { name: 'Message' }).fill('stall here');
    await app.getByRole('button', { name: 'Send' }).click();
    await expect(
      app.getByRole('button', { name: 'Stop the current turn' }),
    ).toBeVisible();

    // Enter still sends while a turn runs: the hub queues it. A composer that
    // disabled itself would lose whatever the user was mid-way through typing.
    const message = app.getByRole('textbox', { name: 'Message' });
    await expect(message).toBeEditable();
    await message.fill('stall again');
    await message.press('Enter');

    await expect(
      app.getByText(/message waiting|messages waiting/),
    ).toBeVisible();
    await app.getByRole('button', { name: 'Stop the current turn' }).click();
  });
});

/**
 * Reworking what is already on screen.
 *
 * Both of these are truncate-then-re-run: the server drops the messages after a
 * point and starts a turn from it. What that looks like from the outside is an
 * answer being *replaced* rather than a second one appearing underneath, and
 * that distinction is the whole point — a transcript that grew a second answer
 * every time you asked for one would be a transcript nobody could read back.
 */
/**
 * The line under the composer, on a screen too narrow to hold it.
 *
 * A media query is the one kind of change a unit test cannot see — jsdom has no
 * layout, so `flex-direction: column` is a string in a stylesheet until a real
 * browser resolves it. The assertion is geometric on purpose: the two things in
 * that row occupy different rows here, which is the actual claim.
 *
 * It used to measure the keyboard hint against the budget. The hint has moved
 * to the welcome screen — it never changed, and it was taking the width from
 * the one thing in this row that does — so the pair being measured is now the
 * agent picker and the budget.
 */
test.describe('the composer on a narrow screen', () => {
  test('puts the agent picker and the context budget on separate rows', async ({
    app,
  }) => {
    // Resized here rather than through `test.use`: the `app` fixture waits for
    // the inline sidebar, which below the shell's `md` breakpoint is a drawer
    // and never appears. Boot wide, then narrow.
    await app.setViewportSize({ width: 480, height: 900 });

    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('button', { name: 'Send' }).click();
    await expect(
      app.getByTestId('transcript').getByText('Here is what I found.'),
    ).toBeVisible({
      timeout: 15_000,
    });

    const picker = app.getByRole('button', { name: /^Agent: / });
    const budget = app.getByRole('button', {
      name: /of .* tokens|of [\d,]+ ·/u,
    });
    await expect(budget).toBeVisible({ timeout: 15_000 });

    const pickerBox = await picker.boundingBox();
    const budgetBox = await budget.boundingBox();
    if (pickerBox === null || budgetBox === null) {
      throw new Error('no layout to measure');
    }

    // Stacked: the budget starts below the picker ends, rather than beside it.
    expect(budgetBox.y).toBeGreaterThanOrEqual(pickerBox.y + pickerBox.height);
  });

  test('shows the keyboard hint where a first-time reader is already looking', async ({
    app,
  }) => {
    // On the welcome screen, once, rather than under the box on every render
    // for the life of the install.
    await expect(
      app.getByText('Enter to send', { exact: false }),
    ).toBeVisible();

    await app.getByRole('textbox', { name: 'Message' }).fill('hello');
    await app.getByRole('button', { name: 'Send' }).click();
    await expect(app.getByTestId('transcript').getByText('hello')).toBeVisible({
      timeout: 15_000,
    });

    // And gone once there is a session, because the row under the box is
    // the budget's now.
    await expect(app.getByText('Enter to send', { exact: false })).toHaveCount(
      0,
    );
  });
});

test.describe('reworking a turn', () => {
  test('regenerate replaces the answer rather than appending one', async ({
    app,
  }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('button', { name: 'Send' }).click();

    const answer = app
      .getByTestId('transcript')
      .getByText('Here is what I found.');
    await expect(answer).toBeVisible({ timeout: 15_000 });

    const question = app
      .getByTestId('transcript')
      .getByText('stream a long answer');
    await expect(question).toHaveCount(1);

    await app.getByRole('button', { name: 'Regenerate the answer' }).click();

    // Still exactly one. The scripted provider says the same thing again, so a
    // second copy would be the transcript growing rather than being rebuilt.
    await expect(answer).toHaveCount(1, { timeout: 15_000 });
    await expect(app.getByRole('button', { name: 'Send' })).toBeVisible({
      timeout: 15_000,
    });

    // And the question is still there, exactly once. Re-running deletes that row
    // and writes it back, and the client used to rebuild over the gap — so the
    // message vanished for good. Asserted after the turn has settled, which is
    // the durable state: whether it is momentarily `pending` mid-flight is the
    // kind of timing the runner decides, and not something to hold a test to.
    await expect(question).toHaveCount(1);
  });

  test('the info button opens what the turn cost', async ({ app }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('button', { name: 'Send' }).click();
    await expect(
      app.getByTestId('transcript').getByText('Here is what I found.'),
    ).toBeVisible({
      timeout: 15_000,
    });

    await app.getByRole('button', { name: 'Turn details' }).click();

    // The scripted provider reports this usage, so the figure proves the panel
    // is reading the turn rather than rendering an empty shell.
    const details = app.getByRole('dialog');
    await expect(details.getByText('412')).toBeVisible();
    await expect(details.getByText('Model')).toBeVisible();
  });

  test('editing a message re-runs the turn from the new wording', async ({
    app,
  }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('button', { name: 'Send' }).click();
    await expect(
      app.getByTestId('transcript').getByText('Here is what I found.'),
    ).toBeVisible({
      timeout: 15_000,
    });

    await app.getByRole('button', { name: 'Edit this message' }).click();

    const editor = app.getByRole('textbox', { name: 'Edit message' });
    await editor.fill('list the workspace');
    await app.getByRole('button', { name: 'Save' }).click();

    // The new wording ran: `list the workspace` is a different scripted route,
    // so its tool card is proof the edit reached the model rather than only the
    // transcript.
    await expect(
      app.getByRole('region', { name: 'Tool call: list_dir' }),
    ).toBeVisible({
      timeout: 15_000,
    });
    await expect(
      app.getByTestId('transcript').getByText('Here is what I found.'),
    ).toHaveCount(0);
  });

  test('an attached image is still on screen after a reload', async ({
    app,
  }) => {
    // Two durable states, and the reload is the point of the test. An
    // attachment used to be stored as a signed URL with a ten-minute life, so
    // the transcript held a dead link the moment the tab sat idle -- and the
    // name and size were never stored at all, so even the chip forgot what it
    // was. Both are settled states with the turn finished, not the in-between
    // wording of an upload.
    const picker = app.locator('input[type="file"]');
    await picker.setInputFiles([
      { name: 'shot.png', mimeType: 'image/png', buffer: onePixelPng() },
      {
        name: 'rows.csv',
        mimeType: 'text/csv',
        buffer: Buffer.from('date,amount\n'),
      },
    ]);

    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('button', { name: 'Send' }).click();
    await expect(
      app.getByTestId('transcript').getByText('Here is what I found.'),
    ).toBeVisible({
      timeout: 15_000,
    });

    const transcript = app.getByTestId('transcript');
    await expect(
      transcript.getByRole('img', { name: 'shot.png' }),
    ).toBeVisible();
    await expect(transcript.getByText('rows.csv')).toBeVisible();

    await app.reload();

    // Rebuilt from storage rather than from the socket's live frames: this is
    // the assertion the old shape could not pass.
    await expect(transcript.getByRole('img', { name: 'shot.png' })).toBeVisible(
      {
        timeout: 15_000,
      },
    );
    await expect(transcript.getByText('rows.csv')).toBeVisible();
  });
});

/** The smallest valid PNG, so the fixture is bytes rather than a file on disk. */
function onePixelPng(): Buffer {
  return Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
    'base64',
  );
}
