/**
 * The context inspector.
 *
 * Two things are asserted that a screenshot could not: the bar is measured
 * against the window rather than against itself, and the same numbers are
 * available as text. The second is not a nicety — the bar is `aria-hidden`, so
 * if the table were wrong or absent the panel would be empty for a screen
 * reader and nobody looking at it would know.
 */

import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { ContextBody } from '@/context/context-inspector.js';
import { ContextStrip } from '@/context/context-strip.js';
import {
  renderWithProviders,
  stubApi,
  type StubRoute,
} from '@testkit/render.js';

const CONTEXT = {
  sessionKey: 'web:1',
  systemPrompt: 'You are GhostAI, a helpful agent.',
  tools: [
    {
      name: 'read_file',
      description: 'Read a file from the workspace.',
      parameters: { type: 'object', properties: { path: { type: 'string' } } },
      risk: 'safe',
      source: 'builtin',
    },
    {
      name: 'exec',
      description: 'Run a command.',
      parameters: { type: 'object', properties: { argv: { type: 'array' } } },
      risk: 'exec',
      source: 'builtin',
    },
  ],
  messages: [
    {
      id: 'm1',
      sessionKey: 'web:1',
      seq: 1,
      createdAtMs: 1,
      message: { role: 'user', content: [{ type: 'text', text: 'hello' }] },
    },
  ],
  runtimeBlock:
    '## Live state\n\nCurrent time: Tuesday, 14 November 2023 at 22:13 (UTC)',
  estimatedTokens: 5120,
  contextWindowTokens: 10_000,
  breakdown: {
    systemPrompt: 1000,
    tools: 1000,
    messages: 3000,
    runtimeBlock: 120,
  },
};

/**
 * The strip is the trigger now, so it is what the tests mount. The panel's own
 * assertions are unchanged — they run after opening it, which is the only thing
 * that moved.
 */
function mount(
  routes: Record<string, StubRoute> = {},
): ReturnType<typeof userEvent.setup> {
  stubApi({ '/api/sessions/web%3A1/context': [200, CONTEXT], ...routes });
  renderWithProviders(<ContextStrip sessionKey="web:1" />);
  return userEvent.setup();
}

/** Opens the panel from the strip under the composer. */
async function open(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.click(await screen.findByRole('button', { name: /of 10,000/ }));
}

describe('the context strip', () => {
  it('shows the budget without being opened', async () => {
    mount();

    // The whole point of moving it here: the number is readable at a glance,
    // rather than behind a button nobody presses.
    expect(
      await screen.findByRole('button', { name: /5,120 of 10,000 · 51%/ }),
    ).toBeInTheDocument();
  });

  it('renders nothing before a session exists', () => {
    stubApi({});
    const { container } = renderWithProviders(
      <ContextStrip sessionKey={undefined} />,
    );

    // A fresh tab holds a key the socket minted with no stored row behind it,
    // so the request would 404 — and an error under the composer of a
    // session that simply has not started answers a question nobody asked.
    expect(container.querySelector('.context-strip')).toBeNull();
  });

  it('renders nothing when the session has not started', async () => {
    stubApi({
      '/api/sessions/web%3A1/context': [404, { error: { code: 'not_found' } }],
    });
    const { container } = renderWithProviders(
      <ContextStrip sessionKey="web:1" />,
    );

    await waitFor(() => {
      expect(container.querySelector('.context-strip')).toBeNull();
    });
  });
});

