/**
 * One agent handing a task to another, through the real stack.
 *
 * The whole chain is live here and that is the point: a real `AgentLoop`
 * resolves a second real `AgentLoop` from the runtime's cache, runs a turn on
 * it against a second session in SQLite, and its events reach the browser as
 * `subagent.event` frames over the same socket the caller's use.
 *
 * **Only durable state is asserted.** A delegation streams — the card opens
 * itself, says "working", and closes on `turn.end` — and the scripted provider
 * answers inside a frame, so every one of those is a race against how busy the
 * runner is. This spec waits for what the run settles into; the in-flight
 * wording is held still in `chat/subagent.test.tsx`.
 */

import type { Locator, Page } from '@playwright/test';

import { expect, test } from '../src/fixtures.js';
import { SUBAGENT_TASK } from '../src/harness/script.js';

/**
 * The delegating card, and its *own* header.
 *
 * Every locator here goes through the header rather than the card, because a
 * delegating card contains another tool card: `card.getByLabel('Succeeded')`
 * matches the outer status glyph and the nested one, and Playwright's strict
 * mode fails on the ambiguity. The header is the one row that belongs to this
 * card alone.
 */
function delegation(app: Page): { card: Locator; header: Locator } {
  const card = app.getByRole('region', { name: 'Tool call: ask_researcher' });
  return { card, header: card.getByRole('button', { name: /ask_researcher/ }) };
}

/**
 * Settles the disclosure open, so a test never depends on auto-open.
 *
 * A poll rather than read-once-then-click, and the difference is the repo rule
 * about transient state wearing a different hat. `open` is `useState` seeded
 * from the tool's status and pushed open again when a *live* subagent run
 * appears — so "it is closed" is a fact with a shelf life, and a click aimed at
 * it can cross the component's own state change or land on a card React has
 * just remounted with its initial value. Against an in-process server those two
 * were the same frame and nothing could get between them. Against a process
 * they are not, and a card that ended up closed took the run, the nested card
 * and every assertion below it off the screen with it.
 *
 * Open is the durable state, so this waits for open.
 */
async function expand(header: Locator): Promise<void> {
  await expect
    .poll(async () => {
      if ((await header.getAttribute('aria-expanded')) === 'true') return true;
      await header.click();
      return (await header.getAttribute('aria-expanded')) === 'true';
    })
    .toBe(true);
}

test.use({
  harnessOptions: {
    config: {
      agents: {
        list: {
          // A second agent, and the default pointed at it. Nothing else about
          // it is special — it is an ordinary entry, which is the design.
          researcher: {
            label: 'Researcher',
            provider: 'ollama',
            model: 'qwen3',
            tools: { list_dir: 'allow', read_file: 'allow' },
          },
          default: {
            // `permission` is spelled out because `ConfigPatch` is the schema's
            // *output* type — the protocol keeps input and output identical so
            // the OpenAPI document describes what the server enforces, which
            // means a defaulted field is still required of a TypeScript literal.
            // A hand-written `config.json` may leave it out; this cannot.
            subagents: [
              {
                id: 'researcher',
                prompt: 'Ask when you need to look something up.',
                permission: 'allow',
              },
            ],
          },
        },
      },
    },
  },
});

