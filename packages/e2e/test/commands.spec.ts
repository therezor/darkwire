/**
 * Slash commands in the composer, through the real server.
 *
 * Four things a component test cannot see, which is why these four and not
 * the whole table. The completion list has to open against the *installed*
 * agents rather than a stubbed listing. `/rename` has to reach
 * `PATCH /api/sessions/:key` and come back far enough for the sidebar to agree.
 * A message that merely looks like a command has to survive the whole journey
 * to the model as prose. And `/model` has to move the agent the *session* is
 * bound to — which needs a real settings tree, a real binding and a real turn,
 * because the bug it fixes was invisible to every one of those in isolation.
 *
 * **Every assertion here is on a durable state.** The toast a command raises is
 * the textbook transient — it is on screen for a few seconds and whether a run
 * catches it depends on how the machine was feeling — so what is asserted is
 * the title in the sidebar, the list in the box, the answer in the transcript.
 * The wording of the toasts is covered where it can be held still, in
 * `packages/web/test/chat/commands.test.ts`.
 */

import { expect, test } from '../src/fixtures.js';

test.describe('slash commands', () => {
  test('opens the list on a slash and completes what is typed', async ({
    app,
  }) => {
    const message = app.getByRole('textbox', { name: 'Message' });

    await message.fill('/');
    const list = app.getByRole('listbox', { name: 'Commands' });
    await expect(list.getByRole('option').first()).toContainText('/new');

    // Narrowing is the parser's, not a filter over what is already rendered.
    await message.fill('/ren');
    await expect(list.getByRole('option')).toHaveCount(1);
    await expect(list.getByRole('option')).toContainText('/rename');

    // `/rename` still needs an argument, so Enter accepts rather than sending
    // and leaves the cursor after the space.
    await message.press('Enter');
    await expect(message).toHaveValue('/rename ');

    // A command that needs nothing closes the list as soon as it is complete,
    // so Enter sends it. One keypress, as it is in the terminal and the bot.
    await message.fill('/stop');
    await expect(app.getByRole('listbox', { name: 'Commands' })).toHaveCount(0);
  });

  test('renames the session, and the sidebar agrees', async ({ app }) => {
    const sidebar = app.getByRole('complementary', { name: 'Sidebar' });
    const message = app.getByRole('textbox', { name: 'Message' });

    await message.fill('stream a long answer');
    await message.press('Enter');
    // The session has to exist before it can be renamed — nothing is stored
    // until the first turn lands, which is what `/rename` refuses on.
    await expect(sidebar.getByText('stream a long answer')).toBeVisible({
      timeout: 15_000,
    });

    await message.fill('/rename Renamed from the composer');
    await message.press('Enter');

    // The durable half: the title on the row, which came back from the PATCH.
    await expect(sidebar.getByText('Renamed from the composer')).toBeVisible({
      timeout: 15_000,
    });
    await expect(message).toHaveValue('');
  });

  test('sends a path as the sentence it is', async ({ app }) => {
    const message = app.getByRole('textbox', { name: 'Message' });

    // The trap the parser exists for. A second slash in the first word is what
    // keeps every path out of the dispatcher.
    await message.fill('/usr/bin/env is on the path');
    await expect(app.getByRole('listbox', { name: 'Commands' })).toHaveCount(0);

    await message.press('Enter');

    // It reached the transcript as a message, which is only true if nothing
    // claimed it on the way.
    await expect(
      app.getByText('/usr/bin/env is on the path', { exact: false }).first(),
    ).toBeVisible({ timeout: 15_000 });
  });
});

test.describe('/model on a session bound to an agent of its own', () => {
  /**
   * A session on an agent of its own, which is the case that matters.
   *
   * A model lives on an agent and nowhere else, so `/model` has to move the one
   * the conversation is bound to rather than any other. Two models have to be
   * reachable for the command to have somewhere to go, and `providers.<id>.models`
   * is what puts one in the catalogue without an endpoint to ask.
   */
  async function installCoder(
    request: {
      patch: (url: string, init: { data: unknown }) => Promise<unknown>;
    },
    url: string,
  ): Promise<void> {
    await request.patch(`${url}/api/settings`, {
      data: {
        providers: { ollama: { type: 'ollama', models: ['qwen3', 'gpt-oss'] } },
        agents: {
          list: {
            coder: {
              label: 'Coder',
              provider: 'ollama',
              model: 'llama3',
              systemPrompt: 'You write code.',
              tools: { read_file: 'allow' },
            },
          },
        },
      },
    });
  }

  test('moves that agent, leaves the install default alone, and the next turn follows', async ({
    app,
    harness,
  }) => {
    await installCoder(app.request, harness.url);

    await app.goto(`${harness.url}/`);
    await app.getByRole('button', { name: /^Agent: / }).click();
    await app.getByRole('menuitemradio', { name: /Coder/ }).click();

    // The catalogue has to offer it before the command can be about anything:
    // `/model` refuses an id no endpoint published, and it needs the lookup
    // anyway for the `providerId` half of the pair it writes.
    const catalogue = await app.request.get(`${harness.url}/api/models`);
    const offered = (await catalogue.json()) as {
      models: Array<{ id: string }>;
    };
    expect(offered.models.map((model) => model.id)).toContain('gpt-oss');

    const message = app.getByRole('textbox', { name: 'Message' });
    await message.fill('/model gpt-oss');
    // Escape first. `/model` completes its argument, so the value list is open
    // and Enter would accept a row rather than send the line — the same reason
    // the `/rename` case above asserts that Enter accepts.
    await message.press('Escape');
    await message.press('Enter');

    // The durable half of the write, and the assertion the whole change exists
    // for: the entry moved, it kept everything else it held — `agents.list.*`
    // replaces wholesale, so a patch naming `model` alone would have taken the
    // label, the prompt and the tools with it — and the default agent did not
    // follow.
    await expect
      .poll(
        async () => {
          const response = await app.request.get(`${harness.url}/api/settings`);
          const body = (await response.json()) as {
            config: {
              agents: {
                defaults: { model: string };
                list: Record<string, { model?: string }>;
              };
            };
          };
          return {
            coder: body.config.agents.list.coder?.model,
            defaults: body.config.agents.list.default?.model,
          };
        },
        { timeout: 15_000 },
      )
      .toEqual({ coder: 'gpt-oss', defaults: 'qwen3' });

    const settings = await app.request.get(`${harness.url}/api/settings`);
    const config = (await settings.json()) as {
      config: {
        agents: {
          list: Record<
            string,
            {
              label: string;
              systemPrompt: string;
              tools: Record<string, string>;
            }
          >;
        };
      };
    };
    expect(config.config.agents.list.coder).toMatchObject({
      label: 'Coder',
      systemPrompt: 'You write code.',
      tools: { read_file: 'allow' },
    });

    // And the turn that follows runs on it. `turn.start` carries the model the
    // loop was built with, so this is the model the request actually named
    // rather than a label read back off the settings tree.
    await message.fill('stream a long answer');
    await app.getByRole('button', { name: 'Send' }).click();
    await expect(
      app.getByTestId('transcript').getByText('Here is what I found.'),
    ).toBeVisible({ timeout: 15_000 });

    await app.getByRole('button', { name: 'Turn details' }).click();
    await expect(app.getByRole('dialog').getByText('gpt-oss')).toBeVisible();
  });
});