describe('the context inspector', () => {
  it('measures the budget against the window and says it in words', async () => {
    const user = mount();
    await open(user);

    expect(await screen.findByText('5,120')).toBeInTheDocument();
    expect(screen.getByText(/of 10,000 tokens · 51%/)).toBeInTheDocument();
    expect(screen.getByText('4,880 free')).toBeInTheDocument();
    // The figure the two halves exist to move, beside the total rather than
    // buried in the table: what one more step of this turn costs.
    expect(screen.getByText('120 per step')).toBeInTheDocument();
  });

  it('breaks the total down by section, in a table and not only in a bar', async () => {
    const user = mount();
    await open(user);

    const table = await screen.findByRole('table', {
      name: 'Token usage by section',
    });
    const rows = [...table.querySelectorAll('tbody tr')].map(
      (row) => row.textContent,
    );

    // Request order, which is also cached-then-not: the trailing turn is last
    // because it is the only row a provider's cache cannot serve.
    expect(rows).toEqual([
      'System prompt1,00010.0%',
      'Tool definitions1,00010.0%',
      'Session3,00030.0%',
      'Live state1201.2%',
    ]);
  });

  it('shows the prompt that would actually be sent', async () => {
    const user = mount();
    await open(user);

    expect(
      await screen.findByText(/You are GhostAI, a helpful agent\./),
    ).toBeInTheDocument();
    // Was `1 messages in the window`. The count is one, and the sentence now
    // agrees with it — this line asserted the bug rather than the behaviour,
    // which is what an inflection hand-rolled at the call site buys you.
    expect(screen.getByText('1 message in the window')).toBeInTheDocument();
  });

  it('says a budget is over the window rather than rendering it as full', async () => {
    const user = mount({
      '/api/sessions/web%3A1/context': [
        200,
        { ...CONTEXT, estimatedTokens: 12_000 },
      ],
    });
    await user.click(
      await screen.findByRole('button', { name: /over the window/ }),
    );

    // The distinction the panel exists for: "exactly full" and "twice over" are
    // the same picture, and only one of them explains a dropped turn. The strip
    // says it too, so there are two of them once the dialog is open.
    expect(await screen.findAllByText('over the window')).not.toHaveLength(0);
    expect(screen.queryByText(/free/)).not.toBeInTheDocument();
  });

  /**
   * The next two mount the body rather than opening it from the strip, because
   * the strip hides itself on an error and so cannot reach either state. The
   * panel still handles both: it shares a query key with the strip, and a cache
   * eviction between the strip rendering and the dialog opening puts the fetch
   * back on the wire where it can fail again.
   */
  it('does not treat a session that has not started as a failure', async () => {
    // The socket mints a session key the moment a tab connects; the store holds
    // no row for it until the first message lands. A red error there answers a
    // question nobody asked.
    stubApi({
      '/api/sessions/web%3A1/context': [
        404,
        { error: { code: 'not_found', message: 'No session "web:1"' } },
      ],
    });
    renderWithProviders(<ContextBody sessionKey="web:1" />);

    expect(
      await screen.findByText(/this session has not started/i),
    ).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('reports a real failure instead of an empty panel', async () => {
    stubApi({
      '/api/sessions/web%3A1/context': [
        500,
        {
          error: { code: 'internal', message: 'the prompt could not be built' },
        },
      ],
    });
    renderWithProviders(<ContextBody sessionKey="web:1" />);

    await waitFor(() => {
      expect(screen.getByRole('alert')).toHaveTextContent(
        'the prompt could not be built',
      );
    });
  });
});

/**
 * The three sections, openable.
 *
 * The table reports "tools: 1,000" and the only follow-up question anyone has is
 * *which* tools — which used to be unanswerable from the panel that raised it,
 * because the client had no copy of the definitions at all.
 */
describe('the context inspector: what is in each section', () => {
  it('opens the system prompt', async () => {
    const user = mount();
    await open(user);

    // By `summary`, because "System prompt" is also the label of its row in the
    // table above — the two are deliberately the same word.
    await user.click(
      await screen.findByText('System prompt', { selector: 'summary' }),
    );
    expect(screen.getByText('You are GhostAI, a helpful agent.')).toBeVisible();
  });

  it('opens the live state, which is the part every step of a turn pays for again', async () => {
    // The section this panel exists to make legible: everything above it is the
    // provider's cached prefix, and this is the tail re-read at full price.
    const user = mount();
    await open(user);

    await user.click(
      await screen.findByText('Live state', { selector: 'summary' }),
    );
    expect(
      screen.getByText(
        /Current time: Tuesday, 14 November 2023 at 22:13 \(UTC\)/,
      ),
    ).toBeVisible();
  });

  it('opens the tool definitions, with each schema behind its own disclosure', async () => {
    const user = mount();
    await open(user);

    await user.click(await screen.findByText('Tool definitions (2)'));

    expect(screen.getByText('read_file')).toBeVisible();
    expect(screen.getByText('Read a file from the workspace.')).toBeVisible();
    // The risk band, because it is the field that decides whether a call needs
    // approving and it is the one most worth seeing beside the name.
    expect(screen.getByText('exec', { selector: '.badge' })).toBeVisible();

    // The schema is nested one level deeper: it is the reason to open this at all
    // when a budget is unexpectedly large, and it is far too long to show inline.
    // Asserted on *visibility*, not presence — a closed `<details>` keeps its
    // children in the DOM, so `queryByText` finds them either way.
    const [schema] = screen.getAllByText(/"properties"/);
    expect(schema).not.toBeVisible();
    await user.click(screen.getByText('read_file schema'));
    expect(schema).toBeVisible();
  });

  it('opens the session, addressed by the seq the rest of the UI uses', async () => {
    const user = mount();
    await open(user);

    await user.click(await screen.findByText('Session (1 message)'));

    expect(screen.getByText('hello')).toBeVisible();
    expect(screen.getByText('#1')).toBeVisible();
  });

  it('shows the answer without the thinking behind it', async () => {
    // The route does not send `reasoning` — this asserts the panel would not
    // render one if a stale client or an older server did. What the screen
    // claims is the request, and the request has never carried it; the
    // transcript beside it still shows the collapsible block.
    const user = mount({
      '/api/sessions/web%3A1/context': [
        200,
        {
          ...CONTEXT,
          messages: [
            ...CONTEXT.messages,
            {
              id: 'm2',
              sessionKey: 'web:1',
              seq: 2,
              createdAtMs: 2,
              message: {
                role: 'assistant',
                content: [{ type: 'text', text: 'the answer' }],
                toolCalls: [],
                reasoning: 'first I considered the alternatives',
              },
            },
          ],
        },
      ],
    });
    await open(user);

    await user.click(await screen.findByText('Session (2 messages)'));
    expect(screen.getByText('the answer')).toBeVisible();
    expect(
      screen.queryByText(/first I considered the alternatives/),
    ).not.toBeInTheDocument();
  });

  it('says so rather than showing an empty box for an agent with no tools', async () => {
    const user = mount({
      '/api/sessions/web%3A1/context': [200, { ...CONTEXT, tools: [] }],
    });
    await open(user);

    await user.click(await screen.findByText('Tool definitions (0)'));
    expect(screen.getByText('This agent has no tools.')).toBeVisible();
  });
});
