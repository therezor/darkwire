/**
 * The approval prompt, both ways.
 *
 * `exec` is seeded at `ask` on a new agent, which is what a browser-facing
 * server ships with and therefore what these tests leave alone. Both paths
 * continue into the same second model turn — approve and deny differ in what
 * the tool result says, not in whether the loop keeps going — so the assertion
 * that separates them is what the card reports, not whether an answer arrived.
 */

import { expect, test } from '../src/fixtures.js';

test.describe('a tool that needs approval', () => {
  test('runs after approval and reports its output', async ({ app }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('run the version command');
    await app.getByRole('button', { name: 'Send' }).click();

    const card = app.getByRole('region', { name: 'Tool call: exec' });
    await expect(card).toBeVisible();
    await expect(card.getByText(/needs approval to run/)).toBeVisible();
    // The card opens itself when a decision is needed: a prompt inside a
    // collapsed card is a turn that has silently stopped.
    await expect(card.getByRole('button', { name: /Once/ })).toBeVisible();

    await card.getByRole('button', { name: 'Once', exact: true }).click();

    // The durable state, not the prompt's "Approved — waiting for the agent."
    // line — the same race the denial below avoids. The line lives only while
    // the gate is still waiting, and a scripted provider answers inside a frame,
    // so asserting on it here passes or fails on how busy the runner is. Its
    // rendering is covered deterministically in `chat/approval.test.tsx`.
    await expect(card.getByLabel('Succeeded')).toBeVisible();
    await expect(
      app.getByTestId('transcript').getByText('That is the runtime version.'),
    ).toBeVisible();
  });

  test('is refused after a denial, and the turn continues', async ({ app }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('run the version command');
    await app.getByRole('button', { name: 'Send' }).click();

    const card = app.getByRole('region', { name: 'Tool call: exec' });
    await expect(card.getByText(/needs approval to run/)).toBeVisible();
    await card.getByRole('button', { name: 'Deny' }).click();

    // The durable state, not the prompt's "Denied — waiting for the agent."
    // line: that one is replaced the moment the loop moves on, so asserting on
    // it is a race against a scripted provider that answers instantly.
    await expect(card.getByLabel('Failed')).toBeVisible();
    // And the band survives the turn landing in storage, which does not record
    // it — a call the operator was asked to approve must not go on to describe
    // itself as a read.
    await expect(card.getByLabel('Risk: exec')).toBeVisible();
    await card.getByRole('button', { expanded: false }).click();
    await expect(card.getByText(/the user refused this call/)).toBeVisible();
    // A denial is an answer to the model, not an error to the operator: the
    // loop takes the refusal as the tool result and produces its next turn.
    await expect(
      app.getByTestId('transcript').getByText('That is the runtime version.'),
    ).toBeVisible();
    await expect(app.getByRole('button', { name: 'Send' })).toBeVisible();
  });
});

// The same journey with one setting moved, which is the only honest way to
// assert that the prompt is the agent's *permission* speaking rather than
// something the tool card does on its own.
test.describe('with exec allowed', () => {
  test.use({
    harnessOptions: {
      config: {
        agents: {
          list: {
            // The whole map, because it replaces rather than merges — an agent
            // holding only `exec` could not read a file to answer with.
            default: {
              tools: {
                read_file: 'allow',
                list_dir: 'allow',
                write_file: 'allow',
                edit_file: 'allow',
                e2e_wait: 'allow',
                exec: 'allow',
              },
            },
          },
        },
      },
    },
  });

  test('runs unattended', async ({ app }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('run the version command');
    await app.getByRole('button', { name: 'Send' }).click();

    const card = app.getByRole('region', { name: 'Tool call: exec' });
    await expect(card.getByLabel('Succeeded')).toBeVisible();
    await expect(card.getByText(/needs approval to run/)).toHaveCount(0);
  });
});