test.describe('a delegating agent', () => {
  test('runs the subagent and answers from what it returned', async ({
    app,
  }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('delegate this to the researcher');
    await app.getByRole('button', { name: 'Send' }).click();

    // The delegation is a tool call like any other, named after the agent.
    const { card, header } = delegation(app);
    await expect(card).toBeVisible();
    await expect(header.getByLabel('Succeeded')).toBeVisible();

    // What the caller did with the answer.
    await expect(
      app.getByTestId('transcript').getByText('The researcher found notes.md.'),
    ).toBeVisible();
  });

  test('shows what the subagent did, nested inside the card', async ({
    app,
  }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('delegate this to the researcher');
    await app.getByRole('button', { name: 'Send' }).click();

    const { card, header } = delegation(app);
    await expect(header.getByLabel('Succeeded')).toBeVisible();
    await expand(header);

    const run = card.getByRole('region', { name: 'Subagent run: Researcher' });
    await expect(run).toBeVisible();
    // The task the caller wrote, as the argument on the card. `first()` because
    // the same sentence is also the subagent's own opening message inside the
    // run — which is exactly the point: one string, two places it has to be.
    await expect(card.getByText(SUBAGENT_TASK).first()).toBeVisible();

    // The subagent's own tool call, as a card of its own inside the run — the
    // same landmark and the same status label a top-level call gets.
    const nested = run.getByRole('region', { name: 'Tool call: list_dir' });
    await expect(nested).toBeVisible();
    await expect(nested.getByLabel('Succeeded')).toBeVisible();
    await expect(run.getByText('There is one file: notes.md.')).toBeVisible();
  });

  test('can still show the run after a reload', async ({ app }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('delegate this to the researcher');
    await app.getByRole('button', { name: 'Send' }).click();
    await expect(delegation(app).header.getByLabel('Succeeded')).toBeVisible();

    // The events are gone; the rows are not. The parent's history names the
    // subagent's session, and the card fetches it when a reader opens it.
    await app.reload();

    const { card, header } = delegation(app);
    await expect(header.getByLabel('Succeeded')).toBeVisible();
    await expand(header);

    const run = card.getByRole('region', { name: 'Subagent run: Researcher' });
    await expect(run).toBeVisible();
    await expect(
      run.getByRole('region', { name: 'Tool call: list_dir' }),
    ).toBeVisible();
    await expect(run.getByText('There is one file: notes.md.')).toBeVisible();
  });

  /**
   * Both halves of one decision, which is why they are one test.
   *
   * A delegation is not a conversation, so it is not in the column of them: the
   * sidebar is a shortlist of thirty, and an agent that delegates three times a
   * turn would fill it with rows nobody chose to open.
   *
   * But it *is* the turn that produced the answer, and the last time these were
   * hidden they were hidden everywhere — which left no way to read the run when
   * the answer was wrong. So the absence is only meaningful next to the
   * presence, and asserting either alone would let the other regress silently.
   * The third way in — the card in the parent's transcript — is covered by the
   * reload test above.
   */
  test('keeps the subagent run out of the sidebar, and on the sessions list', async ({
    app,
  }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('delegate this to the researcher');
    await app.getByRole('button', { name: 'Send' }).click();
    await expect(delegation(app).header.getByLabel('Succeeded')).toBeVisible();

    // The parent conversation is there, so this asserts a filtered list rather
    // than an empty or still-loading one — without it, a sidebar that failed to
    // load at all would pass.
    const sidebar = app.getByRole('complementary', { name: 'Sidebar' });
    await expect(
      sidebar.getByRole('link', { name: /delegate this/ }),
    ).toBeVisible();
    await expect(sidebar.getByText(SUBAGENT_TASK)).toHaveCount(0);

    // Still listed where the record is kept, badged for what it is. The row's
    // link is labelled `Open {{title}}`, hence the pattern rather than the bare
    // task.
    await sidebar.getByRole('link', { name: 'Sessions' }).click();
    await expect(
      app.getByRole('link', { name: new RegExp(SUBAGENT_TASK) }),
    ).toBeVisible();
  });
});

/**
 * The reload that used to lose the run.
 *
 * Its own server, because the thing under test is what happens when the replay
 * ring cannot cover the gap — and with a ring of any size these scripted turns
 * fit inside it, so the fallback would never be reached. `replayBufferSize: 0`
 * makes every resume incomplete, which is what a real delegation does to a
 * ring of 512 within a second of streaming.
 *
 * Nothing transient is asserted. The subagent stops in a tool that does not
 * finish, so "a delegation is in flight" is a state the spec holds rather than
 * a moment it has to catch, and everything checked after the reload was on
 * screen before it.
 */
test.describe('reloading mid-delegation', () => {
  test.use({
    harnessOptions: {
      config: {
        server: { replayBufferSize: 0 },
        agents: {
          list: {
            researcher: {
              label: 'Researcher',
              provider: 'ollama',
              model: 'qwen3',
              tools: {
                list_dir: 'allow',
                read_file: 'allow',
                e2e_wait: 'allow',
              },
            },
            default: {
              subagents: [
                {
                  id: 'researcher',
                  prompt: 'Ask when you need to look something up.',
                  permission: 'allow',
                },
              ],
            },
          },
        },
      },
    },
  });

  test('comes back to what the subagent had already done', async ({ app }) => {
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('handover to the researcher');
    await app.getByRole('button', { name: 'Send' }).click();

    const before = app.getByRole('region', {
      name: 'Tool call: ask_researcher',
    });
    await expand(before.getByRole('button', { name: /ask_researcher/ }));
    const beforeRun = before.getByRole('region', {
      name: 'Subagent run: Researcher',
    });
    // The run is now held open by a tool that does not return, so both of these
    // stay on screen for as long as this test takes.
    await expect(
      beforeRun.getByRole('region', { name: 'Tool call: list_dir' }),
    ).toBeVisible();
    await expect(beforeRun.getByText('I checked the folder.')).toBeVisible();

    await app.reload();

    // Nothing new can arrive — the subagent is still inside `e2e_wait` — so
    // everything below is the run as it was before the reload, replayed.
    const { card, header } = delegation(app);
    await expand(header);
    const run = card.getByRole('region', { name: 'Subagent run: Researcher' });
    await expect(
      run.getByRole('region', { name: 'Tool call: list_dir' }),
    ).toBeVisible();
    await expect(run.getByText('I checked the folder.')).toBeVisible();
  });
});

test.describe('the agent editor', () => {
  test('shows the delegation the config declares', async ({ app }) => {
    await app.getByRole('link', { name: 'Agents' }).click();
    await app.getByRole('link', { name: 'default' }).click();

    await expect(
      app.getByRole('combobox', { name: 'Agent for subagent 1' }),
    ).toHaveText('Researcher');
    // The derived name, which is what the model is really offered.
    await expect(app.getByText('ask_researcher')).toBeVisible();
  });
});
