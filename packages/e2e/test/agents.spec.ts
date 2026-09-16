/**
 * Multiple agents, in a browser, against the real stack.
 *
 * The unit suites already prove the pieces: the resolver inherits field by
 * field, the loop cache builds one loop per agent, the tool scope hides a
 * denied tool, the hub picks a loop per turn. What only a browser can show is
 * that they are wired to each other — that an agent created in the settings
 * panel becomes an agent the API will actually run a turn on, with the tools
 * and the prompt that agent was given rather than the default's.
 *
 * Every assertion here is on **durable** state: a settings tree that came back
 * from the server, a context response, a stored session row. Nothing waits on a
 * line that exists only between two frames — that is what put `approvals.spec`
 * red in CI four runs while green on a laptop every time.
 */

import { expect, test } from '../src/fixtures.js';

test.describe('agents', () => {
  test('the picker is in the composer, where the question is asked', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: { label: 'Reviewer', provider: 'ollama', model: 'qwen3' },
          },
        },
      },
    });

    await app.goto(`${harness.url}/`);

    // Beside the composer's hint, not three columns away in the sidebar.
    await app.getByRole('button', { name: /^Agent: / }).click();
    await expect(
      app.getByRole('menuitemradio', { name: /Reviewer/ }),
    ).toBeVisible();

    await app.getByRole('menuitem', { name: 'Manage agents…' }).click();
    await expect(app.getByRole('heading', { name: 'Agents' })).toBeVisible();
  });

  test('picking one before the first message binds the session to it', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: { label: 'Reviewer', provider: 'ollama', model: 'qwen3' },
          },
        },
      },
    });

    await app.goto(`${harness.url}/`);
    await app.getByRole('button', { name: /^Agent: / }).click();
    await app.getByRole('menuitemradio', { name: /Reviewer/ }).click();

    await app.getByRole('textbox', { name: /message/i }).fill('hello');
    await app.getByRole('button', { name: 'Send' }).click();

    // The durable result: the stored row names the agent that was picked.
    await expect
      .poll(async () => {
        const response = await app.request.get(`${harness.url}/api/sessions`);
        const body = (await response.json()) as {
          sessions: Array<{ agentId?: string }>;
        };
        return body.sessions[0]?.agentId;
      })
      .toBe('reviewer');
  });

  test('the default agent is listed and cannot be deleted', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: { label: 'Reviewer', provider: 'ollama', model: 'qwen3' },
          },
        },
      },
    });

    await app.goto(`${harness.url}/agents`);

    // It is there even though nothing wrote it down: every unbound session
    // runs on it, and a list that only showed the written-down ones would hide
    // the one actually in use.
    await expect(app.getByRole('link', { name: 'Edit default' })).toBeVisible();

    await app.getByRole('link', { name: 'Edit default' }).click();
    await expect(app.getByLabel('Max output tokens')).toBeVisible();
    // No row menu at all on the default: the only thing in it would be a
    // delete this agent cannot have.
    await expect(app.getByRole('button', { name: /^Actions for/ })).toHaveCount(
      0,
    );

    // A named agent does offer one.
    await app.goto(`${harness.url}/agents/reviewer`);
    await app.getByRole('button', { name: 'Actions for Reviewer' }).click();
    await expect(
      app.getByRole('menuitem', { name: 'Delete this agent' }),
    ).toBeVisible();
  });

  test('the model and budget live on the default agent, not in Settings', async ({
    app,
    harness,
  }) => {
    await app.goto(`${harness.url}/settings`);
    // The Agent panel is gone: what it edited *is* the default agent's.
    await expect(app.getByRole('tab', { name: 'Agent' })).toHaveCount(0);

    await app.goto(`${harness.url}/agents/default`);
    const maxTokens = app.getByLabel('Max output tokens');
    await maxTokens.fill('2048');
    await app.getByRole('button', { name: 'Save changes' }).click();

    await expect
      .poll(async () => {
        const response = await app.request.get(`${harness.url}/api/settings`);
        const body = (await response.json()) as {
          config: { agents: { list: { default: { maxTokens: number } } } };
        };
        return body.config.agents.list.default.maxTokens;
      })
      .toBe(2048);
  });

  test('the memory switch denies the tool that gates the section', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: {
              label: 'Reviewer',
              provider: 'ollama',
              model: 'qwen3',
              tools: { memory: 'allow', skill: 'allow' },
            },
          },
        },
      },
    });

    await app.goto(`${harness.url}/agents/reviewer`);
    await app.getByLabel('Remember across sessions').click();
    await app.getByRole('button', { name: 'Save changes' }).click();

    // The permission, read back from the API — the switch and the Tools row are
    // one value, and this is the one that decides whether the section is placed.
    await expect
      .poll(async () => {
        const response = await app.request.get(`${harness.url}/api/settings`);
        const body = (await response.json()) as {
          config: {
            agents: {
              list: Record<
                string,
                { tools?: Record<string, string> } | undefined
              >;
            };
          };
        };
        return body.config.agents.list.reviewer?.tools?.memory;
      })
      .toBe('deny');
  });

  test('a memory prompt saves onto the agent that wrote it', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            scribe: { label: 'Scribe', provider: 'ollama', model: 'qwen3' },
          },
        },
      },
    });

    await app.goto(`${harness.url}/agents/scribe`);
    await app.getByText('Advanced prompt settings').click();
    const box = app.getByLabel('Memory for Scribe');
    await box.fill('## What I know\n\n{{index}}');
    await app.getByRole('button', { name: 'Save changes' }).click();

    await expect
      .poll(async () => {
        const response = await app.request.get(`${harness.url}/api/settings`);
        const body = (await response.json()) as {
          config: {
            agents: { list: Record<string, { memoryPrompt?: string }> };
          };
        };
        return body.config.agents.list.scribe?.memoryPrompt;
      })
      .toContain('## What I know');
  });

  test('a new agent is created holding the default’s settings', async ({
    app,
    harness,
  }) => {
    // Give the default agent something worth copying. One entry holds all of
    // it — the model, the budget, the prompt and the tools — and the patch
    // carries all of it, because `agents.list.*` replaces wholesale.
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            default: {
              provider: 'ollama',
              model: 'qwen3',
              maxTokens: 1234,
              systemPrompt: 'House style: be terse.',
              tools: { read_file: 'allow', exec: 'deny' },
            },
          },
        },
      },
    });

    await app.goto(`${harness.url}/agents`);
    await app.getByRole('link', { name: 'New agent' }).click();
    await app.getByLabel('Name', { exact: true }).fill('Reviewer');
    await app.getByRole('button', { name: 'Save changes' }).click();

    // Nothing inherits any more, so what a new agent runs on has to be written
    // into its own entry — otherwise the editor would have to describe this
    // agent's model as somebody else's.
    await expect
      .poll(async () => {
        const response = await app.request.get(`${harness.url}/api/settings`);
        const body = (await response.json()) as {
          config: {
            agents: {
              list: Record<
                string,
                {
                  systemPrompt: string;
                  tools: Record<string, string>;
                  maxTokens?: number;
                }
              >;
            };
          };
        };
        return body.config.agents.list.reviewer;
      })
      .toMatchObject({
        systemPrompt: 'House style: be terse.',
        tools: { read_file: 'allow', exec: 'deny' },
        maxTokens: 1234,
      });
  });

  test('a rename in the editor reaches the composer’s picker', async ({
    app,
    harness,
  }) => {
    // The reported bug, end to end. `/api/agents` is what the picker renders and
    // it is derived from the settings tree rather than part of it, so a save
    // that renamed an agent left the picker on the old name until a reload.
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: { label: 'Reviewer', provider: 'ollama', model: 'qwen3' },
          },
        },
      },
    });

    await app.goto(`${harness.url}/agents/reviewer`);
    await app.getByLabel('Name', { exact: true }).fill('Second Reader');
    await app.getByRole('button', { name: 'Save changes' }).click();

    // Durable state on both sides: the stored label, and the name the picker
    // renders on a screen that was never told about the edit.
    await expect
      .poll(async () => {
        const response = await app.request.get(`${harness.url}/api/agents`);
        const body = (await response.json()) as {
          agents: Array<{ id: string; label: string }>;
        };
        return body.agents.find((agent) => agent.id === 'reviewer')?.label;
      })
      .toBe('Second Reader');

    await app.getByRole('link', { name: 'Agents' }).first().click();
    await expect(
      app.getByRole('link', { name: 'Edit Second Reader' }),
    ).toBeVisible();
  });

  test('an agent switched off from the list stops being one a turn can run on', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: {
              label: 'Reviewer',
              provider: 'ollama',
              model: 'qwen3',
              tools: { read_file: 'allow', exec: 'deny' },
            },
          },
        },
      },
    });

    await app.goto(`${harness.url}/agents`);
    await app.getByRole('button', { name: 'Actions for Reviewer' }).click();
    await app.getByRole('menuitem', { name: 'Disable' }).click();

    // The durable result, in two places: the stored entry keeps everything it
    // held and is merely off, and the list of runnable agents no longer has it.
    await expect
      .poll(async () => {
        const response = await app.request.get(`${harness.url}/api/settings`);
        const body = (await response.json()) as {
          config: {
            agents: {
              list: Record<
                string,
                { enabled: boolean; tools: Record<string, string> }
              >;
            };
          };
        };
        return body.config.agents.list.reviewer;
      })
      .toMatchObject({
        enabled: false,
        tools: { read_file: 'allow', exec: 'deny' },
      });

    const agents = await app.request.get(`${harness.url}/api/agents`);
    const listed = (await agents.json()) as { agents: Array<{ id: string }> };
    expect(listed.agents.map((agent) => agent.id)).toEqual(['default']);

    // And back on from the same menu, with what it held still there.
    await app.getByRole('button', { name: 'Actions for Reviewer' }).click();
    await app.getByRole('menuitem', { name: 'Enable' }).click();

    await expect
      .poll(async () => {
        const response = await app.request.get(`${harness.url}/api/agents`);
        const body = (await response.json()) as {
          agents: Array<{ id: string }>;
        };
        return body.agents.map((agent) => agent.id);
      })
      .toEqual(['default', 'reviewer']);
  });

  test('an agent created on the page is one the server will run a turn on', async ({
    app,
    harness,
  }) => {
    await app.goto(`${harness.url}/agents`);

    await app.getByRole('link', { name: 'New agent' }).click();
    await app.getByLabel('Name', { exact: true }).fill('Code Reviewer');
    await app.getByRole('button', { name: 'Save changes' }).click();

    // The durable result: the settings tree the server sends back names it.
    await expect
      .poll(async () => {
        const response = await app.request.get(`${harness.url}/api/settings`);
        const body = (await response.json()) as {
          config: { agents: { list: Record<string, { label: string }> } };
        };
        return body.config.agents.list['code-reviewer']?.label;
      })
      .toBe('Code Reviewer');

    // And it is offered as something a turn can run on.
    const agents = await app.request.get(`${harness.url}/api/agents`);
    const listed = (await agents.json()) as {
      agents: Array<{ id: string; label: string }>;
    };
    expect(listed.agents.map((agent) => agent.id)).toEqual([
      'default',
      'code-reviewer',
    ]);
  });

  test('a session bound to an agent carries that agent’s whole prompt', async ({
    app,
    harness,
  }) => {
    // Configured through the settings route the screen uses, so the spec proves
    // the same path a browser takes rather than a seeded fixture.
    const saved = await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: {
              label: 'Reviewer',
              provider: 'ollama',
              model: 'qwen3',
              systemPrompt: '# {{name}}\n\nOnly ever read. Never write.',
              tools: { read_file: 'allow', exec: 'deny' },
            },
          },
        },
      },
    });
    expect(saved.ok(), 'saving the agent should succeed').toBe(true);

    const created = await app.request.post(`${harness.url}/api/sessions`, {
      data: { key: 'web-reviewer', agentId: 'reviewer' },
    });
    expect(created.ok(), 'creating the session should succeed').toBe(true);

    // The context inspector reports what a turn on this session would carry —
    // assembled by that agent's own loop, not by a second implementation.
    const context = await app.request.get(
      `${harness.url}/api/sessions/web-reviewer/context`,
    );
    const body = (await context.json()) as { systemPrompt: string };

    // The placeholder is filled from the agent's label, at turn time, because
    // the same stored template runs in every workspace and on every platform.
    expect(body.systemPrompt).toContain('# Reviewer');
    expect(body.systemPrompt).toContain('Only ever read. Never write.');
    // And nothing else. A stored prompt *is* the static half now rather than an
    // `## Instructions` section appended below a fixed one — which is the whole
    // of "the system prompt belongs to the agent".
    expect(body.systemPrompt).not.toContain('That directory is your root');
    expect(body.systemPrompt).not.toContain('## Guidelines');
  });

  /**
   * The one bug the phone sweep in `a11y.spec` cannot see.
   *
   * That sweep asks whether anything overflows its column, and an unplaced grid
   * item overflows nothing: the wording pencil simply fell into an implicit row
   * of its own, centred under the permission select, reading as a control that
   * belonged to no tool. Nothing scrolled and nothing escaped.
   *
   * Geometry after reflow rather than a class name, because the assertion is
   * about what an operator sees and the class is only how it was arranged. And
   * geometry is durable here — the row has settled by the time the select is
   * visible, so this is not a state that exists between two frames.
   */
  test('the wording pencil stays on the tool row on a phone', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: { label: 'Reviewer', provider: 'ollama', model: 'qwen3' },
          },
        },
      },
    });

    await app.goto(`${harness.url}/agents/reviewer`);
    await expect(
      app.getByRole('complementary', { name: 'Sidebar' }),
    ).toBeVisible();
    await app.setViewportSize({ width: 375, height: 800 });
    await expect(app.getByRole('button', { name: 'Open menu' })).toBeVisible();

    const pencil = app.getByRole('button', { name: 'Wording for read_file' });
    const permission = app.getByRole('combobox', {
      name: 'Permission for read_file',
    });
    await expect(pencil).toBeVisible();
    await expect(permission).toBeVisible();

    const above = await pencil.boundingBox();
    const below = await permission.boundingBox();
    if (above === null || below === null) {
      throw new Error('both controls should have a box');
    }

    // On the badge's line, above the full-width select — not on a line of its own
    // below it, which is where an unplaced grid item lands.
    expect(above.y).toBeLessThan(below.y);
  });

  /**
   * A focus ring is painted *outside* the border box — `--ring-width` plus
   * `--ring-offset` beyond it — and a scroll container clips at its padding
   * edge. So a scrollport with no padding eats the ring of everything it holds.
   *
   * `.dialog` is already a scrollport and already has `--space-6` of padding,
   * which is room to spare. What broke this was a *second* scroller nested
   * inside it with none: a focused textarea kept the top and bottom of its ring
   * and lost the left and right, which reads as a green underline rather than as
   * focus. It also gave one dialog two scrollbars.
   *
   * Asserted as the property rather than as "this element has no overflow",
   * because the property is what an operator sees and the overflow is only one
   * way to break it.
   */
  test('a focused control in a scrolling dialog keeps its whole focus ring', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: { label: 'Reviewer', provider: 'ollama', model: 'qwen3' },
          },
        },
      },
    });

    await app.goto(`${harness.url}/agents/reviewer`);
    // `exec` carries the longest built-in wording of the five, which is what
    // makes this dialog tall enough to scroll at all.
    await app.getByRole('button', { name: 'Wording for exec' }).click();

    const description = app.getByLabel('Description');
    await description.click();
    await expect(description).toBeFocused();

    const clippedBy = await description.evaluate((element) => {
      // `--ring-width` + `--ring-offset`, both 0.125rem, at the default root size.
      const ring = 4;
      const rect = element.getBoundingClientRect();
      for (
        let node = element.parentElement;
        node !== null;
        node = node.parentElement
      ) {
        const style = getComputedStyle(node);
        if (!/auto|scroll|hidden/.test(style.overflowX + style.overflowY)) {
          continue;
        }
        // Only the nearest clipping ancestor can crop the ring; anything above it
        // is clipping a box that already fits.
        const port = node.getBoundingClientRect();
        const cropped =
          rect.left - ring < port.left || rect.right + ring > port.right;
        return cropped ? node.className : null;
      }
      return null;
    });

    expect(
      clippedBy,
      'nothing should crop the ring of a focused control',
    ).toBeNull();
  });

  test('an agent that stores no prompt still gets the built-in one', async ({
    app,
    harness,
  }) => {
    // The other half of the contract: empty means "the built-in", so an install
    // that never customised a prompt keeps receiving improvements to it.
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            plain: { label: 'Plain', provider: 'ollama', model: 'qwen3' },
          },
        },
      },
    });
    await app.request.post(`${harness.url}/api/sessions`, {
      data: { key: 'web-plain-prompt', agentId: 'plain' },
    });

    const context = await app.request.get(
      `${harness.url}/api/sessions/web-plain-prompt/context`,
    );
    const body = (await context.json()) as { systemPrompt: string };

    expect(body.systemPrompt).toContain('# Plain');
    expect(body.systemPrompt).toContain('It is the only place you');
    expect(body.systemPrompt).toContain('## Guidelines');
  });

  test('a denied tool is not offered to the agent that denied it', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reader: {
              label: 'Reader',
              provider: 'ollama',
              model: 'qwen3',
              tools: { read_file: 'allow' },
            },
          },
        },
      },
    });
    await app.request.post(`${harness.url}/api/sessions`, {
      data: { key: 'web-reader', agentId: 'reader' },
    });
    await app.request.post(`${harness.url}/api/sessions`, {
      data: { key: 'web-plain' },
    });

    /** What the tool definitions cost this session, as the inspector measures it. */
    const toolBudget = async (key: string): Promise<number> => {
      const response = await app.request.get(
        `${harness.url}/api/sessions/${key}/context`,
      );
      expect(response.ok(), `${key} should have a context`).toBe(true);
      const body = (await response.json()) as {
        breakdown: Record<string, number>;
      };
      return body.breakdown.tools ?? 0;
    };

    const restricted = await toolBudget('web-reader');
    const unrestricted = await toolBudget('web-plain');

    // The measurement is the assertion: the inspector is fed the *agent's*
    // tool scope, so an agent holding one tool cannot cost the same as one
    // holding five. A context panel that reported the registry's list would
    // describe tools this agent can never call.
    expect(restricted).toBeGreaterThan(0);
    expect(restricted).toBeLessThan(unrestricted);

    // And the registry itself is untouched — the denial is a view, not a removal.
    const tools = await app.request.get(`${harness.url}/api/tools`);
    const names = (
      (await tools.json()) as { tools: Array<{ name: string }> }
    ).tools.map((t) => t.name);
    expect(names).toContain('exec');
  });

  test('the picker survives a reload, because the binding is on the session row', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            writer: { label: 'Writer', provider: 'ollama', model: 'qwen3' },
          },
        },
      },
    });
    await app.request.post(`${harness.url}/api/sessions`, {
      data: { key: 'web-bound', agentId: 'writer' },
    });

    const read = async (): Promise<string | undefined> => {
      const response = await app.request.get(
        `${harness.url}/api/sessions/web-bound`,
      );
      return ((await response.json()) as { agentId?: string }).agentId;
    };

    expect(await read()).toBe('writer');

    // Moving it is an explicit update, not something a frame can do.
    const moved = await app.request.patch(
      `${harness.url}/api/sessions/web-bound`,
      {
        data: { agentId: 'default' },
      },
    );
    expect(moved.ok()).toBe(true);
    expect(await read()).toBe('default');
  });

  test('a session keeps working after the agent it names is deleted', async ({
    app,
    harness,
  }) => {
    // The whole point of the fallback. An agent id is user-authored and lives
    // in a file the operator edits, so it can go at any moment — and a
    // session must not become a thing that cannot take another turn.
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: { label: 'Reviewer', provider: 'ollama', model: 'qwen3' },
          },
        },
      },
    });
    await app.request.post(`${harness.url}/api/sessions`, {
      data: { key: 'web-orphan', agentId: 'reviewer' },
    });

    const deleted = await app.request.patch(`${harness.url}/api/settings`, {
      data: { agents: { list: { reviewer: null } } },
    });
    expect(deleted.ok()).toBe(true);

    await app.goto(`${harness.url}/?session=web-orphan`);
    await app
      .getByRole('textbox', { name: 'Message' })
      .fill('stream a long answer');
    await app.getByRole('button', { name: 'Send' }).click();

    // Settled state only: the answer arrived and the composer is offering Send
    // again. Nothing here waits on a line that exists between two frames.
    const transcript = app.getByTestId('transcript');
    await expect(transcript.getByText('Here is what I found.')).toBeVisible();
    await expect(app.getByRole('button', { name: 'Send' })).toBeVisible();

    // The picker says what happened, and this *is* durable: it is read off the
    // session row and the agent listing, both of which survive a reload. The
    // `agent_fallback` notice is not — it is a live frame and nothing persists
    // it — so it is asserted where the state can be held still, in
    // `chat.test.tsx`, rather than raced for here.
    await expect(
      app.getByRole('button', { name: /reviewer — no longer configured/ }),
    ).toBeVisible();

    // And the binding is untouched, which is what lets re-creating the agent
    // restore this session with no action taken on it.
    const stored = await app.request.get(
      `${harness.url}/api/sessions/web-orphan`,
    );
    expect(((await stored.json()) as { agentId?: string }).agentId).toBe(
      'reviewer',
    );
  });

  test('deleting an agent another one delegates to succeeds', async ({
    app,
    harness,
  }) => {
    // Used to be a 500 that changed nothing: the rebuild threw over the
    // now-dangling delegation, and the rebuild happens before the write.
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: { label: 'Reviewer', provider: 'ollama', model: 'qwen3' },
            planner: {
              label: 'Planner',
              provider: 'ollama',
              model: 'qwen3',
              subagents: [
                { id: 'reviewer', prompt: 'Check it.', permission: 'allow' },
              ],
            },
          },
        },
      },
    });

    const deleted = await app.request.patch(`${harness.url}/api/settings`, {
      data: { agents: { list: { reviewer: null } } },
    });
    expect(deleted.ok()).toBe(true);

    // The settled tree: the agent is gone and so is the delegation to it.
    const settings = await app.request.get(`${harness.url}/api/settings`);
    const config = (await settings.json()) as {
      config: {
        agents: { list: Record<string, { subagents: Array<{ id: string }> }> };
      };
    };
    expect(config.config.agents.list.reviewer).toBeUndefined();
    expect(config.config.agents.list.planner?.subagents).toEqual([]);

    await app.goto(`${harness.url}/agents`);
    await expect(app.getByRole('link', { name: 'Edit Reviewer' })).toHaveCount(
      0,
    );
  });

  test('renaming an agent takes its sessions and delegations with it', async ({
    app,
    harness,
  }) => {
    await app.request.patch(`${harness.url}/api/settings`, {
      data: {
        agents: {
          list: {
            reviewer: { label: 'Reviewer', provider: 'ollama', model: 'qwen3' },
            planner: {
              label: 'Planner',
              provider: 'ollama',
              model: 'qwen3',
              subagents: [
                { id: 'reviewer', prompt: 'Check it.', permission: 'allow' },
              ],
            },
          },
        },
      },
    });
    await app.request.post(`${harness.url}/api/sessions`, {
      data: { key: 'web-renamed', agentId: 'reviewer' },
    });

    await app.goto(`${harness.url}/agents/reviewer`);
    const id = app.getByLabel('Identifier');
    await id.fill('code-review');
    // The screen's one Save, the same as every other box on it.
    await app.getByRole('button', { name: 'Save changes' }).click();

    // The settled destination: the editor is now on the new id.
    await expect(app).toHaveURL(new RegExp('/agents/code-review$'));

    const settings = await app.request.get(`${harness.url}/api/settings`);
    const config = (await settings.json()) as {
      config: {
        agents: { list: Record<string, { subagents: Array<{ id: string }> }> };
      };
    };
    expect(config.config.agents.list.reviewer).toBeUndefined();
    expect(config.config.agents.list['code-review']).toBeDefined();
    expect(config.config.agents.list.planner?.subagents[0]?.id).toBe(
      'code-review',
    );

    // The session followed, so it is not left on the fallback path for an
    // agent that never went anywhere.
    const stored = await app.request.get(
      `${harness.url}/api/sessions/web-renamed`,
    );
    expect(((await stored.json()) as { agentId?: string }).agentId).toBe(
      'code-review',
    );
  });

  test('a config naming a delegation to nothing still boots, and says so', async ({
    app,
    harness,
  }) => {
    // Written the way a hand edit would leave it — the settings route prunes a
    // dangling ref on the way in, so this goes through the file the reload
    // re-reads rather than through a patch.
    harness.writeConfig({
      agents: {
        list: {
          planner: {
            subagents: [{ id: 'ghost', prompt: '', permission: 'allow' }],
          },
        },
      },
    });
    const reloaded = await app.request.post(
      `${harness.url}/api/settings/reload`,
    );
    expect(reloaded.ok()).toBe(true);

    await app.goto(`${harness.url}/settings`);
    await expect(
      app.getByRole('alert').filter({ hasText: /does not exist/ }),
    ).toBeVisible();
  });
});
